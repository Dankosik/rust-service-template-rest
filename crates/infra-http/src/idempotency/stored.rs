//! Stored-success admission and replay.

use axum::body::{Body, Bytes};
use axum::http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_TYPE, ETAG, LAST_MODIFIED,
    LOCATION,
};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use http_body_util::{BodyExt, Limited};
use infra_idempotency_store::{HeaderPair, Record};

use super::caused_by_length_limit;

/// The largest success body the boundary stores and replays (1 MiB).
const MAX_STORED_BODY_BYTES: usize = 1_048_576;

/// The largest total of replayable header name and value bytes (8 KiB).
const MAX_STORED_HEADER_BYTES: usize = 8_192;

/// The only headers the idempotency boundary stores and replays. This
/// allow-list is what keeps `Idempotent-Replayed` out of storage: the marker
/// derives from the sealed outcome after the handler returns.
const REPLAYABLE: [HeaderName; 7] = [
    CONTENT_TYPE,
    CONTENT_ENCODING,
    CONTENT_LANGUAGE,
    CONTENT_DISPOSITION,
    LOCATION,
    ETAG,
    LAST_MODIFIED,
];

/// A captured success: what the first response and every replay carry.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct Stored {
    status: StatusCode,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Bytes,
}

/// Why a successful response cannot be stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unstorable {
    Status,
    BodyStream,
    BodyLimit,
    HeaderLimit,
}

impl Unstorable {
    pub(super) const fn class(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::BodyStream => "body_stream",
            Self::BodyLimit => "body_limit",
            Self::HeaderLimit => "header_limit",
        }
    }
}

/// Why a stored record cannot be replayed.
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

pub(super) async fn capture(response: Response) -> Result<Stored, Unstorable> {
    let (parts, body) = response.into_parts();
    if !parts.status.is_success() {
        return Err(Unstorable::Status);
    }
    let body = Limited::new(body, MAX_STORED_BODY_BYTES)
        .collect()
        .await
        .map_err(|error| {
            if caused_by_length_limit(&*error) {
                Unstorable::BodyLimit
            } else {
                Unstorable::BodyStream
            }
        })?
        .to_bytes();
    let headers = REPLAYABLE
        .iter()
        .flat_map(|name| {
            parts
                .headers
                .get_all(name)
                .iter()
                .map(move |value| (name.clone(), value.clone()))
        })
        .collect::<Vec<_>>();
    if header_bytes(&headers) > MAX_STORED_HEADER_BYTES {
        return Err(Unstorable::HeaderLimit);
    }
    Ok(Stored {
        status: parts.status,
        headers,
        body,
    })
}

impl Stored {
    pub(super) fn to_record(&self) -> Result<Record, Unstorable> {
        let headers = self
            .headers
            .iter()
            .map(|(name, value)| HeaderPair {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .collect();
        let status = i16::try_from(self.status.as_u16()).map_err(|_| Unstorable::Status)?;
        Ok(Record {
            status,
            headers,
            // Share the captured Bytes with persistence while Stored retains
            // the first response; cloning here must not copy up to 1 MiB.
            body: self.body.clone(),
        })
    }

    pub(super) fn into_response(self) -> Response {
        let mut response = Response::new(Body::from(self.body));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        for (name, value) in self.headers {
            headers.append(name, value);
        }
        response
    }
}

/// Validate a database record before replaying it.
pub(super) fn decode(record: Record) -> Result<Stored, Undecodable> {
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
            let name = HeaderName::from_bytes(pair.name.as_bytes())
                .map_err(|_| Undecodable::HeaderName)?;
            // `capture` stores only the canonical allowlist spellings. Another
            // spelling is a record this boundary did not write and would replay
            // a duplicate of an allowlisted header.
            if name.as_str() != pair.name || !is_replayable_header_name(name.as_str()) {
                return Err(Undecodable::HeaderName);
            }
            let value =
                HeaderValue::from_bytes(&pair.value).map_err(|_| Undecodable::HeaderValue)?;
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if header_bytes(&headers) > MAX_STORED_HEADER_BYTES {
        return Err(Undecodable::HeaderLimit);
    }
    Ok(Stored {
        status,
        headers,
        body: record.body,
    })
}

/// Whether a documented success header can be retained and replayed.
pub(super) fn is_replayable_header_name(name: &str) -> bool {
    REPLAYABLE
        .iter()
        .any(|candidate| candidate.as_str().eq_ignore_ascii_case(name))
}

/// Saturates, so an overflowing total still reads as over any limit.
fn header_bytes(headers: &[(HeaderName, HeaderValue)]) -> usize {
    headers.iter().fold(0, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.len())
    })
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    fn response(status: u16, headers: &[(&str, &str)], body: impl Into<Body>) -> Response {
        let mut builder = Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(body.into()).expect("valid response")
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
        let captured = capture(response(
            201,
            &[
                ("content-type", "application/json"),
                ("x-trace", "drop"),
                ("content-language", "en"),
                ("content-language", "fr"),
                ("etag", "\"v1\""),
                ("last-modified", "Mon, 01 Jan 2024 00:00:00 GMT"),
                ("set-cookie", "secret"),
            ],
            br#"{"id":1}"#.as_slice(),
        ))
        .await
        .expect("storable response");
        let record = captured.to_record().expect("record");
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

    #[tokio::test]
    async fn admission_rejects_non_success_and_oversized_headers() {
        assert_eq!(
            capture(response(409, &[], Body::empty())).await,
            Err(Unstorable::Status)
        );
        let large = "x".repeat(MAX_STORED_HEADER_BYTES);
        assert_eq!(
            capture(response(
                200,
                &[("content-type", large.as_str())],
                Body::empty()
            ))
            .await,
            Err(Unstorable::HeaderLimit)
        );
    }

    #[test]
    fn decode_refuses_corrupt_or_noncanonical_structured_headers() {
        let valid = record(
            200,
            vec![HeaderPair {
                name: "content-type".to_owned(),
                value: b"text/plain".to_vec(),
            }],
            b"stored".to_vec(),
        );
        assert!(decode(valid).is_ok());
        for (invalid, reason) in [
            (record(199, Vec::new(), Vec::new()), Undecodable::Status),
            (
                record(
                    200,
                    vec![HeaderPair {
                        name: "Content-Type".to_owned(),
                        value: b"text/plain".to_vec(),
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
}
