use std::fmt;

use bytes::Bytes;
use http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, CONTENT_TYPE, ETAG, LAST_MODIFIED,
    LOCATION,
};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use sqlx::postgres::{PgHasArrayType, PgTypeInfo, PgValueRef};
use sqlx::{Decode, Postgres, Type};

pub const MAX_STORED_BODY_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_STORED_HEADER_BYTES: usize = 8 * 1024;
const HEADER_PAIR_TYPE: &str = "http_idempotency_header_pair";

static REPLAYABLE: [HeaderName; 7] = [
    CONTENT_TYPE,
    CONTENT_ENCODING,
    CONTENT_LANGUAGE,
    CONTENT_DISPOSITION,
    LOCATION,
    ETAG,
    LAST_MODIFIED,
];

#[derive(Clone, Copy)]
struct StoredHeaderName(Option<&'static HeaderName>);

impl StoredHeaderName {
    fn parse(name: &str) -> Self {
        Self(
            REPLAYABLE
                .iter()
                .find(|candidate| candidate.as_str() == name),
        )
    }
}

impl Type<Postgres> for StoredHeaderName {
    fn type_info() -> PgTypeInfo {
        <str as Type<Postgres>>::type_info()
    }

    fn compatible(ty: &PgTypeInfo) -> bool {
        <str as Type<Postgres>>::compatible(ty)
    }
}

impl Decode<'_, Postgres> for StoredHeaderName {
    fn decode(value: PgValueRef<'_>) -> Result<Self, sqlx::error::BoxDynError> {
        Ok(Self::parse(value.as_str()?))
    }
}

#[derive(derive_more::Debug, sqlx::Decode)]
pub(crate) struct HeaderPair {
    #[debug(skip)]
    name: StoredHeaderName,
    #[debug(skip)]
    value: Vec<u8>,
}

impl Type<Postgres> for HeaderPair {
    fn type_info() -> PgTypeInfo {
        PgTypeInfo::with_name(HEADER_PAIR_TYPE)
    }
}

impl PgHasArrayType for HeaderPair {
    fn array_type_info() -> PgTypeInfo {
        PgTypeInfo::array_of(HEADER_PAIR_TYPE)
    }
}

#[derive(sqlx::Encode)]
pub(crate) struct HeaderPairRef<'a> {
    name: &'a str,
    value: &'a [u8],
}

impl Type<Postgres> for HeaderPairRef<'_> {
    fn type_info() -> PgTypeInfo {
        HeaderPair::type_info()
    }
}

impl PgHasArrayType for HeaderPairRef<'_> {
    fn array_type_info() -> PgTypeInfo {
        HeaderPair::array_type_info()
    }
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
    pub(crate) const fn cause(self) -> &'static str {
        match self {
            Self::BodyLimit => "body_limit",
            Self::Status => "status",
            Self::HeaderName => "header_name",
            Self::HeaderValue => "header_value",
            Self::HeaderLimit => "header_limit",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuccessRecord(Record);

impl SuccessRecord {
    pub fn capture(
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, CaptureError> {
        if !status.is_success() {
            return Err(CaptureError::InvalidStatus);
        }
        Record::capture_parts(status, headers, body).map(Self)
    }

    #[must_use]
    pub fn into_record(self) -> Record {
        self.0
    }
}

impl From<SuccessRecord> for Record {
    fn from(record: SuccessRecord) -> Self {
        record.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRefusal(Record);

impl TerminalRefusal {
    pub fn capture(
        status: StatusCode,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<Self, CaptureError> {
        if !is_terminal_refusal(status) {
            return Err(CaptureError::InvalidStatus);
        }
        Record::capture_parts(status, headers, body).map(Self)
    }

    #[must_use]
    pub fn into_record(self) -> Record {
        self.0
    }
}

impl From<TerminalRefusal> for Record {
    fn from(refusal: TerminalRefusal) -> Self {
        refusal.0
    }
}

impl Record {
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
        Ok(Self {
            status,
            headers: decode_headers(headers)?,
            body: if forbids_body(status) {
                Bytes::new()
            } else {
                body
            },
        })
    }

    pub(crate) const fn encode_status(&self) -> i16 {
        self.status.as_u16().cast_signed()
    }

    pub(crate) fn encode_headers(&self) -> impl Iterator<Item = HeaderPairRef<'_>> {
        replayable_headers(&self.headers).map(|(name, value)| HeaderPairRef {
            name: name.as_str(),
            value: value.as_bytes(),
        })
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

fn decode_headers(headers: Vec<HeaderPair>) -> Result<HeaderMap, Undecodable> {
    let mut admitted = HeaderMap::new();
    let mut size = 0_usize;
    for HeaderPair { name, value } in headers {
        let name = name.0.ok_or(Undecodable::HeaderName)?;
        let value = HeaderValue::try_from(value).map_err(|_| Undecodable::HeaderValue)?;
        size = size.saturating_add(header_size(name, &value));
        if size <= MAX_TOTAL_STORED_HEADER_BYTES {
            admitted.append(name, value);
        }
    }
    if size > MAX_TOTAL_STORED_HEADER_BYTES {
        return Err(Undecodable::HeaderLimit);
    }
    Ok(admitted)
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
            name: StoredHeaderName::parse(name),
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
        let record = SuccessRecord::capture(StatusCode::CREATED, &headers, body.clone())
            .unwrap()
            .into_record();

        assert_eq!(record.encode_status(), 201);
        assert_eq!(record.body(), b"stored");
        let encoded: Vec<_> = record
            .encode_headers()
            .map(|header| (header.name, header.value))
            .collect();
        assert_eq!(
            encoded,
            [
                ("content-type", [0xff].as_slice()),
                ("content-language", b"en".as_slice()),
                ("content-language", b"fr".as_slice()),
                ("etag", b"\"v1\"".as_slice()),
            ]
        );
        let stored_headers = encoded
            .into_iter()
            .map(|(name, value)| pair(name, value))
            .collect();
        let (status, headers, body) = Record::decode(record.encode_status(), stored_headers, body)
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
                200,
                vec![pair("x-unreplayable-long-header-name", b"value")],
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
                vec![pair(
                    "content-type",
                    &vec![b'a'; MAX_TOTAL_STORED_HEADER_BYTES],
                )],
                Bytes::new(),
                Undecodable::HeaderLimit,
            ),
        ] {
            assert_eq!(Record::decode(status, headers, body), Err(reason));
        }
    }

    #[test]
    fn success_and_terminal_refusal_enforce_disjoint_status_sets() {
        for status in [StatusCode::OK, StatusCode::CREATED, StatusCode::NO_CONTENT] {
            assert!(SuccessRecord::capture(status, &HeaderMap::new(), Bytes::new()).is_ok());
            assert_eq!(
                TerminalRefusal::capture(status, &HeaderMap::new(), Bytes::new()),
                Err(CaptureError::InvalidStatus)
            );
        }
        for status in [StatusCode::CONFLICT, StatusCode::UNPROCESSABLE_ENTITY] {
            assert!(TerminalRefusal::capture(status, &HeaderMap::new(), Bytes::new()).is_ok());
            assert_eq!(
                SuccessRecord::capture(status, &HeaderMap::new(), Bytes::new()),
                Err(CaptureError::InvalidStatus)
            );
        }
        for status in [StatusCode::BAD_REQUEST, StatusCode::INTERNAL_SERVER_ERROR] {
            assert_eq!(
                SuccessRecord::capture(status, &HeaderMap::new(), Bytes::new()),
                Err(CaptureError::InvalidStatus)
            );
            assert_eq!(
                TerminalRefusal::capture(status, &HeaderMap::new(), Bytes::new()),
                Err(CaptureError::InvalidStatus)
            );
        }
    }

    #[test]
    fn debug_output_omits_payloads_and_raw_header_pairs() {
        let mut headers = HeaderMap::new();
        headers.insert(LOCATION, HeaderValue::from_static("/private-resource"));
        let record = SuccessRecord::capture(
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
