use std::fmt;

use bytes::Bytes;
use http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_TYPE, ETAG, LAST_MODIFIED,
    LOCATION,
};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};

pub const MAX_STORED_BODY_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_STORED_HEADER_BYTES: usize = 8 * 1024;

static REPLAYABLE: [HeaderName; 7] = [
    CONTENT_TYPE,
    CONTENT_ENCODING,
    CONTENT_LANGUAGE,
    CONTENT_DISPOSITION,
    LOCATION,
    ETAG,
    LAST_MODIFIED,
];

#[derive(PartialEq, Eq, derive_more::Debug, sqlx::Type)]
#[sqlx(type_name = "http_idempotency_header_pair")]
pub(crate) struct HeaderPair {
    #[debug(skip)]
    name: String,
    #[debug(skip)]
    value: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Record {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl fmt::Debug for Record {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Record")
            .field("status", &self.status)
            .field("header_count", &self.headers.len())
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    #[error("status")]
    InvalidStatus,
    #[error("body_forbidden")]
    BodyForbidden,
    #[error("body_limit")]
    BodyLimit,
    #[error("header_limit")]
    HeaderLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Undecodable {
    BodyLimit,
    Status,
    HeaderName,
    HeaderValue,
    HeaderLimit,
}

impl Undecodable {
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::BodyLimit => "body_limit",
            Self::Status => "status",
            Self::HeaderName => "header_name",
            Self::HeaderValue => "header_value",
            Self::HeaderLimit => "header_limit",
        }
    }
}

impl Record {
    pub fn capture(
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, CaptureError> {
        if !status.is_success() {
            return Err(CaptureError::InvalidStatus);
        }
        Self::capture_parts(status, headers, body)
    }

    pub fn capture_terminal_refusal(
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, CaptureError> {
        if !is_terminal_refusal(status) {
            return Err(CaptureError::InvalidStatus);
        }
        Self::capture_parts(status, headers, body)
    }

    fn capture_parts(
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, CaptureError> {
        if forbids_body(status) && !body.is_empty() {
            return Err(CaptureError::BodyForbidden);
        }
        if body.len() > MAX_STORED_BODY_BYTES {
            return Err(CaptureError::BodyLimit);
        }
        let mut admitted = HeaderMap::new();
        let mut size = 0_usize;
        for (name, value) in replayable_headers(headers) {
            size = size.saturating_add(header_size(name, value));
            if size > MAX_TOTAL_STORED_HEADER_BYTES {
                return Err(CaptureError::HeaderLimit);
            }
            admitted.append(name, value.clone());
        }
        Ok(Self {
            status,
            headers: admitted,
            body,
        })
    }

    #[must_use]
    pub fn into_parts(self) -> (StatusCode, HeaderMap, Bytes) {
        (self.status, self.headers, self.body)
    }

    pub(crate) fn decode(
        status: i16,
        headers: Vec<HeaderPair>,
        body: Bytes,
    ) -> Result<Self, Undecodable> {
        if body.len() > MAX_STORED_BODY_BYTES {
            return Err(Undecodable::BodyLimit);
        }
        let status = u16::try_from(status)
            .ok()
            .and_then(|status| StatusCode::from_u16(status).ok())
            .filter(|status| status.is_success() || is_terminal_refusal(*status))
            .ok_or(Undecodable::Status)?;
        let mut admitted = HeaderMap::new();
        let mut size = 0_usize;
        for HeaderPair { name, value } in headers {
            let name = REPLAYABLE
                .iter()
                .find(|candidate| candidate.as_str() == name)
                .ok_or(Undecodable::HeaderName)?;
            let value = HeaderValue::try_from(value).map_err(|_| Undecodable::HeaderValue)?;
            size = size.saturating_add(header_size(name, &value));
            if size <= MAX_TOTAL_STORED_HEADER_BYTES {
                admitted.append(name, value);
            }
        }
        if size > MAX_TOTAL_STORED_HEADER_BYTES {
            return Err(Undecodable::HeaderLimit);
        }
        Ok(Self {
            status,
            headers: admitted,
            body: if forbids_body(status) {
                Bytes::new()
            } else {
                body
            },
        })
    }

    pub(crate) const fn status(&self) -> i16 {
        self.status.as_u16().cast_signed()
    }

    #[must_use]
    pub fn is_terminal_refusal(&self) -> bool {
        is_terminal_refusal(self.status)
    }

    pub(crate) fn encode_headers(&self) -> Vec<HeaderPair> {
        replayable_headers(&self.headers)
            .map(|(name, value)| HeaderPair {
                name: name.as_str().to_owned(),
                value: value.as_bytes().to_vec(),
            })
            .collect()
    }

    pub(crate) fn body(&self) -> &[u8] {
        self.body.as_ref()
    }
}

#[must_use]
pub fn is_replayable_header_name(name: &str) -> bool {
    REPLAYABLE
        .iter()
        .any(|candidate| candidate.as_str().eq_ignore_ascii_case(name))
}

fn replayable_headers(
    headers: &HeaderMap,
) -> impl Iterator<Item = (&'static HeaderName, &HeaderValue)> {
    REPLAYABLE
        .iter()
        .flat_map(move |name| headers.get_all(name).iter().map(move |value| (name, value)))
}

fn header_size(name: &HeaderName, value: &HeaderValue) -> usize {
    name.as_str().len().saturating_add(value.len())
}

fn forbids_body(status: StatusCode) -> bool {
    matches!(status, StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT)
}

fn is_terminal_refusal(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, value: &[u8]) -> HeaderPair {
        HeaderPair {
            name: name.to_owned(),
            value: value.to_vec(),
        }
    }

    #[test]
    fn storage_keeps_canonical_names_and_repeat_order() {
        let mut headers = HeaderMap::new();
        headers.append(CONTENT_LANGUAGE, HeaderValue::from_static("en"));
        headers.append(ETAG, HeaderValue::from_static("\"v1\""));
        headers.append(CONTENT_LANGUAGE, HeaderValue::from_static("fr"));
        headers.append(CONTENT_TYPE, HeaderValue::from_bytes(&[0xff]).unwrap());
        headers.append("set-cookie", HeaderValue::from_static("secret"));
        let body = Bytes::from_static(b"stored");
        let record = Record::capture(StatusCode::CREATED, &headers, body.clone()).unwrap();

        assert_eq!(record.status(), 201);
        assert_eq!(record.body(), b"stored");
        assert_eq!(
            record.encode_headers(),
            [
                pair("content-type", &[0xff]),
                pair("content-language", b"en"),
                pair("content-language", b"fr"),
                pair("etag", b"\"v1\""),
            ]
        );
        let (status, headers, body) =
            Record::decode(record.status(), record.encode_headers(), body)
                .unwrap()
                .into_parts();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(headers[CONTENT_TYPE].as_bytes(), &[0xff]);
        let languages: Vec<_> = headers
            .get_all(CONTENT_LANGUAGE)
            .iter()
            .map(HeaderValue::as_bytes)
            .collect();
        assert_eq!(languages, [b"en".as_slice(), b"fr".as_slice()]);
        assert_eq!(headers[ETAG], "\"v1\"");
        assert!(!headers.contains_key("set-cookie"));
        assert_eq!(body.as_ref(), b"stored");
    }

    #[test]
    fn decoding_rejects_corrupt_records() {
        let large_body = Bytes::from(vec![0; MAX_STORED_BODY_BYTES + 1]);
        for (status, headers, body, reason) in [
            (205, vec![], large_body, Undecodable::BodyLimit),
            (199, vec![], Bytes::new(), Undecodable::Status),
            (
                200,
                vec![pair("Content-Type", b"\n")],
                Bytes::new(),
                Undecodable::HeaderName,
            ),
            (
                200,
                vec![pair("set-cookie", b"secret")],
                Bytes::new(),
                Undecodable::HeaderName,
            ),
            (
                204,
                vec![pair("content-type", b"\n")],
                Bytes::from_static(b"legacy body"),
                Undecodable::HeaderValue,
            ),
            (
                200,
                vec![HeaderPair {
                    name: "content-type".to_owned(),
                    value: vec![b'a'; MAX_TOTAL_STORED_HEADER_BYTES],
                }],
                Bytes::new(),
                Undecodable::HeaderLimit,
            ),
        ] {
            assert_eq!(Record::decode(status, headers, body), Err(reason));
        }
    }

    #[test]
    fn debug_output_omits_payloads_and_raw_header_pairs() {
        let mut headers = HeaderMap::new();
        headers.insert(LOCATION, HeaderValue::from_static("/private-resource"));
        let record = Record::capture(
            StatusCode::OK,
            &headers,
            Bytes::from_static(b"private-body"),
        )
        .unwrap();
        let debug = format!("{record:?}");
        assert!(!debug.contains("private-body"));
        assert!(!debug.contains("private-resource"));

        let raw_pair = pair("private-name", b"private-value");
        let debug = format!("{raw_pair:?}");
        assert!(!debug.contains("private-name"));
        assert!(!debug.contains("private-value"));
        assert!(!debug.contains(&format!("{:?}", b"private-value")));
    }
}
