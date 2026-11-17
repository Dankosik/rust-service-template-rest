use axum::body::{Body, Bytes};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::{BufMut as _, BytesMut};
use infra_idempotency_store::{CaptureError, MAX_STORED_BODY_BYTES, Record};
use serde::Serialize;

use super::execute::Rollback;

pub(super) use infra_idempotency_store::is_replayable_header_name;

#[derive(Debug)]
pub struct ReplayableResponse(Record);

impl ReplayableResponse {
    pub fn bytes(status: StatusCode, headers: &HeaderMap, body: Bytes) -> Result<Self, Rollback> {
        Record::capture(status, headers, body)
            .map(Self)
            .map_err(|error| match error {
                CaptureError::InvalidStatus => Rollback::InvalidStatus,
                CaptureError::BodyLimit => Rollback::BodyLimit,
                CaptureError::HeaderLimit => Rollback::HeaderLimit,
            })
    }

    pub fn json<T: Serialize + ?Sized>(
        status: StatusCode,
        mut headers: HeaderMap,
        value: &T,
    ) -> Result<Self, Rollback> {
        let mut body = BytesMut::new();
        let mut writer = (&mut body).limit(MAX_STORED_BODY_BYTES).writer();
        serde_json::to_writer(&mut writer, value).map_err(|error| {
            if error.io_error_kind() == Some(std::io::ErrorKind::WriteZero) {
                Rollback::BodyLimit
            } else {
                Rollback::Serialization
            }
        })?;
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Self::bytes(status, &headers, body.freeze())
    }

    pub fn empty(status: StatusCode, headers: &HeaderMap) -> Result<Self, Rollback> {
        Self::bytes(status, headers, Bytes::new())
    }

    pub(super) fn into_record(self) -> Record {
        self.0
    }

    pub(super) fn from_record(record: Record) -> Self {
        Self(record)
    }

    pub(super) fn into_response(self) -> Response {
        let (status, headers, body) = self.0.into_parts();
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        response
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderName;
    use http_body_util::BodyExt as _;

    use super::*;

    fn header_map(headers: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn admission_reports_invalid_status_and_oversized_headers_as_rollback() {
        std::assert_matches!(
            ReplayableResponse::empty(StatusCode::CONFLICT, &HeaderMap::new()),
            Err(Rollback::InvalidStatus)
        );
        let large = "x".repeat(8192);
        std::assert_matches!(
            ReplayableResponse::empty(
                StatusCode::OK,
                &header_map(&[("content-type", large.as_str())])
            ),
            Err(Rollback::HeaderLimit)
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        #[test]
        fn the_response_carries_the_status_body_and_only_the_allow_listed_headers(
            status in 200_u16..300,
            body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
            headers in proptest::collection::vec(
                (
                    proptest::sample::select(vec![
                        "content-type", "content-encoding", "content-language",
                        "content-disposition", "location", "etag", "last-modified",
                        "set-cookie", "authorization", "x-trace", "idempotent-replayed",
                    ]),
                    "[ -~]{1,24}",
                ),
                0..12,
            ),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let pairs: Vec<(&str, &str)> =
                headers.iter().map(|(name, value)| (*name, value.as_str())).collect();
            let captured = ReplayableResponse::bytes(
                StatusCode::from_u16(status).unwrap(), &header_map(&pairs), Bytes::from(body.clone())
            ).unwrap();
            let response = ReplayableResponse::from_record(captured.into_record()).into_response();

            proptest::prop_assert_eq!(response.status().as_u16(), status);
            let allowed = [
                "content-type", "content-encoding", "content-language", "content-disposition",
                "location", "etag", "last-modified",
            ];
            for name in response.headers().keys() {
                proptest::prop_assert!(allowed.contains(&name.as_str()), "{name}");
            }
            for name in allowed {
                let sent: Vec<_> = pairs
                    .iter()
                    .filter(|(sent, _)| *sent == name)
                    .map(|(_, value)| value.as_bytes())
                    .collect();
                let returned: Vec<_> =
                    response.headers().get_all(name).iter().map(HeaderValue::as_bytes).collect();
                proptest::prop_assert_eq!(returned, sent, "{}", name);
            }
            let returned_body = runtime
                .block_on(response.into_body().collect())
                .unwrap()
                .to_bytes();
            proptest::prop_assert_eq!(returned_body.as_ref(), body.as_slice());
        }
    }
}
