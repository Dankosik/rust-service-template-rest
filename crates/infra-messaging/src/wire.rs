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

/// NATS enforces this encoded-header ceiling independently of message size.
pub(crate) const NATIVE_HEADER_LIMIT_BYTES: usize = 65_535;
const DEAD_LETTER_ID_PREFIX: &str = "dlq-";

#[derive(Clone, Copy)]
pub(crate) enum DeadLetterReason {
    Malformed,
    Permanent,
    Exhausted,
}

impl DeadLetterReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Permanent => "permanent",
            Self::Exhausted => "exhausted",
        }
    }
}

pub(crate) struct DeadLetterBounds {
    pub(crate) total: usize,
    pub(crate) headers: usize,
}

/// NATS 2.15 counts the original subject and ACK reply in a pull's byte limit.
/// V2 ACKs cover V1 too: prefix, domain, eight-byte account hash, stream,
/// consumer, then five 64-bit decimal fields (including a signed timestamp).
pub(crate) fn pull_delivery_bytes(
    payload_bytes: usize,
    subject_bytes: usize,
    stream_bytes: usize,
    consumer_bytes: usize,
    domain_bytes: usize,
) -> Option<usize> {
    // Three separators between names, then a separator and at most 20 bytes
    // for each numeric field. An absent domain is represented by "_".
    let ack_fixed_bytes = "$JS.ACK.".len() + 8 + 3 + 5 * 21;
    [
        HEADER_LIMIT_BYTES,
        subject_bytes,
        stream_bytes,
        consumer_bytes,
        domain_bytes.max(1),
        ack_fixed_bytes,
    ]
    .into_iter()
    .try_fold(payload_bytes, usize::checked_add)
}

/// Bounds a supported normal transfer using source total bytes and known routes.
/// The source already includes header framing; replacement fields are counted
/// again so the bound does not depend on a minimum original envelope size.
pub(crate) fn dead_letter_bounds(
    source_bytes: usize,
    subject_bytes: usize,
    stream_bytes: usize,
) -> Option<DeadLetterBounds> {
    let reason_bytes = [
        DeadLetterReason::Malformed,
        DeadLetterReason::Permanent,
        DeadLetterReason::Exhausted,
    ]
    .into_iter()
    .map(|reason| reason.as_str().len())
    .max()?;
    let expected_stream = async_nats::header::NATS_EXPECTED_STREAM;
    let additions = [
        (NATS_MSG_ID, DEAD_LETTER_ID_PREFIX.len().checked_add(64)?),
        (ORIGINAL_SUBJECT, subject_bytes),
        (DEAD_LETTER_REASON, reason_bytes),
        (expected_stream.as_ref(), stream_bytes),
    ]
    .into_iter()
    .try_fold(0_usize, |bytes, (name, value_bytes)| {
        bytes
            .checked_add(name.len())?
            .checked_add(4)?
            .checked_add(value_bytes)
    })?;
    Some(DeadLetterBounds {
        total: source_bytes.checked_add(additions)?,
        headers: source_bytes
            .min(HEADER_LIMIT_BYTES)
            .checked_add(additions)?,
    })
}

/// Keeps first identity/trace values and appends the metadata admission budgets.
pub(crate) fn dead_letter_headers(
    original: &HeaderMap,
    subject: &str,
    stream: &str,
    transfer_id: &str,
    reason: DeadLetterReason,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for name in [
        name::MESSAGE_ID,
        name::EVENT_TYPE,
        name::EVENT_SCHEMA,
        name::CREATED_AT,
        crate::trace::TRACEPARENT,
        crate::trace::TRACESTATE,
    ] {
        let value = header_value(original, name.clone());
        if !value.is_empty() {
            headers.insert(name, value);
        }
    }
    if header_value(&headers, name::MESSAGE_ID).is_empty() {
        headers.insert(name::MESSAGE_ID, transfer_id);
    }
    headers.insert(name::NATS_MSG_ID, transfer_id);
    headers.insert(name::ORIGINAL_SUBJECT, subject);
    headers.insert(name::DEAD_LETTER_REASON, reason.as_str());
    headers.insert(async_nats::header::NATS_EXPECTED_STREAM, stream);
    headers
}

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
/// Every constructor of [`PreparedEvent`] has already validated its identity
/// and formatted its creation time.
///
/// # Errors
/// Rejects an oversized encoded header set.
pub fn encode_prepared(event: &PreparedEvent) -> Result<HeaderMap, MessagingError> {
    let mut headers = HeaderMap::new();
    headers.insert(name::MESSAGE_ID, event.message_id());
    headers.insert(name::EVENT_TYPE, event.event_type());
    headers.insert(name::EVENT_SCHEMA, event.schema());
    headers.insert(name::CREATED_AT, event.created_at.as_str());
    headers.insert(name::NATS_MSG_ID, event.publication_id());
    validate_header_bytes(&headers)?;
    Ok(headers)
}

// template:begin outbox:messaging-outbox-validate-prepared
/// Checks the identity every [`PreparedEvent`] carries. Returns the formatted
/// creation time.
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
// template:end outbox:messaging-outbox-validate-prepared

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
    // One pass reads the five identity headers and sums the encoded size;
    // each `HeaderMap::get` would hash the name with SipHash.
    let mut encoded = "NATS/1.0\r\n\r\n".len();
    let mut found: [Option<&str>; 5] = [None; 5];
    for (header, values) in headers.iter() {
        let header: &str = header.as_ref();
        for value in values {
            encoded += header.len() + 2 + value.as_str().len() + 2;
        }
        let slot = match header {
            MESSAGE_ID => 0,
            EVENT_TYPE => 1,
            EVENT_SCHEMA => 2,
            CREATED_AT => 3,
            NATS_MSG_ID => 4,
            _ => continue,
        };
        found[slot] = values.first().map(async_nats::HeaderValue::as_str);
    }
    if encoded > HEADER_LIMIT_BYTES {
        return Err(MessagingError::Envelope("encoded headers exceed 8 KiB"));
    }
    let [message_id, event_type, schema, created_at, publication_id] = found;
    let message_id = required_value(message_id)?;
    let event_type = required_value(event_type)?;
    let schema = required_value(schema)?;
    let created_at = created_at.unwrap_or("");
    let _publication_id = required_value(publication_id)?;
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
        created_at: format_timestamp(occurred_at)?,
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
        DEAD_LETTER_ID_PREFIX,
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

fn required_value(value: Option<&str>) -> Result<&str, MessagingError> {
    let value = value.ok_or(MessagingError::Envelope("required header is missing"))?;
    validate_text(value)?;
    Ok(value)
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
    if !valid_text(value) {
        return Err(MessagingError::Envelope("header identity is invalid"));
    }
    Ok(())
}

/// Whether `value` is 1..=256 bytes without a control character. `Cc` is
/// U+0000..=U+001F and U+007F..=U+009F; in UTF-8 the last range is 0x7F or
/// 0xC2 followed by 0x80..=0x9F, so no character needs decoding.
pub(crate) const fn valid_text(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 256 {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x20 || byte == 0x7f || (byte == 0xc2 && bytes[index + 1] < 0xa0) {
            return false;
        }
        index += 1;
    }
    true
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

pub(crate) fn format_timestamp(value: OffsetDateTime) -> Result<String, MessagingError> {
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

pub(crate) fn is_zero_time(value: OffsetDateTime) -> bool {
    value.year() == 1 && value.ordinal() == 1 && value.time() == time::Time::MIDNIGHT
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed valid fixtures")]
mod tests {
    use super::*;

    #[test]
    fn pull_budget_covers_native_ack_metadata_and_checks_overflow() {
        let subject = "x".repeat(57_000);
        let source = "SOURCE";
        let consumer = "handler";
        let maximum = u64::MAX;
        let timestamp = i64::MIN;
        let v1 = format!(
            "$JS.ACK.{source}.{consumer}.{maximum}.{maximum}.{maximum}.{timestamp}.{maximum}"
        );
        for domain in [String::new(), "domain".repeat(1000)] {
            let ack_domain = if domain.is_empty() { "_" } else { &domain };
            let v2 = format!(
                "$JS.ACK.{ack_domain}.01234567.{source}.{consumer}.{maximum}.{maximum}.{maximum}.{timestamp}.{maximum}"
            );
            let bound = pull_delivery_bytes(
                1024,
                subject.len(),
                source.len(),
                consumer.len(),
                domain.len(),
            )
            .unwrap();
            assert_eq!(bound, 1024 + HEADER_LIMIT_BYTES + subject.len() + v2.len());
            assert!(1024 + HEADER_LIMIT_BYTES + subject.len() + v1.len() <= bound);
        }
        for [payload, subject, stream, consumer, domain] in [
            [usize::MAX, 1, 1, 1, 1],
            [1, usize::MAX, 1, 1, 1],
            [1, 1, usize::MAX, 1, 1],
            [1, 1, 1, usize::MAX, 1],
            [1, 1, 1, 1, usize::MAX],
        ] {
            assert!(pull_delivery_bytes(payload, subject, stream, consumer, domain).is_none());
        }
    }

    #[test]
    fn normal_transfer_bounds_cover_retained_trace_bytes_and_all_metadata() {
        for subject in ["orders.created".to_owned(), "x".repeat(57_000)] {
            let mut original = envelope_headers();
            original.insert(
                crate::trace::TRACEPARENT,
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            );
            original.insert(crate::trace::TRACESTATE, "");
            let trace = "x".repeat(HEADER_LIMIT_BYTES - encoded_header_bytes(&original));
            original.insert(crate::trace::TRACESTATE, trace.as_str());
            assert_eq!(encoded_header_bytes(&original), HEADER_LIMIT_BYTES);
            let payload = Bytes::from_static(b"{\"quantity\":1}");
            decode_envelope(&subject, &original, payload.clone()).unwrap();
            let transfer_id =
                dead_letter_id("SOURCE", 1, OffsetDateTime::UNIX_EPOCH, "event-1").unwrap();
            let source_bytes = encoded_header_bytes(&original) + payload.len();
            let bounds =
                dead_letter_bounds(source_bytes, subject.len(), "ACTUAL_DLQ".len()).unwrap();
            for reason in [
                DeadLetterReason::Malformed,
                DeadLetterReason::Permanent,
                DeadLetterReason::Exhausted,
            ] {
                let transferred =
                    dead_letter_headers(&original, &subject, "ACTUAL_DLQ", &transfer_id, reason);
                assert!(encoded_header_bytes(&transferred) <= bounds.headers);
                assert!(encoded_header_bytes(&transferred) + payload.len() <= bounds.total);
                assert_eq!(
                    transferred.get("Original-Subject").unwrap().as_str(),
                    subject
                );
                assert_eq!(
                    transferred.get("Nats-Expected-Stream").unwrap().as_str(),
                    "ACTUAL_DLQ"
                );
                assert_eq!(transferred.get("tracestate").unwrap().as_str(), trace);
            }
        }
        // A small stream also bounds the source headers below the adapter cap.
        let bounds = dead_letter_bounds(128, 14, 10).unwrap();
        assert_eq!(bounds.headers, bounds.total);
    }

    #[test]
    fn transfer_size_overflow_is_not_an_admissible_bound() {
        for (source, subject, stream) in
            [(usize::MAX, 1, 1), (1, usize::MAX, 1), (1, 1, usize::MAX)]
        {
            assert!(dead_letter_bounds(source, subject, stream).is_none());
        }
    }

    #[test]
    fn header_text_rejects_exactly_the_unicode_control_characters() {
        for control in ['\0', '\u{1f}', '\u{7f}', '\u{80}', '\u{85}', '\u{9f}'] {
            assert!(!valid_text(&format!("id{control}x")), "{control:?}");
        }
        for allowed in ["\u{a0}", "caf\u{e9}", "\u{2028}", "\u{1F600}", "~"] {
            assert!(valid_text(allowed), "{allowed:?}");
        }
        assert!(!valid_text(""));
        assert!(valid_text(&"x".repeat(256)));
        assert!(!valid_text(&"x".repeat(257)));
    }

    fn envelope_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(name::MESSAGE_ID, "event-1");
        headers.insert(name::EVENT_TYPE, "order.created");
        headers.insert(name::EVENT_SCHEMA, "v2");
        headers.insert(name::CREATED_AT, "2026-09-29T10:00:00Z");
        headers.insert(name::NATS_MSG_ID, "event-1");
        headers
    }

    #[test]
    fn decode_reads_the_first_value_of_each_identity_header_and_ignores_others() {
        let mut headers = envelope_headers();
        headers.append(name::MESSAGE_ID, "event-2");
        headers.insert(
            "Traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        );
        let envelope = decode_envelope("orders.created", &headers, Bytes::new()).unwrap();
        assert_eq!(envelope.message_id(), "event-1");
        assert_eq!(envelope.event_type(), "order.created");
        assert_eq!(envelope.schema_version(), 2);
    }

    #[test]
    fn decode_rejects_a_missing_or_invalid_identity_header() {
        for missing in [
            MESSAGE_ID,
            EVENT_TYPE,
            EVENT_SCHEMA,
            CREATED_AT,
            NATS_MSG_ID,
        ] {
            let headers: HeaderMap = envelope_headers()
                .iter()
                .filter(|(name, _)| AsRef::<str>::as_ref(*name) != missing)
                .map(|(name, values)| (name.clone(), values[0].clone()))
                .collect();
            assert!(
                decode_envelope("orders.created", &headers, Bytes::new()).is_err(),
                "{missing}"
            );
        }
        let mut headers = envelope_headers();
        headers.insert(name::EVENT_TYPE, "order\u{85}created");
        assert!(decode_envelope("orders.created", &headers, Bytes::new()).is_err());
    }
}
