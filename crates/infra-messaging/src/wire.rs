//! Go-compatible NATS envelope headers and bounded decoding.

use async_nats::HeaderMap;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

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
    let mut headers = HeaderMap::new();
    headers.insert(MESSAGE_ID, event.message_id());
    headers.insert(EVENT_TYPE, event.event_type());
    headers.insert(EVENT_SCHEMA, event.schema());
    headers.insert(CREATED_AT, format_timestamp(event.occurred_at())?);
    headers.insert(NATS_MSG_ID, event.publication_id());
    validate_header_bytes(&headers)?;
    Ok(headers)
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
    let message_id = required_header(headers, MESSAGE_ID)?;
    let event_type = required_header(headers, EVENT_TYPE)?;
    let schema = required_header(headers, EVENT_SCHEMA)?;
    let created_at = header_value(headers, CREATED_AT);
    let _publication_id = required_header(headers, NATS_MSG_ID)?;
    let schema_version = parse_schema(&schema)?;
    let occurred_at = parse_timestamp(created_at)?;
    if is_zero_time(occurred_at) {
        return Err(MessagingError::Envelope("creation time is required"));
    }

    Ok(InboundEnvelope {
        message_id,
        event_type,
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
    let subject = header_value(&record.headers, ORIGINAL_SUBJECT).to_owned();
    if !valid_subject(&subject) {
        return Err(MessagingError::Envelope(
            "dead-letter original subject is invalid",
        ));
    }
    let message_id = required_header(&record.headers, MESSAGE_ID)?;
    let event_type = required_header(&record.headers, EVENT_TYPE)?;
    let schema_version = parse_schema(&required_header(&record.headers, EVENT_SCHEMA)?)?;
    let occurred_at = parse_timestamp(header_value(&record.headers, CREATED_AT))?;
    if is_zero_time(occurred_at) {
        return Err(MessagingError::Envelope(
            "dead-letter creation time is required",
        ));
    }
    // Go hashes an absent original publication ID too. The reconstructed
    // event identity, rather than the transfer metadata, decides restorability.
    let original_publication_id = header_value(&record.headers, NATS_MSG_ID);
    let publication_id = record_id(
        "redrive-",
        &record.stream,
        record.stream_sequence,
        record.stored_at,
        original_publication_id,
    )?;
    Ok(PreparedEvent {
        subject,
        message_id,
        publication_id,
        event_type,
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
    !subject.is_empty()
        && !subject.split('.').any(|token| {
            token.is_empty()
                || token
                    .bytes()
                    .any(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                || token.contains('*')
                || token.contains('>')
        })
}

pub(crate) fn valid_filter(filter: &str) -> bool {
    let mut tokens = filter.split('.').peekable();
    while let Some(token) = tokens.next() {
        if token == ">" {
            return tokens.peek().is_none();
        }
        if token != "*" && !valid_subject(token) {
            return false;
        }
    }
    !filter.is_empty()
}

/// Whether every subject selected by `filter` is included by `pattern`.
pub(crate) fn filter_covers(pattern: &str, filter: &str) -> bool {
    let mut source = pattern.split('.');
    let mut selected = filter.split('.');
    loop {
        match (source.next(), selected.next()) {
            (Some(">"), Some(_)) | (None, None) => return true,
            (Some("*"), Some(token)) if token != ">" => {}
            (Some(left), Some(right)) if left == right => {}
            _ => return false,
        }
    }
}

pub(crate) fn subject_matches(filter: &str, subject: &str) -> bool {
    valid_subject(subject) && filter_covers(filter, subject)
}

pub(crate) fn header_value<'a>(headers: &'a HeaderMap, name: &'static str) -> &'a str {
    headers.get(name).map_or("", |value| value.as_str())
}

fn required_header(headers: &HeaderMap, name: &'static str) -> Result<String, MessagingError> {
    let value = headers
        .get(name)
        .map(ToString::to_string)
        .ok_or(MessagingError::Envelope("required header is missing"))?;
    validate_text(&value)?;
    Ok(value)
}

fn parse_schema(value: &str) -> Result<u16, MessagingError> {
    let Some(raw) = value.strip_prefix('v') else {
        return Err(MessagingError::Envelope("event schema is invalid"));
    };
    let version = raw
        .parse::<u16>()
        .map_err(|_| MessagingError::Envelope("event schema is invalid"))?;
    if version == 0 || format!("v{version}") != value {
        return Err(MessagingError::Envelope("event schema is invalid"));
    }
    Ok(version)
}

fn validate_text(value: &str) -> Result<(), MessagingError> {
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
                .map(|value| name.to_string().len() + 2 + value.to_string().len() + 2)
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

pub(crate) fn validate_encoded_message(
    subject: &str,
    headers: &HeaderMap,
    payload_bytes: usize,
    max_payload_bytes: usize,
) -> Result<(), MessagingError> {
    validate_header_bytes(headers)?;
    if payload_bytes > max_payload_bytes
        || subject
            .len()
            .saturating_add(encoded_header_bytes(headers))
            .saturating_add(payload_bytes)
            > max_payload_bytes.saturating_add(HEADER_LIMIT_BYTES)
    {
        return Err(MessagingError::Bounds);
    }
    Ok(())
}

fn format_timestamp(value: OffsetDateTime) -> Result<String, MessagingError> {
    let value = value
        .checked_to_offset(time::UtcOffset::UTC)
        .ok_or(MessagingError::Envelope("creation time is out of range"))?;
    // Go formats even a normalized UTC year outside RFC3339's four-digit
    // parse range. Preserve that asymmetry rather than panicking or truncating.
    let year = if value.year() < 0 {
        format!("-{:04}", value.year().unsigned_abs())
    } else {
        format!("{:04}", value.year())
    };
    let mut encoded = format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}",
        u8::from(value.month()),
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
    );
    if value.nanosecond() != 0 {
        let fraction = format!("{:09}", value.nanosecond());
        encoded.push('.');
        encoded.push_str(fraction.trim_end_matches('0'));
    }
    encoded.push('Z');
    Ok(encoded)
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, MessagingError> {
    let invalid = || MessagingError::Envelope("creation time is invalid");
    if !value.is_ascii() || value.as_bytes().get(10) != Some(&b'T') {
        return Err(invalid());
    }
    // Go's RFC3339Nano fallback accepts a one-digit hour and comma fractions.
    // Normalize only those spellings; time remains the date/time parser.
    let mut normalized = value.replace(',', ".");
    if normalized.as_bytes().get(12) == Some(&b':') {
        normalized.insert(11, '0');
    }
    if normalized.get(17..19) == Some("60") {
        return Err(invalid()); // Go rejects leap seconds; time accepts them.
    }
    let offset_seconds = if normalized.ends_with('Z') {
        0
    } else {
        let start = normalized.len().checked_sub(6).ok_or_else(invalid)?;
        let suffix = normalized.get(start..).ok_or_else(invalid)?;
        if !matches!(suffix.as_bytes()[0], b'+' | b'-')
            || suffix.as_bytes()[3] != b':'
            || !suffix.as_bytes()[1..3].iter().all(u8::is_ascii_digit)
            || !suffix.as_bytes()[4..6].iter().all(u8::is_ascii_digit)
        {
            return Err(invalid());
        }
        let hour: i64 = suffix[1..3].parse().map_err(|_| invalid())?;
        let minute: i64 = suffix[4..6].parse().map_err(|_| invalid())?;
        if hour > 24 || minute > 60 {
            return Err(invalid());
        }
        let sign = if suffix.starts_with('-') { -1 } else { 1 };
        let seconds = sign * (hour * 3600 + minute * 60);
        normalized.truncate(start);
        normalized.push('Z');
        seconds
    };
    OffsetDateTime::parse(&normalized, &Rfc3339)
        .map_err(|_| invalid())?
        .checked_sub(time::Duration::seconds(offset_seconds))
        .ok_or_else(invalid)
}

fn record_id(
    prefix: &str,
    stream: &str,
    stream_sequence: u64,
    stored_at: OffsetDateTime,
    publication_id: &str,
) -> Result<String, MessagingError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let stored_at = format_timestamp(stored_at)?;
    let sequence = stream_sequence.to_string();
    let input = [
        stream,
        sequence.as_str(),
        stored_at.as_str(),
        publication_id,
    ]
    .join("\0");
    let digest = Sha256::digest(input.as_bytes());
    let mut id = String::with_capacity(prefix.len() + digest.len() * 2);
    id.push_str(prefix);
    for byte in digest.iter().copied() {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(id)
}

fn is_zero_time(value: OffsetDateTime) -> bool {
    value.year() == 1 && value.ordinal() == 1 && value.time() == time::Time::MIDNIGHT
}
