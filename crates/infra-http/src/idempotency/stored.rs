use axum::body::{Body, Bytes};
use axum::http::header::{CONTENT_ENCODING, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::{BufMut as _, BytesMut};
use infra_idempotency_store::{CaptureError, MAX_STORED_BODY_BYTES, Record};
use serde::Serialize;

use super::execute::Rollback;
use crate::problem::{Code, Problem, http_status};

pub(super) use infra_idempotency_store::is_replayable_header_name;

#[derive(Debug)]
pub struct ReplayableResponse(Record);

impl ReplayableResponse {
    pub fn bytes(status: StatusCode, headers: &HeaderMap, body: Bytes) -> Result<Self, Rollback> {
        Record::capture(status, headers, body)
            .map(Self)
            .map_err(Rollback::from)
    }

    pub fn json<T: Serialize + ?Sized>(
        status: StatusCode,
        mut headers: HeaderMap,
        value: &T,
    ) -> Result<Self, Rollback> {
        if headers
            .get_all(CONTENT_ENCODING)
            .iter()
            .flat_map(|value| value.as_bytes().split(|&byte| byte == b','))
            .any(|coding| !coding.trim_ascii().eq_ignore_ascii_case(b"identity"))
        {
            return Err(Rollback::ContentEncoding);
        }
        headers.remove(CONTENT_ENCODING);

        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Self::bytes(status, &headers, json_body(value)?)
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

    pub(super) fn terminal_problem(problem: &Problem) -> Result<Record, Rollback> {
        if !matches!(
            problem.code(),
            Code::Conflict | Code::AlreadyExists | Code::UnprocessableContent
        ) {
            return Err(Rollback::InvalidStatus);
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        Record::capture_terminal_refusal(http_status(problem.code()), &headers, json_body(problem)?)
            .map_err(Rollback::from)
    }

    pub(super) fn into_response(self) -> Response {
        let (status, headers, body) = self.0.into_parts();
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        response
    }
}

fn json_body<T: Serialize + ?Sized>(value: &T) -> Result<Bytes, Rollback> {
    let mut body = BytesMut::new();
    let mut writer = (&mut body).limit(MAX_STORED_BODY_BYTES).writer();
    serde_json::to_writer(&mut writer, value).map_err(|error| {
        if error.io_error_kind() == Some(std::io::ErrorKind::WriteZero) {
            Rollback::BodyLimit
        } else {
            Rollback::Serialization
        }
    })?;
    Ok(body.freeze())
}

impl From<CaptureError> for Rollback {
    fn from(error: CaptureError) -> Self {
        match error {
            CaptureError::InvalidStatus => Self::InvalidStatus,
            CaptureError::BodyForbidden => Self::BodyForbidden,
            CaptureError::BodyLimit => Self::BodyLimit,
            CaptureError::HeaderLimit => Self::HeaderLimit,
        }
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

    #[test]
    fn admission_rejects_nonempty_bodies_for_no_content_and_reset_content() {
        for status in [StatusCode::NO_CONTENT, StatusCode::RESET_CONTENT] {
            std::assert_matches!(
                ReplayableResponse::bytes(status, &HeaderMap::new(), Bytes::from_static(b"body")),
                Err(Rollback::BodyForbidden)
            );
        }
    }

    #[tokio::test]
    async fn json_requires_uncoded_content() {
        for headers in [
            header_map(&[("content-encoding", "gzip")]),
            header_map(&[("content-encoding", "identity, gzip")]),
            header_map(&[("content-encoding", "identity"), ("content-encoding", "br")]),
        ] {
            std::assert_matches!(
                ReplayableResponse::json(StatusCode::OK, headers, &1),
                Err(Rollback::ContentEncoding)
            );
        }

        let response = ReplayableResponse::json(
            StatusCode::OK,
            header_map(&[("content-encoding", "IDENTITY")]),
            &1,
        )
        .unwrap()
        .into_response();
        assert!(!response.headers().contains_key(CONTENT_ENCODING));
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(b"1")
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
            let body = if matches!(status, 204 | 205) { Vec::new() } else { body };
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
