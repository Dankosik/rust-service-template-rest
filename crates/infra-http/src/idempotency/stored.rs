use axum::body::{Body, Bytes};
use axum::http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_TYPE, ETAG, LAST_MODIFIED,
    LOCATION,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::{BufMut as _, BytesMut};
use infra_idempotency_store::{HeaderPair, Record};
use serde::Serialize;
use smallvec::SmallVec;

use super::execute::Rollback;

const MAX_STORED_BODY_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_STORED_HEADER_BYTES: usize = 8 * 1024;

const REPLAYABLE: [HeaderName; 7] = [
    CONTENT_TYPE,
    CONTENT_ENCODING,
    CONTENT_LANGUAGE,
    CONTENT_DISPOSITION,
    LOCATION,
    ETAG,
    LAST_MODIFIED,
];

type StoredHeaders = SmallVec<[(HeaderName, HeaderValue); 4]>;

#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct ReplayableResponse {
    status: StatusCode,
    headers: StoredHeaders,
    body: Bytes,
}

impl std::fmt::Debug for ReplayableResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReplayableResponse")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Undecodable {
    BodyLimit,
    Status,
    HeaderName,
    HeaderValue,
    HeaderLimit,
}

impl Undecodable {
    pub(super) const fn class(self) -> &'static str {
        match self {
            Self::BodyLimit => "body_limit",
            Self::Status => "status",
            Self::HeaderName => "header_name",
            Self::HeaderValue => "header_value",
            Self::HeaderLimit => "header_limit",
        }
    }
}

impl ReplayableResponse {
    pub fn bytes(status: StatusCode, headers: &HeaderMap, body: Bytes) -> Result<Self, Rollback> {
        if !status.is_success() {
            return Err(Rollback::InvalidStatus);
        }
        if body.len() > MAX_STORED_BODY_BYTES {
            return Err(Rollback::BodyLimit);
        }
        let headers = REPLAYABLE
            .iter()
            .flat_map(|name| {
                headers
                    .get_all(name)
                    .iter()
                    .map(move |value| (name.clone(), value.clone()))
            })
            .collect::<StoredHeaders>();
        if header_bytes(&headers) > MAX_TOTAL_STORED_HEADER_BYTES {
            return Err(Rollback::HeaderLimit);
        }
        Ok(Self {
            status,
            headers,
            body,
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

    pub(super) fn to_record(&self) -> Record {
        let headers = self
            .headers
            .iter()
            .map(|(name, value)| HeaderPair {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .collect();
        let status = self.status.as_u16().cast_signed();
        Record {
            status,
            headers,
            body: self.body.clone(),
        }
    }

    pub(super) fn into_response(self) -> Response {
        let mut response = Response::new(Body::from(self.body));
        *response.status_mut() = self.status;
        response.headers_mut().extend(self.headers);
        response
    }
}

pub(super) fn decode(record: Record) -> Result<ReplayableResponse, Undecodable> {
    if record.body.len() > MAX_STORED_BODY_BYTES {
        return Err(Undecodable::BodyLimit);
    }
    let status = u16::try_from(record.status)
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .filter(StatusCode::is_success)
        .ok_or(Undecodable::Status)?;
    let headers = record
        .headers
        .into_iter()
        .map(|pair| {
            let name = REPLAYABLE
                .into_iter()
                .find(|name| name.as_str() == pair.name)
                .ok_or(Undecodable::HeaderName)?;
            let value = HeaderValue::try_from(pair.value).map_err(|_| Undecodable::HeaderValue)?;
            Ok((name, value))
        })
        .collect::<Result<StoredHeaders, _>>()?;
    if header_bytes(&headers) > MAX_TOTAL_STORED_HEADER_BYTES {
        return Err(Undecodable::HeaderLimit);
    }
    Ok(ReplayableResponse {
        status,
        headers,
        body: record.body,
    })
}

pub(super) fn is_replayable_header_name(name: &str) -> bool {
    REPLAYABLE
        .iter()
        .any(|candidate| candidate.as_str().eq_ignore_ascii_case(name))
}

fn header_bytes(headers: &[(HeaderName, HeaderValue)]) -> usize {
    headers.iter().fold(0, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;

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

    fn record(status: i16, headers: Vec<HeaderPair>, body: Vec<u8>) -> Record {
        Record {
            status,
            headers,
            body: body.into(),
        }
    }

    #[tokio::test]
    async fn first_success_and_replay_have_the_same_admitted_bytes() {
        let captured = ReplayableResponse::bytes(
            StatusCode::CREATED,
            &header_map(&[
                ("content-type", "application/json"),
                ("x-trace", "drop"),
                ("content-language", "en"),
                ("content-language", "fr"),
                ("etag", "\"v1\""),
                ("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT"),
                ("set-cookie", "secret"),
            ]),
            Bytes::from_static(br#"{"id":1}"#),
        )
        .expect("storable response");
        let record = captured.to_record();
        assert_eq!(
            record.headers,
            vec![
                HeaderPair {
                    name: "content-type".to_owned(),
                    value: b"application/json".to_vec()
                },
                HeaderPair {
                    name: "content-language".to_owned(),
                    value: b"en".to_vec()
                },
                HeaderPair {
                    name: "content-language".to_owned(),
                    value: b"fr".to_vec()
                },
                HeaderPair {
                    name: "etag".to_owned(),
                    value: b"\"v1\"".to_vec()
                },
                HeaderPair {
                    name: "last-modified".to_owned(),
                    value: b"Mon, 01 Jan 2024 00:00:00 GMT".to_vec()
                },
            ]
        );
        let replay = decode(record).expect("admitted record").into_response();
        assert_eq!(replay.status(), StatusCode::CREATED);
        assert_eq!(
            replay.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert_eq!(replay.headers().get_all(CONTENT_LANGUAGE).iter().count(), 2);
        assert!(!replay.headers().contains_key("x-trace"));
        assert_eq!(
            replay.into_body().collect().await.expect("body").to_bytes(),
            Bytes::from_static(br#"{"id":1}"#)
        );
    }

    #[test]
    fn admission_rejects_non_success_and_oversized_headers() {
        std::assert_matches!(
            ReplayableResponse::empty(StatusCode::CONFLICT, &HeaderMap::new()),
            Err(Rollback::InvalidStatus)
        );
        let large = "x".repeat(MAX_TOTAL_STORED_HEADER_BYTES);
        std::assert_matches!(
            ReplayableResponse::empty(
                StatusCode::OK,
                &header_map(&[("content-type", large.as_str())])
            ),
            Err(Rollback::HeaderLimit)
        );
    }

    #[test]
    fn decode_refuses_corrupt_or_noncanonical_structured_headers() {
        let valid = record(
            200,
            vec![HeaderPair {
                name: "content-type".to_owned(),
                value: vec![0xff],
            }],
            b"stored".to_vec(),
        );
        let response = decode(valid).unwrap().into_response();
        assert_eq!(response.headers()[CONTENT_TYPE].as_bytes(), &[0xff]);
        for (invalid, reason) in [
            (record(199, Vec::new(), Vec::new()), Undecodable::Status),
            (
                record(
                    200,
                    vec![HeaderPair {
                        name: "Content-Type".to_owned(),
                        value: vec![b'\n'],
                    }],
                    Vec::new(),
                ),
                Undecodable::HeaderName,
            ),
            (
                record(
                    200,
                    vec![HeaderPair {
                        name: "set-cookie".to_owned(),
                        value: b"secret".to_vec(),
                    }],
                    Vec::new(),
                ),
                Undecodable::HeaderName,
            ),
            (
                record(
                    200,
                    vec![HeaderPair {
                        name: "content-type".to_owned(),
                        value: vec![b'\n'],
                    }],
                    Vec::new(),
                ),
                Undecodable::HeaderValue,
            ),
        ] {
            assert_eq!(decode(invalid), Err(reason));
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        #[test]
        fn a_replay_carries_the_status_body_and_only_the_allow_listed_headers(
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
            let replay = decode(captured.to_record()).unwrap().into_response();

            proptest::prop_assert_eq!(replay.status().as_u16(), status);
            for name in replay.headers().keys() {
                proptest::prop_assert!(is_replayable_header_name(name.as_str()), "{name}");
            }
            for name in REPLAYABLE {
                let sent: Vec<_> = pairs
                    .iter()
                    .filter(|(sent, _)| *sent == name.as_str())
                    .map(|(_, value)| value.as_bytes())
                    .collect();
                let replayed: Vec<_> =
                    replay.headers().get_all(&name).iter().map(HeaderValue::as_bytes).collect();
                proptest::prop_assert_eq!(replayed, sent, "{}", name);
            }
            let replayed_body = runtime
                .block_on(replay.into_body().collect())
                .unwrap()
                .to_bytes();
            proptest::prop_assert_eq!(replayed_body.as_ref(), body.as_slice());
        }
    }
}
