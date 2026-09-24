//! Stored success format 1: capture, encoding, decoding, and replay.
//!
//! A 2xx response is captured once: its body is buffered within the bound,
//! only the replayable headers are kept, and the first response is answered
//! from that captured form, so it carries the same status, headers, and body
//! bytes as every later replay. The record store only moves the encoded
//! bytes.

use axum::body::{Body, Bytes};
use axum::http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_TYPE, LOCATION,
};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::Response;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use infra_idempotency_store::{Digest, Record};

/// The largest success body the boundary stores and replays (1 MiB).
pub const MAX_STORED_BODY_BYTES: usize = 1_048_576;

/// The largest total of replayable header fields the boundary stores,
/// counting `name.len + value.len + 4` per field (8 KiB).
pub const MAX_STORED_HEADER_BYTES: usize = 8_192;

/// The stored-success encoding this module writes and reads.
const FORMAT: i16 = 1;

/// The headers a replay reproduces, with their format-1 name ids.
const REPLAYABLE: [(u8, HeaderName); 5] = [
    (1, CONTENT_TYPE),
    (2, CONTENT_ENCODING),
    (3, CONTENT_LANGUAGE),
    (4, CONTENT_DISPOSITION),
    (5, LOCATION),
];

/// A captured success: what the first response and every replay carry.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) struct Stored {
    status: StatusCode,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Bytes,
}

/// Why a success cannot be stored. The work's transaction rolls back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unstorable {
    /// Not a 2xx status.
    Status,
    /// The body stream failed while buffering.
    BodyStream,
    /// The body exceeds [`MAX_STORED_BODY_BYTES`].
    BodyLimit,
    /// The replayable headers exceed [`MAX_STORED_HEADER_BYTES`].
    HeaderLimit,
}

impl Unstorable {
    /// The failure class for logs; never a value.
    pub(super) const fn class(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::BodyStream => "body_stream",
            Self::BodyLimit => "body_limit",
            Self::HeaderLimit => "header_limit",
        }
    }
}

/// A record that is not a valid stored success of format 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Undecodable;

/// Buffer a 2xx response within the body bound and keep its replayable
/// headers, every value, in `HeaderMap` order.
pub(super) async fn capture(response: Response) -> Result<Stored, Unstorable> {
    let (parts, body) = response.into_parts();
    if !parts.status.is_success() {
        return Err(Unstorable::Status);
    }
    let body = Limited::new(body, MAX_STORED_BODY_BYTES)
        .collect()
        .await
        .map_err(|error| {
            if error.is::<LengthLimitError>() {
                Unstorable::BodyLimit
            } else {
                Unstorable::BodyStream
            }
        })?
        .to_bytes();
    let headers = parts
        .headers
        .iter()
        .filter(|(name, _)| name_id(name).is_some())
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Ok(Stored {
        status: parts.status,
        headers,
        body,
    })
}

impl Stored {
    /// Encode as a format-1 record that carries `fingerprint`.
    pub(super) fn record(&self, fingerprint: Digest) -> Result<Record, Unstorable> {
        if header_bytes(&self.headers) > MAX_STORED_HEADER_BYTES {
            return Err(Unstorable::HeaderLimit);
        }
        let mut headers = Vec::new();
        for (name, value) in &self.headers {
            let id = name_id(name).ok_or(Unstorable::HeaderLimit)?;
            let length = u16::try_from(value.len()).map_err(|_| Unstorable::HeaderLimit)?;
            headers.push(id);
            headers.extend_from_slice(&length.to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let status = i16::try_from(self.status.as_u16()).map_err(|_| Unstorable::Status)?;
        Ok(Record {
            fingerprint,
            format: FORMAT,
            status,
            headers,
            body: self.body.to_vec(),
        })
    }

    /// The response: the stored status and body, then the stored headers in
    /// order. The transport adds the request id, trace context, `nosniff`,
    /// and framing of the exchange that sends it.
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

/// Decode a record written by [`Stored::record`], re-checking the format,
/// the status, the header names and lengths, and both bounds.
pub(super) fn decode(record: Record) -> Result<Stored, Undecodable> {
    if record.format != FORMAT || record.body.len() > MAX_STORED_BODY_BYTES {
        return Err(Undecodable);
    }
    let status = u16::try_from(record.status)
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .filter(StatusCode::is_success)
        .ok_or(Undecodable)?;
    let headers = decode_headers(&record.headers).ok_or(Undecodable)?;
    Ok(Stored {
        status,
        headers,
        body: Bytes::from(record.body),
    })
}

fn decode_headers(mut encoded: &[u8]) -> Option<Vec<(HeaderName, HeaderValue)>> {
    let mut headers = Vec::new();
    while let Some((&id, rest)) = encoded.split_first() {
        let name = REPLAYABLE
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, name)| name.clone())?;
        let (length, rest) = rest.split_first_chunk::<2>()?;
        let (value, rest) = rest.split_at_checked(usize::from(u16::from_be_bytes(*length)))?;
        headers.push((name, HeaderValue::from_bytes(value).ok()?));
        encoded = rest;
    }
    (header_bytes(&headers) <= MAX_STORED_HEADER_BYTES).then_some(headers)
}

fn name_id(name: &HeaderName) -> Option<u8> {
    REPLAYABLE
        .iter()
        .find(|(_, candidate)| candidate == name)
        .map(|(id, _)| *id)
}

/// The header accounting of the specification: `name.len + value.len + 4`
/// per field.
fn header_bytes(headers: &[(HeaderName, HeaderValue)]) -> usize {
    headers.iter().fold(0, |total, (name, value)| {
        total
            .saturating_add(name.as_str().len())
            .saturating_add(value.len())
            .saturating_add(4)
    })
}

#[cfg(test)]
mod tests {
    use axum::http::header::SET_COOKIE;

    use super::*;
    use crate::idempotency::openapi::REPLAYABLE_HEADERS;

    const FINGERPRINT: Digest = [7; 32];

    fn response(status: u16, headers: &[(&str, &str)], body: impl Into<Body>) -> Response {
        let mut builder = Response::builder().status(status);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(body.into()).unwrap()
    }

    fn header(name: &str, value: &str) -> (HeaderName, HeaderValue) {
        (
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        )
    }

    fn record(status: i16, headers: Vec<u8>, body: Vec<u8>) -> Record {
        Record {
            fingerprint: FINGERPRINT,
            format: FORMAT,
            status,
            headers,
            body,
        }
    }

    #[test]
    fn replayable_names_are_the_declared_replayable_headers() {
        let stored: Vec<&str> = REPLAYABLE.iter().map(|(_, name)| name.as_str()).collect();
        let declared: Vec<String> = REPLAYABLE_HEADERS
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        assert_eq!(stored, declared);
        let ids: Vec<u8> = REPLAYABLE.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn a_success_round_trips_with_only_replayable_headers_in_order() {
        let captured = capture(response(
            201,
            &[
                ("content-type", "application/json"),
                ("x-trace", "drop-me"),
                ("location", "/widgets/1"),
                ("content-language", "en"),
                ("content-language", "fr"),
                ("set-cookie", "session=secret"),
            ],
            r#"{"id":1}"#,
        ))
        .await
        .unwrap();
        let kept = vec![
            header("content-type", "application/json"),
            header("location", "/widgets/1"),
            header("content-language", "en"),
            header("content-language", "fr"),
        ];
        assert_eq!(
            captured,
            Stored {
                status: StatusCode::CREATED,
                headers: kept.clone(),
                body: Bytes::from_static(br#"{"id":1}"#),
            }
        );

        let encoded = captured.record(FINGERPRINT).unwrap();
        let expected_headers = [
            &[1, 0, 16][..],
            b"application/json",
            &[5, 0, 10],
            b"/widgets/1",
            &[3, 0, 2],
            b"en",
            &[3, 0, 2],
            b"fr",
        ]
        .concat();
        assert_eq!(
            encoded,
            record(201, expected_headers, br#"{"id":1}"#.to_vec())
        );

        let replayed = decode(encoded).unwrap();
        assert_eq!(replayed, captured);
        let replay = replayed.into_response();
        assert_eq!(replay.status(), StatusCode::CREATED);
        let sent: Vec<(HeaderName, HeaderValue)> = replay
            .headers()
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        assert_eq!(sent, kept);
        assert!(!replay.headers().contains_key(SET_COOKIE));
        let body = replay.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), br#"{"id":1}"#);
    }

    #[tokio::test]
    async fn a_body_of_exactly_one_mebibyte_is_stored_and_one_more_byte_is_not() {
        let exact = capture(response(200, &[], vec![b'x'; MAX_STORED_BODY_BYTES]))
            .await
            .unwrap();
        let encoded = exact.record(FINGERPRINT).unwrap();
        assert_eq!(encoded.body.len(), MAX_STORED_BODY_BYTES);
        assert_eq!(decode(encoded).unwrap(), exact);

        let over = capture(response(200, &[], vec![b'x'; MAX_STORED_BODY_BYTES + 1])).await;
        assert_eq!(over.unwrap_err(), Unstorable::BodyLimit);
    }

    #[tokio::test]
    async fn a_failing_body_or_a_non_success_status_is_unstorable() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"partial")),
            Err(std::io::Error::other("stream broke")),
        ];
        let failing = response(
            200,
            &[],
            Body::from_stream(futures_util::stream::iter(chunks)),
        );
        assert_eq!(capture(failing).await.unwrap_err(), Unstorable::BodyStream);
        assert_eq!(
            capture(response(404, &[], "missing")).await.unwrap_err(),
            Unstorable::Status
        );
    }

    #[tokio::test]
    async fn header_fields_are_bounded_by_name_value_and_four_bytes_each() {
        // content-disposition (19) + value + 4 = 8192 at the bound.
        let at_bound = "a".repeat(MAX_STORED_HEADER_BYTES - 19 - 4);
        let captured = capture(response(
            200,
            &[("content-disposition", at_bound.as_str())],
            "",
        ))
        .await
        .unwrap();
        let encoded = captured.record(FINGERPRINT).unwrap();
        assert_eq!(decode(encoded).unwrap(), captured);

        let over_bound = format!("{at_bound}a");
        let captured = capture(response(
            200,
            &[("content-disposition", over_bound.as_str())],
            "",
        ))
        .await
        .unwrap();
        assert_eq!(
            captured.record(FINGERPRINT).unwrap_err(),
            Unstorable::HeaderLimit
        );
    }

    #[test]
    fn decoding_refuses_every_invalid_record() {
        let valid_headers = [&[1, 0, 10][..], b"text/plain"].concat();
        assert!(decode(record(200, valid_headers.clone(), b"ok".to_vec())).is_ok());

        let mut wrong_format = record(200, valid_headers.clone(), b"ok".to_vec());
        wrong_format.format = 2;
        let over_header_bound = [
            &[4, 0x1f, 0xea][..],
            "a".repeat(MAX_STORED_HEADER_BYTES - 19 - 3).as_bytes(),
        ]
        .concat();
        let invalid = [
            wrong_format,
            record(199, Vec::new(), Vec::new()),
            record(300, Vec::new(), Vec::new()),
            record(404, Vec::new(), Vec::new()),
            record(-200, Vec::new(), Vec::new()),
            record(200, vec![0, 0, 1, b'a'], Vec::new()),
            record(200, vec![6, 0, 1, b'a'], Vec::new()),
            record(200, vec![1, 0], Vec::new()),
            record(200, vec![1, 0, 5, b'a', b'b'], Vec::new()),
            record(200, [&valid_headers[..], &[5]].concat(), Vec::new()),
            record(200, vec![5, 0, 3, b'a', b'\n', b'b'], Vec::new()),
            record(200, vec![5, 0, 1, 0x7f], Vec::new()),
            record(200, over_header_bound, Vec::new()),
            record(200, Vec::new(), vec![b'x'; MAX_STORED_BODY_BYTES + 1]),
        ];
        for record in invalid {
            assert_eq!(decode(record).unwrap_err(), Undecodable);
        }
    }
}
