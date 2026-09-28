//! Go-compatible NATS envelope headers and bounded decoding.

use async_nats::{HeaderMap, HeaderName};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::error::MessagingError;
use crate::prepared::PreparedEvent;

pub const HEADER_LIMIT_BYTES: usize = 8 * 1024;
pub const MESSAGE_ID: &str = "Message-Id";
pub const EVENT_TYPE: &str = "Event-Type";
pub const EVENT_SCHEMA: &str = "Event-Schema";
pub const CREATED_AT: &str = "Created-At";
pub const NATS_MSG_ID: &str = "Nats-Msg-Id";
pub const ORIGINAL_SUBJECT: &str = "Original-Subject";
pub const DEAD_LETTER_REASON: &str = "Dead-Letter-Reason";

// A `&str` header name is validated and copied on every insert and lookup;
// these are built at compile time.
pub(crate) mod name {
    use async_nats::HeaderName;

    pub(crate) const MESSAGE_ID: HeaderName = HeaderName::from_static(super::MESSAGE_ID);
    pub(crate) const EVENT_TYPE: HeaderName = HeaderName::from_static(super::EVENT_TYPE);
    pub(crate) const EVENT_SCHEMA: HeaderName = HeaderName::from_static(super::EVENT_SCHEMA);
    pub(crate) const CREATED_AT: HeaderName = HeaderName::from_static(super::CREATED_AT);
    pub(crate) const NATS_MSG_ID: HeaderName = HeaderName::from_static(super::NATS_MSG_ID);
    pub(crate) const ORIGINAL_SUBJECT: HeaderName =
        HeaderName::from_static(super::ORIGINAL_SUBJECT);
    pub(crate) const DEAD_LETTER_REASON: HeaderName =
        HeaderName::from_static(super::DEAD_LETTER_REASON);
}

#[derive(Clone, Debug)]
pub struct InboundEnvelope {
    pub(crate) message_id: String,
    pub(crate) event_type: String,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: OffsetDateTime,
    pub(crate) payload: Bytes,
}

/// One stored DLQ record used for deterministic redrive reconstruction.
#[derive(Clone, Debug)]
pub struct DeadLetterRecord {
    pub subject: String,
    pub headers: HeaderMap,
    pub payload: Bytes,
    pub stream: String,
    pub stream_sequence: u64,
    pub stored_at: OffsetDateTime,
}

/// Builds the exact five normal identity headers from an immutable prepared event.
///
/// # Errors
/// Rejects invalid identity, creation time or an oversized encoded header set.
pub fn encode_prepared(event: &PreparedEvent) -> Result<HeaderMap, MessagingError> {
    let created_at = validate_prepared(event)?;
    let mut headers = HeaderMap::new();
    headers.insert(name::MESSAGE_ID, event.message_id());
    headers.insert(name::EVENT_TYPE, event.event_type());
    headers.insert(name::EVENT_SCHEMA, event.schema());
    headers.insert(name::CREATED_AT, created_at);
    headers.insert(name::NATS_MSG_ID, event.publication_id());
    validate_header_bytes(&headers)?;
    Ok(headers)
}

/// Checks that [`encode_prepared`] accepts `event`, without building headers.
/// Returns the formatted creation time.
///
/// Each text value is at most 256 bytes, so the five headers stay far below
/// [`HEADER_LIMIT_BYTES`]; `encode_prepared` still checks the encoded bytes.
pub(crate) fn validate_prepared(event: &PreparedEvent) -> Result<String, MessagingError> {
    for value in [
        event.message_id(),
        event.publication_id(),
        event.event_type(),
    ] {
        validate_text(value)?;
    }
    if event.schema_version() == 0 || is_zero_time(event.occurred_at()) {
        return Err(MessagingError::Envelope("event identity is invalid"));
    }
    format_timestamp(event.occurred_at())
}

/// Decodes the Go envelope before allocating a typed handler payload.
///
/// # Errors
/// Rejects malformed subject, headers, schema or creation time.
pub fn decode_envelope(
    subject: &str,
    headers: &HeaderMap,
    payload: Bytes,
) -> Result<InboundEnvelope, MessagingError> {
    if !valid_subject(subject) {
        return Err(MessagingError::Envelope("subject is invalid"));
    }
    validate_header_bytes(headers)?;
    let message_id = required_header(headers, name::MESSAGE_ID)?;
    let event_type = required_header(headers, name::EVENT_TYPE)?;
    let schema = required_header(headers, name::EVENT_SCHEMA)?;
    let created_at = header_value(headers, name::CREATED_AT);
    let _publication_id = required_header(headers, name::NATS_MSG_ID)?;
    let schema_version = parse_schema(schema)?;
    let occurred_at = parse_timestamp(created_at)?;
    if is_zero_time(occurred_at) {
        return Err(MessagingError::Envelope("creation time is required"));
    }

    Ok(InboundEnvelope {
        message_id: message_id.to_owned(),
        event_type: event_type.to_owned(),
        schema_version,
        occurred_at,
        payload,
    })
}

impl InboundEnvelope {
    #[must_use]
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
    #[must_use]
    pub fn event_type(&self) -> &str {
        &self.event_type
    }
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
    #[must_use]
    pub const fn occurred_at(&self) -> OffsetDateTime {
        self.occurred_at
    }
    #[must_use]
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }
}

/// Reconstructs an original event with Go's deterministic `redrive-` ID.
///
/// # Errors
/// Rejects a record without restorable subject, identity or creation time.
pub fn restore_dead_letter(record: DeadLetterRecord) -> Result<PreparedEvent, MessagingError> {
    let subject = header_value(&record.headers, name::ORIGINAL_SUBJECT).to_owned();
    if !valid_subject(&subject) {
        return Err(MessagingError::Envelope(
            "dead-letter original subject is invalid",
        ));
    }
    let message_id = required_header(&record.headers, name::MESSAGE_ID)?;
    let event_type = required_header(&record.headers, name::EVENT_TYPE)?;
    let schema_version = parse_schema(required_header(&record.headers, name::EVENT_SCHEMA)?)?;
    let occurred_at = parse_timestamp(header_value(&record.headers, name::CREATED_AT))?;
    if is_zero_time(occurred_at) {
        return Err(MessagingError::Envelope(
            "dead-letter creation time is required",
        ));
    }
    // Go hashes an absent original publication ID too. The reconstructed
    // event identity, rather than the transfer metadata, decides restorability.
    let original_publication_id = header_value(&record.headers, name::NATS_MSG_ID);
    let publication_id = record_id(
        "redrive-",
        &record.stream,
        record.stream_sequence,
        record.stored_at,
        original_publication_id,
    )?;
    Ok(PreparedEvent {
        subject,
        message_id: message_id.to_owned(),
        publication_id,
        event_type: event_type.to_owned().into(),
        schema_version,
        occurred_at,
        payload: record.payload,
    })
}

/// Go-compatible deterministic transfer identity for DLQ publication.
///
/// # Errors
/// Rejects a stored timestamp that cannot be normalized to UTC.
pub fn dead_letter_id(
    stream: &str,
    stream_sequence: u64,
    stored_at: OffsetDateTime,
    original_publication_id: &str,
) -> Result<String, MessagingError> {
    record_id(
        "dlq-",
        stream,
        stream_sequence,
        stored_at,
        original_publication_id,
    )
}

pub(crate) fn valid_subject(subject: &str) -> bool {
    let mut previous_dot = true;
    for byte in subject.bytes() {
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' | b'*' | b'>' => return false,
            b'.' if previous_dot => return false,
            b'.' => previous_dot = true,
            _ => previous_dot = false,
        }
    }
    !previous_dot
}

/// Whether the concrete `subject` is selected by the NATS `filter`.
pub(crate) fn subject_matches(filter: &str, subject: &str) -> bool {
    if !valid_subject(subject) {
        return false;
    }
    let mut filter_tokens = filter.split('.');
    let mut subject_tokens = subject.split('.');
    loop {
        match (filter_tokens.next(), subject_tokens.next()) {
            (Some(">"), Some(_)) | (None, None) => return true,
            (Some("*"), Some(_)) => {}
            (Some(expected), Some(actual)) if expected == actual => {}
            _ => return false,
        }
    }
}

pub(crate) fn header_value(headers: &HeaderMap, name: HeaderName) -> &str {
    headers.get(name).map_or("", |value| value.as_str())
}

fn required_header(headers: &HeaderMap, name: HeaderName) -> Result<&str, MessagingError> {
    let value = headers
        .get(name)
        .map(async_nats::HeaderValue::as_str)
        .ok_or(MessagingError::Envelope("required header is missing"))?;
    validate_text(value)?;
    Ok(value)
}

fn parse_schema(value: &str) -> Result<u16, MessagingError> {
    let Some(raw) = value.strip_prefix('v') else {
        return Err(MessagingError::Envelope("event schema is invalid"));
    };
    let version = raw
        .parse::<u16>()
        .map_err(|_| MessagingError::Envelope("event schema is invalid"))?;
    if version == 0 || raw.starts_with('0') || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MessagingError::Envelope("event schema is invalid"));
    }
    Ok(version)
}

pub(crate) fn validate_text(value: &str) -> Result<(), MessagingError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(MessagingError::Envelope("header identity is invalid"));
    }
    Ok(())
}

pub(crate) fn encoded_header_bytes(headers: &HeaderMap) -> usize {
    if headers.is_empty() {
        return 0;
    }
    headers
        .iter()
        .map(|(name, values)| {
            values
                .iter()
                .map(|value| AsRef::<str>::as_ref(name).len() + 2 + value.as_str().len() + 2)
                .sum::<usize>()
        })
        .sum::<usize>()
        + "NATS/1.0\r\n\r\n".len()
}

fn validate_header_bytes(headers: &HeaderMap) -> Result<(), MessagingError> {
    if encoded_header_bytes(headers) > HEADER_LIMIT_BYTES {
        return Err(MessagingError::Envelope("encoded headers exceed 8 KiB"));
    }
    Ok(())
}

fn format_timestamp(value: OffsetDateTime) -> Result<String, MessagingError> {
    value
        .checked_to_offset(UtcOffset::UTC)
        .and_then(|value| value.format(&Rfc3339).ok())
        .ok_or(MessagingError::Envelope("creation time is out of range"))
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, MessagingError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|value| value.checked_to_offset(UtcOffset::UTC))
        .ok_or(MessagingError::Envelope("creation time is invalid"))
}

fn record_id(
    prefix: &str,
    stream: &str,
    stream_sequence: u64,
    stored_at: OffsetDateTime,
    publication_id: &str,
) -> Result<String, MessagingError> {
    let stored_at = format_timestamp(stored_at)?;
    let sequence = stream_sequence.to_string();
    let mut hash = Sha256::new();
    hash.update(stream.as_bytes());
    hash.update([0]);
    hash.update(sequence.as_bytes());
    hash.update([0]);
    hash.update(stored_at.as_bytes());
    hash.update([0]);
    hash.update(publication_id.as_bytes());
    Ok(prefixed_digest_hex(prefix, &hash.finalize()))
}

/// `prefix` followed by the lowercase hex of a SHA-256 digest.
pub(crate) fn prefixed_digest_hex(prefix: &str, digest: &[u8]) -> String {
    let mut id = String::with_capacity(prefix.len() + 64);
    id.push_str(prefix);
    for &byte in digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 15)]));
    }
    id
}

fn is_zero_time(value: OffsetDateTime) -> bool {
    value.year() == 1 && value.ordinal() == 1 && value.time() == time::Time::MIDNIGHT
}
