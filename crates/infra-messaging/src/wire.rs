//! Go-compatible NATS envelope headers and bounded decoding.

use async_nats::{HeaderMap, HeaderName};
use bytes::Bytes;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::error::EnvelopeError;
use crate::prepared::PreparedEvent;

mod dead_letter;

pub(crate) use dead_letter::{DeadLetterReason, dead_letter_bounds, dead_letter_headers};
pub use dead_letter::{DeadLetterRecord, dead_letter_id, restore_dead_letter};

/// The encoded header limit of one envelope; the Go service enforces the same
/// limit, so either side accepts what the other publishes.
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
/// Hex digits of a SHA-256 digest.
const DIGEST_HEX_LEN: usize = 64;
/// The `NATS/1.0` status line and the blank line that ends a header block.
const HEADER_PREAMBLE_BYTES: usize = "NATS/1.0\r\n\r\n".len();

// A `&str` header name is validated and copied on every insert and lookup;
// these are built at compile time.
pub(crate) mod header_name {
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
    headers.insert(header_name::MESSAGE_ID, event.message_id());
    headers.insert(header_name::EVENT_TYPE, event.event_type());
    headers.insert(header_name::EVENT_SCHEMA, event.schema_label());
    headers.insert(
        header_name::CREATED_AT,
        format_timestamp(event.occurred_at())?,
    );
    headers.insert(header_name::NATS_MSG_ID, event.publication_id());
    validate_header_bytes(&headers)?;
    Ok(headers)
}

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
    validate_identity(message_id)?;
    let event_type = required_value(found.event_type, EVENT_TYPE)?;
    validate_identity(event_type)?;
    let schema = required_value(found.schema, EVENT_SCHEMA)?;
    let created_at = required_value(found.created_at, CREATED_AT)?;
    validate_identity(required_value(found.publication_id, NATS_MSG_ID)?)?;
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
///
/// `subject` is a concrete subject accepted by [`valid_subject`].
pub(crate) fn matches_filter(filter: &str, subject: &str) -> bool {
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
    header: &'static str,
) -> Result<&'a str, EnvelopeError> {
    value.ok_or(EnvelopeError::MissingHeader(header))
}

fn required_header<'a>(
    headers: &'a HeaderMap,
    name: HeaderName,
    header: &'static str,
) -> Result<&'a str, EnvelopeError> {
    required_value(
        headers.get(name).map(async_nats::HeaderValue::as_str),
        header,
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

fn validate_identity(value: &str) -> Result<(), EnvelopeError> {
    if !valid_identity(value) {
        return Err(EnvelopeError::Identity);
    }
    Ok(())
}

/// The NATS reader trims surrounding whitespace before handing off a header.
pub(crate) fn validate_outbound_identity(value: &str) -> Result<(), EnvelopeError> {
    validate_identity(value)?;
    if value.trim() != value {
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
const HEADER_LINE_FRAMING_BYTES: usize = ": \r\n".len();

/// Encoded bytes of one `name: value` header line.
fn header_line_bytes(name: &str, value: &str) -> usize {
    name.len() + value.len() + HEADER_LINE_FRAMING_BYTES
}

/// [`header_line_bytes`] for a value of `value_bytes`, `None` on overflow.
fn checked_header_line_bytes(name: &str, value_bytes: usize) -> Option<usize> {
    name.len()
        .checked_add(HEADER_LINE_FRAMING_BYTES)?
        .checked_add(value_bytes)
}

fn encoded_header_bytes(headers: &HeaderMap) -> usize {
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

/// Checks that [`format_timestamp`] accepts `value` without building the
/// text.
pub(crate) fn check_timestamp(value: OffsetDateTime) -> Result<(), EnvelopeError> {
    value
        .checked_to_offset(UtcOffset::UTC)
        .and_then(|value| value.format_into(&mut std::io::sink(), &Rfc3339).ok())
        .map(drop)
        .ok_or(EnvelopeError::CreatedAtOutOfRange)
}

fn parse_timestamp(value: &str) -> Result<OffsetDateTime, EnvelopeError> {
    validate_identity(value).map_err(|_| EnvelopeError::CreatedAtInvalid)?;
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(|value| value.checked_to_offset(UtcOffset::UTC))
        .ok_or(EnvelopeError::CreatedAtInvalid)
}

/// `prefix` followed by the lowercase hex of a SHA-256 digest.
#[must_use]
pub fn prefixed_digest_hex(prefix: &str, digest: &[u8; 32]) -> String {
    let mut id = String::with_capacity(prefix.len() + DIGEST_HEX_LEN);
    id.push_str(prefix);
    id.push_str(const_hex::Buffer::<32>::new().format(digest));
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

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        #[test]
        fn identity_validity_is_non_empty_bounded_and_control_free(
            text in proptest::prop_oneof![
                proptest::prelude::any::<String>(),
                "[a-z\\x00-\\x9f\\u{a0}-\\u{ff}]{0,300}",
            ],
        ) {
            let expected = !text.is_empty()
                && text.len() <= IDENTITY_MAX_BYTES
                && !text.chars().any(char::is_control);
            proptest::prop_assert_eq!(valid_identity(&text), expected);
        }

        #[test]
        fn schema_parses_exactly_v_and_a_positive_decimal_that_fits(
            text in proptest::prop_oneof![
                proptest::prelude::any::<String>(),
                "v?[0-9+\\-]{0,7}",
            ],
        ) {
            let expected = text.strip_prefix('v').and_then(|digits| {
                let well_formed = digits.bytes().all(|byte| byte.is_ascii_digit())
                    && digits.bytes().next().is_some_and(|first| first != b'0');
                if !well_formed {
                    return None;
                }
                let value = digits.bytes().fold(0_u64, |value, byte| {
                    (value * 10 + u64::from(byte - b'0')).min(u64::from(u16::MAX) + 1)
                });
                u16::try_from(value).ok()
            });
            proptest::prop_assert_eq!(parse_schema(&text).ok(), expected);
        }

        #[test]
        fn digest_hex_is_the_prefix_and_lowercase_hex(
            prefix in "[a-z-]{0,8}",
            digest in proptest::prelude::any::<[u8; 32]>(),
        ) {
            let mut expected = prefix.clone();
            for byte in digest {
                let _ = std::fmt::Write::write_fmt(&mut expected, format_args!("{byte:02x}"));
            }
            proptest::prop_assert_eq!(prefixed_digest_hex(&prefix, &digest), expected);
        }
    }

    pub(super) fn envelope_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header_name::MESSAGE_ID, "event-1");
        headers.insert(header_name::EVENT_TYPE, "order.created");
        headers.insert(header_name::EVENT_SCHEMA, "v2");
        headers.insert(header_name::CREATED_AT, "2026-09-29T10:00:00Z");
        headers.insert(header_name::NATS_MSG_ID, "event-1");
        headers
    }

    #[test]
    fn decode_reads_the_first_value_of_each_identity_header_and_ignores_others() {
        let mut headers = envelope_headers();
        headers.append(header_name::MESSAGE_ID, "event-2");
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
        headers.insert(header_name::EVENT_TYPE, "order\u{85}created");
        assert!(decode_envelope("orders.created", &headers, Bytes::new()).is_err());
    }

    #[test]
    fn decode_names_the_invalid_schema_or_time() {
        for (header, expected) in [
            (header_name::EVENT_SCHEMA, EnvelopeError::Schema),
            (header_name::CREATED_AT, EnvelopeError::CreatedAtInvalid),
        ] {
            let mut headers = envelope_headers();
            headers.insert(header, "");
            let error = decode_envelope("orders.created", &headers, Bytes::new()).unwrap_err();
            assert_eq!(error, expected);
        }
    }
}
