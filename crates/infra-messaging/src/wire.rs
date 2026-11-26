use arrayvec::{ArrayString, ArrayVec};
use async_nats::HeaderName;
use compact_str::CompactString;
use time::format_description::well_known::Rfc3339;

use crate::error::{PrepareError, PublishEnvelopeError};
use crate::prepared::{PreparedEvent, Publication};

mod dead_letter;

pub use async_nats::HeaderMap;
pub use bytes::Bytes;
pub(crate) use dead_letter::{DeadLetterReason, dead_letter_bounds, dead_letter_headers};
pub use dead_letter::{DeadLetterRecord, RestoreError, dead_letter_id, restore_dead_letter};
pub use time::{OffsetDateTime, UtcDateTime};

pub const HEADER_LIMIT_BYTES: usize = shared_rules::messaging::HEADER_LIMIT_BYTES;
pub const MESSAGE_ID: &str = "Message-Id";
pub const EVENT_TYPE: &str = "Event-Type";
pub const EVENT_SCHEMA: &str = "Event-Schema";
pub const CREATED_AT: &str = "Created-At";
pub const NATS_MSG_ID: &str = "Nats-Msg-Id";
pub const ORIGINAL_SUBJECT: &str = "Original-Subject";
pub const DEAD_LETTER_REASON: &str = "Dead-Letter-Reason";

pub(crate) const NATIVE_HEADER_LIMIT_BYTES: usize = 65_535;
pub(crate) const IDENTITY_MAX_BYTES: usize = 256;
const SHA256_HEX_LEN: usize = 64;
const HEADER_PREAMBLE_BYTES: usize = "NATS/1.0\r\n\r\n".len();
const RFC3339_MAX_BYTES: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("creation time is out of range")]
pub struct TimestampOutOfRange;

impl From<TimestampOutOfRange> for PrepareError {
    fn from(_: TimestampOutOfRange) -> Self {
        Self::CreatedAtOutOfRange
    }
}

impl From<TimestampOutOfRange> for RestoreError {
    fn from(_: TimestampOutOfRange) -> Self {
        Self::CreatedAtOutOfRange
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CreatedAt {
    instant: UtcDateTime,
    rfc3339: ArrayString<RFC3339_MAX_BYTES>,
}

impl CreatedAt {
    pub(crate) fn new(instant: UtcDateTime) -> Result<Self, TimestampOutOfRange> {
        let bytes = format_timestamp_bytes(instant)?;
        let text = str::from_utf8(&bytes).map_err(|_| TimestampOutOfRange)?;
        let rfc3339 = ArrayString::from(text).map_err(|_| TimestampOutOfRange)?;
        Ok(Self { instant, rfc3339 })
    }

    pub(crate) fn new_outbound(instant: UtcDateTime) -> Result<Self, PrepareError> {
        if is_zero_time(instant) {
            return Err(PrepareError::CreatedAtRequired);
        }
        Self::new(instant).map_err(PrepareError::from)
    }

    pub(crate) const fn instant(self) -> UtcDateTime {
        self.instant
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.rfc3339
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("subject is invalid")]
    Subject,
    #[error("encoded headers exceed {} KiB", HEADER_LIMIT_BYTES / 1024)]
    HeadersTooLarge,
    #[error("required header {0} is missing")]
    MissingHeader(&'static str),
    #[error("event identity is invalid")]
    Identity,
    #[error("event schema is invalid")]
    Schema,
    #[error("creation time is invalid")]
    CreatedAtInvalid,
    #[error("creation time is required")]
    CreatedAtRequired,
}

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
    pub(crate) event_type: CompactString,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: UtcDateTime,
    pub(crate) payload: Bytes,
}

pub(crate) struct InboundEnvelopeRef<'a> {
    pub(crate) message_id: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: UtcDateTime,
    pub(crate) payload: &'a [u8],
}

#[must_use]
pub fn encode_prepared(event: &PreparedEvent) -> HeaderMap {
    encode_publication(&event.publication())
}

pub(crate) fn encode_publication(publication: &Publication<'_>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header_name::MESSAGE_ID, publication.message_id);
    headers.insert(header_name::EVENT_TYPE, publication.event_type);
    headers.insert(
        header_name::EVENT_SCHEMA,
        schema_label(publication.schema_version),
    );
    headers.insert(header_name::CREATED_AT, publication.created_at);
    headers.insert(header_name::NATS_MSG_ID, publication.publication_id);
    headers
}

pub fn decode_envelope(
    subject: &str,
    headers: &HeaderMap,
    payload: Bytes,
) -> Result<InboundEnvelope, DecodeError> {
    let envelope = decode_envelope_ref(subject, headers, &payload)?;
    Ok(InboundEnvelope {
        message_id: envelope.message_id.to_owned(),
        event_type: CompactString::new(envelope.event_type),
        schema_version: envelope.schema_version,
        occurred_at: envelope.occurred_at,
        payload,
    })
}

pub(crate) fn decode_envelope_ref<'a>(
    subject: &str,
    headers: &'a HeaderMap,
    payload: &'a [u8],
) -> Result<InboundEnvelopeRef<'a>, DecodeError> {
    if !valid_subject(subject) {
        return Err(DecodeError::Subject);
    }
    let mut header_bytes = HEADER_PREAMBLE_BYTES;
    let mut found = EnvelopeHeaders::default();
    #[expect(
        clippy::iter_over_hash_type,
        reason = "order-independent header byte sum and distinct-slot decoding"
    )]
    for (header, values) in headers.iter() {
        let header: &str = header.as_ref();
        for value in values {
            header_bytes += header_line_bytes(header, value.as_str());
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
    if header_bytes > HEADER_LIMIT_BYTES {
        return Err(DecodeError::HeadersTooLarge);
    }
    let message_id = required_identity(found.message_id, MESSAGE_ID)?;
    let event_type = required_identity(found.event_type, EVENT_TYPE)?;
    let schema = required_value(found.schema, EVENT_SCHEMA)?;
    let created_at = required_value(found.created_at, CREATED_AT)?;
    required_identity(found.publication_id, NATS_MSG_ID)?;
    let schema_version = parse_schema(schema).ok_or(DecodeError::Schema)?;
    let occurred_at = parse_timestamp(created_at).ok_or(DecodeError::CreatedAtInvalid)?;
    if is_zero_time(occurred_at) {
        return Err(DecodeError::CreatedAtRequired);
    }

    Ok(InboundEnvelopeRef {
        message_id,
        event_type,
        schema_version,
        occurred_at,
        payload,
    })
}

#[derive(Default)]
struct EnvelopeHeaders<'a> {
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
    pub const fn occurred_at(&self) -> UtcDateTime {
        self.occurred_at
    }
    #[must_use]
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }
}

pub(crate) struct BorrowedSubject<'a>(pub(crate) &'a async_nats::Subject);

impl async_nats::ToSubject for BorrowedSubject<'_> {
    fn to_subject(&self) -> async_nats::Subject {
        self.0.clone()
    }
}

pub(crate) fn valid_subject(subject: &str) -> bool {
    let mut at_token_start = true;
    for byte in subject.bytes() {
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' | b'*' | b'>' => return false,
            b'.' if at_token_start => return false,
            b'.' => at_token_start = true,
            _ => at_token_start = false,
        }
    }
    !at_token_start
}

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
) -> Result<&'a str, DecodeError> {
    value.ok_or(DecodeError::MissingHeader(header))
}

fn required_identity<'a>(
    value: Option<&'a str>,
    header: &'static str,
) -> Result<&'a str, DecodeError> {
    let value = required_value(value, header)?;
    valid_identity(value)
        .then_some(value)
        .ok_or(DecodeError::Identity)
}

pub(crate) fn schema_label(version: u16) -> String {
    format!("v{version}")
}

fn parse_schema(value: &str) -> Option<u16> {
    value
        .strip_prefix('v')
        .filter(|raw| matches!(raw.as_bytes().first(), Some(b'1'..=b'9')))
        .and_then(|raw| raw.parse().ok())
}

pub(crate) fn valid_outbound_identity(value: &str) -> bool {
    valid_identity(value) && value.trim().len() == value.len()
}

pub(crate) const IDENTITY_SCHEMA_PATTERN: &str = "^[^\\u0000-\\u001F\\u007F-\\u009F]*$";

pub(crate) const fn valid_identity(value: &str) -> bool {
    const ASCII_C0_END: u8 = 0x20;
    const ASCII_DEL: u8 = 0x7f;
    const UTF8_C1_LEAD: u8 = 0xc2;
    const UTF8_C1_CONTINUATION_END: u8 = 0xa0;

    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > IDENTITY_MAX_BYTES {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < ASCII_C0_END
            || byte == ASCII_DEL
            || (byte == UTF8_C1_LEAD && bytes[index + 1] < UTF8_C1_CONTINUATION_END)
        {
            return false;
        }
        index += 1;
    }
    true
}

const HEADER_LINE_FRAMING_BYTES: usize = ": \r\n".len();

fn header_line_bytes(name: &str, value: &str) -> usize {
    name.len() + value.len() + HEADER_LINE_FRAMING_BYTES
}

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

pub(crate) fn validate_header_bytes(headers: &HeaderMap) -> Result<(), PublishEnvelopeError> {
    (encoded_header_bytes(headers) <= HEADER_LIMIT_BYTES)
        .ok_or(PublishEnvelopeError::HeadersTooLarge)
}

pub(crate) fn format_timestamp_bytes(
    value: UtcDateTime,
) -> Result<ArrayVec<u8, RFC3339_MAX_BYTES>, TimestampOutOfRange> {
    let mut buf = ArrayVec::new();
    value
        .format_into(&mut buf, &Rfc3339)
        .map_err(|_| TimestampOutOfRange)?;
    Ok(buf)
}

fn parse_timestamp(value: &str) -> Option<UtcDateTime> {
    const SEPARATOR_INDEX: usize = "YYYY-MM-DD".len();
    const CREATED_AT_MAX_BYTES: usize = IDENTITY_MAX_BYTES;
    if value.len() > CREATED_AT_MAX_BYTES
        || value
            .as_bytes()
            .get(SEPARATOR_INDEX)
            .is_none_or(u8::is_ascii_control)
    {
        return None;
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .and_then(OffsetDateTime::checked_to_utc)
}

#[must_use]
pub(crate) fn prefixed_digest_hex(prefix: &str, digest: &[u8; 32]) -> String {
    [prefix, const_hex::Buffer::<32>::new().format(digest)].concat()
}

pub(crate) fn is_zero_time(value: UtcDateTime) -> bool {
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
            proptest::prop_assert_eq!(parse_schema(&text), expected);
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

    pub(super) fn envelope_headers_without(header: &str) -> HeaderMap {
        envelope_headers()
            .iter()
            .filter(|(name, _)| AsRef::<str>::as_ref(*name) != header)
            .map(|(name, values)| (name.clone(), values[0].clone()))
            .collect()
    }

    #[test]
    #[warn(clippy::assertions_on_result_states)]
    fn decode_rejects_a_missing_or_invalid_identity_header() {
        for missing in [
            MESSAGE_ID,
            EVENT_TYPE,
            EVENT_SCHEMA,
            CREATED_AT,
            NATS_MSG_ID,
        ] {
            let headers = envelope_headers_without(missing);
            assert_eq!(
                decode_envelope("orders.created", &headers, Bytes::new()).unwrap_err(),
                DecodeError::MissingHeader(missing)
            );
        }
        let mut headers = envelope_headers();
        headers.insert(header_name::EVENT_TYPE, "order\u{85}created");
        assert_eq!(
            decode_envelope("orders.created", &headers, Bytes::new()).unwrap_err(),
            DecodeError::Identity
        );
    }

    #[test]
    fn decode_names_the_invalid_schema_or_time() {
        for (header, expected) in [
            (header_name::EVENT_SCHEMA, DecodeError::Schema),
            (header_name::CREATED_AT, DecodeError::CreatedAtInvalid),
        ] {
            let mut headers = envelope_headers();
            headers.insert(header, "");
            let error = decode_envelope("orders.created", &headers, Bytes::new()).unwrap_err();
            assert_eq!(error, expected);
        }
    }

    #[test]
    fn decode_preserves_timestamp_separators_and_the_byte_limit() {
        let mut headers = envelope_headers();
        let expected = time::macros::utc_datetime!(2026-09-29 10:00);
        for separator in ['t', ' ', '@'] {
            headers.insert(
                header_name::CREATED_AT,
                format!("2026-09-29{separator}10:00:00z"),
            );
            let envelope = decode_envelope("orders.created", &headers, Bytes::new()).unwrap();
            assert_eq!(envelope.occurred_at(), expected);
        }
        for separator in ['\0', '\x1f', '\x7f', '\u{85}', 'é'] {
            headers.insert(
                header_name::CREATED_AT,
                format!("2026-09-29{separator}10:00:00Z"),
            );
            let error = decode_envelope("orders.created", &headers, Bytes::new()).unwrap_err();
            assert_eq!(error, DecodeError::CreatedAtInvalid, "{separator:?}");
        }
        headers.insert(
            header_name::CREATED_AT,
            format!("2026-09-29T10:00:00.{}Z", "0".repeat(235)),
        );
        let envelope = decode_envelope("orders.created", &headers, Bytes::new()).unwrap();
        assert_eq!(envelope.occurred_at(), expected);
        headers.insert(
            header_name::CREATED_AT,
            format!("2026-09-29T10:00:00.{}Z", "0".repeat(236)),
        );
        let error = decode_envelope("orders.created", &headers, Bytes::new()).unwrap_err();
        assert_eq!(error, DecodeError::CreatedAtInvalid);
    }

    #[test]
    fn outbound_timestamp_year_bound_uses_the_utc_year() {
        use time::macros::datetime;

        let crosses_into_range = datetime!(-0001-12-31 23:30 -01:00);
        let utc_in_range = crosses_into_range.to_utc();
        let created_at = CreatedAt::new_outbound(utc_in_range).unwrap();
        assert_eq!(created_at.instant(), utc_in_range);
        assert_eq!(created_at.as_str(), "0000-01-01T00:30:00Z");
        assert!(dead_letter_id("SOURCE", 1, crosses_into_range, "event-1").is_ok());

        let crosses_out_of_range = datetime!(0000-01-01 00:30 +01:00);
        assert_eq!(
            CreatedAt::new_outbound(crosses_out_of_range.to_utc()),
            Err(PrepareError::CreatedAtOutOfRange)
        );
        assert_eq!(
            dead_letter_id("SOURCE", 1, crosses_out_of_range, "event-1"),
            Err(TimestampOutOfRange)
        );
    }
}
