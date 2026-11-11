//! Go-compatible NATS envelope headers and bounded decoding.

use async_nats::{HeaderMap, HeaderName};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::error::EnvelopeError;
use crate::prepared::PreparedEvent;

pub const HEADER_LIMIT_BYTES: usize = 8 * 1024;
pub const MESSAGE_ID: &str = "Message-Id";
pub const EVENT_TYPE: &str = "Event-Type";
pub const EVENT_SCHEMA: &str = "Event-Schema";
pub const CREATED_AT: &str = "Created-At";
/// Header the broker reads to deduplicate publications.
pub const NATS_MSG_ID: &str = "Nats-Msg-Id";
pub const ORIGINAL_SUBJECT: &str = "Original-Subject";
pub const DEAD_LETTER_REASON: &str = "Dead-Letter-Reason";

/// NATS enforces this encoded-header ceiling independently of message size.
pub(crate) const NATIVE_HEADER_LIMIT_BYTES: usize = 65_535;
/// The longest identity header value: message and publication ID, event type.
pub(crate) const IDENTITY_MAX_BYTES: usize = 256;
const DEAD_LETTER_ID_PREFIX: &str = "dlq-";
/// Hex digits of a SHA-256 digest.
const DIGEST_HEX_LEN: usize = 64;
/// The `NATS/1.0` status line and the blank line that ends a header block.
const HEADER_PREAMBLE_BYTES: usize = "NATS/1.0\r\n\r\n".len();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeadLetterReason {
    Malformed,
    Permanent,
    Exhausted,
}

impl DeadLetterReason {
    pub(crate) const ALL: [Self; 3] = [Self::Malformed, Self::Permanent, Self::Exhausted];

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
    stream: &str,
    consumer: &str,
    domain: Option<&str>,
) -> Option<usize> {
    // Three separators between names, then a separator and at most 20 bytes
    // for each numeric field. An absent or empty domain is written as "_".
    let ack_fixed_bytes = "$JS.ACK.".len() + 8 + 3 + 5 * 21;
    [
        HEADER_LIMIT_BYTES,
        subject_bytes,
        stream.len(),
        consumer.len(),
        domain.map_or(0, str::len).max(1),
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
    let reason_bytes = DeadLetterReason::ALL
        .into_iter()
        .map(|reason| reason.as_str().len())
        .max()?;
    let expected_stream = async_nats::header::NATS_EXPECTED_STREAM;
    let additions = [
        (NATS_MSG_ID, DEAD_LETTER_ID_PREFIX.len() + DIGEST_HEX_LEN),
        (ORIGINAL_SUBJECT, subject_bytes),
        (DEAD_LETTER_REASON, reason_bytes),
        (expected_stream.as_ref(), stream_bytes),
    ]
    .into_iter()
    .try_fold(0_usize, |bytes, (name, value_bytes)| {
        bytes.checked_add(header_entry_bytes(name, value_bytes)?)
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
    /// The subject of the stored DLQ record. [`restore_dead_letter`] reads the
    /// original subject from `Original-Subject`; this field is provenance for
    /// the record itself.
    pub subject: String,
    pub headers: HeaderMap,
    pub payload: Bytes,
    pub stream: String,
    pub stream_sequence: u64,
    pub stored_at: OffsetDateTime,
}

/// Builds the exact five normal identity headers from an immutable prepared event.
///
/// Every [`PreparedEvent`] a caller can hold has a validated identity and a
/// creation time that can be formatted.
///
/// # Errors
/// Rejects an oversized encoded header set or a creation time that cannot be
/// formatted.
pub fn encode_prepared(event: &PreparedEvent) -> Result<HeaderMap, EnvelopeError> {
    let mut headers = HeaderMap::new();
    headers.insert(name::MESSAGE_ID, event.message_id());
    headers.insert(name::EVENT_TYPE, event.event_type());
    headers.insert(name::EVENT_SCHEMA, event.schema_label());
    let created_at = format_timestamp(event.occurred_at())?;
    headers.insert(name::CREATED_AT, created_at.as_str());
    headers.insert(name::NATS_MSG_ID, event.publication_id());
    validate_header_bytes(&headers)?;
    Ok(headers)
}

// template:begin outbox:messaging-outbox-validate-prepared
/// Checks the identity and creation time every [`PreparedEvent`] carries.
///
/// Each text value is at most [`IDENTITY_MAX_BYTES`], so the five headers stay far below
/// [`HEADER_LIMIT_BYTES`]; `encode_prepared` still checks the encoded bytes.
pub(crate) fn validate_prepared(event: &PreparedEvent) -> Result<(), EnvelopeError> {
    for value in [
        event.message_id(),
        event.publication_id(),
        event.event_type(),
    ] {
        validate_identity(value)?;
    }
    if event.schema_version() == 0 || is_zero_time(event.occurred_at()) {
        return Err(EnvelopeError::Identity);
    }
    format_timestamp(event.occurred_at())?;
    Ok(())
}
// template:end outbox:messaging-outbox-validate-prepared

/// Decodes the Go envelope before allocating a typed handler payload.
///
/// Requires `Message-Id`, `Event-Type`, `Event-Schema`, `Created-At` and
/// `Nats-Msg-Id`. `Nats-Msg-Id` is validated and not returned; the logical
/// identity is `Message-Id`.
///
/// # Errors
/// Rejects a malformed subject, a missing or invalid required header, an
/// invalid schema, or a creation time that is zero or cannot be parsed.
pub fn decode_envelope(
    subject: &str,
    headers: &HeaderMap,
    payload: Bytes,
) -> Result<InboundEnvelope, EnvelopeError> {
    if !valid_subject(subject) {
        return Err(EnvelopeError::Subject);
    }
    // One pass reads the five identity headers and sums the encoded size;
    // each `HeaderMap::get` would hash the name with SipHash.
    let mut encoded = HEADER_PREAMBLE_BYTES;
    let mut found = Found::default();
    for (header, values) in headers.iter() {
        let header: &str = header.as_ref();
        for value in values {
            encoded += header_line_bytes(header, value.as_str());
        }
        let slot = match header {
            MESSAGE_ID => &mut found.message_id,
            EVENT_TYPE => &mut found.event_type,
            EVENT_SCHEMA => &mut found.schema,
            CREATED_AT => &mut found.created_at,
            NATS_MSG_ID => &mut found.publication_id,
            _ => continue,
        };
        *slot = values.first().map(async_nats::HeaderValue::as_str);
    }
    if encoded > HEADER_LIMIT_BYTES {
        return Err(EnvelopeError::HeadersTooLarge);
    }
    let message_id = required_value(found.message_id, MESSAGE_ID)?;
    let event_type = required_value(found.event_type, EVENT_TYPE)?;
    let schema = required_value(found.schema, EVENT_SCHEMA)?;
    let created_at = required_value(found.created_at, CREATED_AT)?;
    let _publication_id = required_value(found.publication_id, NATS_MSG_ID)?;
    let schema_version = parse_schema(schema)?;
    let occurred_at = parse_timestamp(created_at)?;
    if is_zero_time(occurred_at) {
        return Err(EnvelopeError::CreatedAtRequired);
    }

    Ok(InboundEnvelope {
        message_id: message_id.to_owned(),
        event_type: event_type.to_owned(),
        schema_version,
        occurred_at,
        payload,
    })
}

/// The first value of each identity header.
#[derive(Default)]
struct Found<'a> {
    message_id: Option<&'a str>,
    event_type: Option<&'a str>,
    schema: Option<&'a str>,
    created_at: Option<&'a str>,
    publication_id: Option<&'a str>,
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
pub fn restore_dead_letter(record: DeadLetterRecord) -> Result<PreparedEvent, EnvelopeError> {
    let subject = header_value(&record.headers, name::ORIGINAL_SUBJECT).to_owned();
    if !valid_subject(&subject) {
        return Err(EnvelopeError::DeadLetterSubject);
    }
    let message_id = required_header(&record.headers, name::MESSAGE_ID, MESSAGE_ID)?;
    // Transfer keeps Go's wire and fills an absent `Message-Id` with the
    // transfer ID, which is also the record's `Nats-Msg-Id`. That value names
    // the transfer, not the event, so republishing under it would invent a
    // logical identity.
    if message_id == header_value(&record.headers, name::NATS_MSG_ID) {
        return Err(EnvelopeError::DeadLetterMessageIdMissing);
    }
    let event_type = required_header(&record.headers, name::EVENT_TYPE, EVENT_TYPE)?;
    let schema_version = parse_schema(required_header(
        &record.headers,
        name::EVENT_SCHEMA,
        EVENT_SCHEMA,
    )?)?;
    let occurred_at = parse_timestamp(required_header(
        &record.headers,
        name::CREATED_AT,
        CREATED_AT,
    )?)?;
    if is_zero_time(occurred_at) {
        return Err(EnvelopeError::DeadLetterCreatedAtRequired);
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
    // The prepared event keeps `occurred_at`; the created-at text is formatted
    // when the envelope is encoded.
    format_timestamp(occurred_at)?;
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
) -> Result<String, EnvelopeError> {
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
pub(crate) fn matches_filter(filter: &str, subject: &str) -> bool {
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

fn required_value<'a>(
    value: Option<&'a str>,
    label: &'static str,
) -> Result<&'a str, EnvelopeError> {
    let value = value.ok_or(EnvelopeError::MissingHeader(label))?;
    validate_identity(value)?;
    Ok(value)
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: HeaderName,
    label: &'static str,
) -> Result<&'a str, EnvelopeError> {
    required_value(
        headers.get(name).map(async_nats::HeaderValue::as_str),
        label,
    )
}

/// The `Event-Schema` header value of `version`.
pub(crate) fn schema_label(version: u16) -> String {
    format!("v{version}")
}

/// Reads a [`schema_label`]: `v` and a positive decimal without leading zeros.
fn parse_schema(value: &str) -> Result<u16, EnvelopeError> {
    value
        .strip_prefix('v')
        .filter(|raw| !raw.starts_with('0') && raw.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|raw| raw.parse().ok())
        .ok_or(EnvelopeError::Schema)
}

pub(crate) fn validate_identity(value: &str) -> Result<(), EnvelopeError> {
    if !valid_identity(value) {
        return Err(EnvelopeError::Identity);
    }
    Ok(())
}

/// Whether `value` is 1..=[`IDENTITY_MAX_BYTES`] bytes without a control character. `Cc` is
/// U+0000..=U+001F and U+007F..=U+009F; in UTF-8 the last range is 0x7F or
/// 0xC2 followed by 0x80..=0x9F, so no character needs decoding.
pub(crate) const fn valid_identity(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > IDENTITY_MAX_BYTES {
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

/// The `: ` after a header name and the line break after its value.
const HEADER_ENTRY_FRAMING_BYTES: usize = ": \r\n".len();

/// Encoded bytes of one `name: value` header line.
fn header_line_bytes(name: &str, value: &str) -> usize {
    name.len() + value.len() + HEADER_ENTRY_FRAMING_BYTES
}

/// [`header_line_bytes`] for a value of `value_bytes`, `None` on overflow.
fn header_entry_bytes(name: &str, value_bytes: usize) -> Option<usize> {
    name.len()
        .checked_add(HEADER_ENTRY_FRAMING_BYTES)?
        .checked_add(value_bytes)
}

pub(crate) fn encoded_header_bytes(headers: &HeaderMap) -> usize {
    if headers.is_empty() {
        return 0;
    }
    headers
        .iter()
        .flat_map(|(name, values)| {
            values
                .iter()
                .map(move |value| header_line_bytes(name.as_ref(), value.as_str()))
        })
        .sum::<usize>()
        + HEADER_PREAMBLE_BYTES
}

pub(crate) fn validate_header_bytes(headers: &HeaderMap) -> Result<(), EnvelopeError> {
    if encoded_header_bytes(headers) > HEADER_LIMIT_BYTES {
        return Err(EnvelopeError::HeadersTooLarge);
    }
    Ok(())
}

pub(crate) fn format_timestamp(value: OffsetDateTime) -> Result<String, EnvelopeError> {
    value
        .checked_to_offset(UtcOffset::UTC)
        .and_then(|value| value.format(&Rfc3339).ok())
        .ok_or(EnvelopeError::CreatedAtOutOfRange)
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, EnvelopeError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|value| value.checked_to_offset(UtcOffset::UTC))
        .ok_or(EnvelopeError::CreatedAtInvalid)
}

fn record_id(
    prefix: &str,
    stream: &str,
    stream_sequence: u64,
    stored_at: OffsetDateTime,
    publication_id: &str,
) -> Result<String, EnvelopeError> {
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
#[must_use]
pub fn prefixed_digest_hex(prefix: &str, digest: &[u8]) -> String {
    let mut id = String::with_capacity(prefix.len() + DIGEST_HEX_LEN);
    id.push_str(prefix);
    for &byte in digest {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 15)]));
    }
    id
}

/// Whether `value` is Go's zero `time.Time{}`, January 1 of year 1 UTC, which
/// a Go publisher writes for an unset creation time.
pub(crate) fn is_zero_time(value: OffsetDateTime) -> bool {
    value.year() == 1 && value.ordinal() == 1 && value.time() == time::Time::MIDNIGHT
}

#[cfg(test)]
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
        let long_domain = "domain".repeat(1000);
        for domain in [None, Some(""), Some(long_domain.as_str())] {
            let ack_domain = domain.filter(|domain| !domain.is_empty()).unwrap_or("_");
            let v2 = format!(
                "$JS.ACK.{ack_domain}.01234567.{source}.{consumer}.{maximum}.{maximum}.{maximum}.{timestamp}.{maximum}"
            );
            let bound = pull_delivery_bytes(1024, subject.len(), source, consumer, domain).unwrap();
            assert_eq!(bound, 1024 + HEADER_LIMIT_BYTES + subject.len() + v2.len());
            assert!(1024 + HEADER_LIMIT_BYTES + subject.len() + v1.len() <= bound);
        }
        for [payload, subject] in [[usize::MAX, 1], [1, usize::MAX]] {
            assert!(pull_delivery_bytes(payload, subject, "s", "c", Some("d")).is_none());
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
            for reason in DeadLetterReason::ALL {
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
            assert!(!valid_identity(&format!("id{control}x")), "{control:?}");
        }
        for allowed in ["\u{a0}", "caf\u{e9}", "\u{2028}", "\u{1F600}", "~"] {
            assert!(valid_identity(allowed), "{allowed:?}");
        }
        assert!(!valid_identity(""));
        assert!(valid_identity(&"x".repeat(256)));
        assert!(!valid_identity(&"x".repeat(257)));
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

    fn transferred_record(original: &HeaderMap) -> DeadLetterRecord {
        let transfer_id =
            dead_letter_id("SOURCE", 7, OffsetDateTime::UNIX_EPOCH, "event-1").unwrap();
        DeadLetterRecord {
            subject: "orders.dlq".to_owned(),
            headers: dead_letter_headers(
                original,
                "orders.created",
                "DLQ",
                &transfer_id,
                DeadLetterReason::Malformed,
            ),
            payload: Bytes::from_static(b"{}"),
            stream: "DLQ".to_owned(),
            stream_sequence: 3,
            stored_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn without(header: &str) -> HeaderMap {
        envelope_headers()
            .iter()
            .filter(|(name, _)| AsRef::<str>::as_ref(*name) != header)
            .map(|(name, values)| (name.clone(), values[0].clone()))
            .collect()
    }

    fn restore_failure(record: DeadLetterRecord) -> EnvelopeError {
        match restore_dead_letter(record) {
            Err(error) => error,
            other => panic!("expected an envelope refusal, got {other:?}"),
        }
    }

    #[test]
    fn restore_keeps_the_original_message_id() {
        let restored = restore_dead_letter(transferred_record(&envelope_headers())).unwrap();
        assert_eq!(restored.message_id(), "event-1");
    }

    #[test]
    fn restore_refuses_the_message_id_transfer_filled_in() {
        let record = transferred_record(&without(MESSAGE_ID));
        assert_eq!(
            record.headers.get(name::MESSAGE_ID),
            record.headers.get(name::NATS_MSG_ID),
            "transfer keeps Go's wire fallback"
        );
        assert_eq!(
            restore_failure(record),
            EnvelopeError::DeadLetterMessageIdMissing
        );
    }

    #[test]
    fn restore_reports_a_missing_creation_time_as_missing() {
        assert_eq!(
            restore_failure(transferred_record(&without(CREATED_AT))),
            EnvelopeError::MissingHeader(CREATED_AT)
        );
    }
}
