//! Dead-letter transfer and redrive: the headers a transfer adds, the bound
//! it must fit, its deterministic identity, and restoring the original event.

use async_nats::{HeaderMap, HeaderName};
use bytes::Bytes;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{
    CREATED_AT, EVENT_SCHEMA, EVENT_TYPE, HEADER_LIMIT_BYTES, HEADER_PREAMBLE_BYTES, MESSAGE_ID,
    SHA256_HEX_LEN, checked_header_line_bytes, format_timestamp, header_line_bytes, header_name,
    header_value, is_zero_time, parse_schema, parse_timestamp, prefixed_digest_hex,
    required_header, valid_subject, validate_outbound_identity,
};
use crate::error::EnvelopeError;
use enum_map::EnumMap;

use crate::observe::{OutcomeLabel as _, outcome_label};
use crate::prepared::{PreparedEvent, PreparedEventParts};

const DEAD_LETTER_ID_PREFIX: &str = "dlq-";
const ORIGINAL_ID_PRESENT: HeaderName = HeaderName::from_static("Original-Message-Id-Present");
const RETAINED_HEADERS: [HeaderName; 6] = [
    header_name::MESSAGE_ID,
    header_name::EVENT_TYPE,
    header_name::EVENT_SCHEMA,
    header_name::CREATED_AT,
    crate::trace::TRACEPARENT,
    crate::trace::TRACESTATE,
];

outcome_label! {
    /// Why a delivery moved to the dead-letter stream; the `Dead-Letter-Reason`
    /// header value and the `messaging_dead_letter_total` label.
    pub(crate) enum DeadLetterReason {
        Malformed => "malformed",
        Permanent => "permanent",
        Exhausted => "exhausted",
    }
}

pub(crate) struct DeadLetterBounds {
    pub(crate) total: usize,
    pub(crate) headers: usize,
}

fn transfer_headers<T>(
    publication_id: T,
    subject: T,
    reason: T,
    stream: T,
) -> [(HeaderName, T); 4] {
    [
        (header_name::NATS_MSG_ID, publication_id),
        (header_name::ORIGINAL_SUBJECT, subject),
        (header_name::DEAD_LETTER_REASON, reason),
        (async_nats::header::NATS_EXPECTED_STREAM, stream),
    ]
}

/// Reserve for the 12-byte framing of a new block and for re-encoding
/// retained fields from a noncanonical block the client accepted.
pub(crate) fn dead_letter_bounds(
    source_bytes: usize,
    subject_bytes: usize,
    stream_bytes: usize,
) -> Option<DeadLetterBounds> {
    let reason_bytes = EnumMap::from_fn(|reason: DeadLetterReason| reason.label().len())
        .into_values()
        .max()?;
    let transfer_id_bytes = DEAD_LETTER_ID_PREFIX.len() + SHA256_HEX_LEN;
    // The fallback Message-Id line is larger than the mutually exclusive provenance marker.
    let identity_bytes = checked_header_line_bytes(MESSAGE_ID, transfer_id_bytes)?
        .max(checked_header_line_bytes(ORIGINAL_ID_PRESENT.as_ref(), 1)?);
    let additions = transfer_headers(transfer_id_bytes, subject_bytes, reason_bytes, stream_bytes)
        .into_iter()
        .try_fold(identity_bytes, |bytes, (name, value_bytes)| {
            bytes.checked_add(checked_header_line_bytes(name.as_ref(), value_bytes)?)
        })?;
    // The client accepts bare LF and a missing final newline. Re-encoding can
    // add one CR and one space per retained line, a CR on the version line,
    // a final blank CRLF, and the last missing LF.
    let framing_reserve = HEADER_PREAMBLE_BYTES.max(RETAINED_HEADERS.len() * 2 + 4);
    let framed_source = source_bytes.checked_add(framing_reserve)?;
    Some(DeadLetterBounds {
        total: framed_source.checked_add(additions)?,
        headers: framed_source
            .min(HEADER_LIMIT_BYTES)
            .checked_add(additions)?,
    })
}

/// Keeps whole first identity and trace values that fit the source header budget.
pub(crate) fn dead_letter_headers(
    original: &HeaderMap,
    subject: &str,
    stream: &str,
    transfer_id: &str,
    reason: DeadLetterReason,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let mut remaining = HEADER_LIMIT_BYTES - HEADER_PREAMBLE_BYTES;
    for header in RETAINED_HEADERS {
        let value = header_value(original, header.clone());
        let bytes = header_line_bytes(header.as_ref(), value);
        if !value.is_empty() && bytes <= remaining {
            headers.insert(header, value);
            remaining -= bytes;
        }
    }
    let message_id = header_value(&headers, header_name::MESSAGE_ID);
    if message_id.is_empty() {
        headers.insert(header_name::MESSAGE_ID, transfer_id);
    } else if message_id == transfer_id {
        headers.insert(ORIGINAL_ID_PRESENT, "1");
    }
    for (name, value) in transfer_headers(transfer_id, subject, reason.label(), stream) {
        headers.insert(name, value);
    }
    headers
}

/// One stored dead-letter record used for deterministic redrive reconstruction.
#[derive(Clone, Debug)]
pub struct DeadLetterRecord {
    pub headers: HeaderMap,
    pub payload: Bytes,
    pub stream: String,
    pub stream_sequence: u64,
    pub stored_at: OffsetDateTime,
}

/// Reconstructs an original event with Go's deterministic `redrive-` ID.
///
/// # Errors
/// Rejects a record without restorable subject, identity or creation time.
/// An unmarked legacy transfer ID shared by both identity headers is ambiguous.
pub fn restore_dead_letter(record: DeadLetterRecord) -> Result<PreparedEvent, EnvelopeError> {
    let subject = header_value(&record.headers, header_name::ORIGINAL_SUBJECT).to_owned();
    if !valid_subject(&subject) {
        return Err(EnvelopeError::DeadLetterSubject);
    }
    let message_id = required_header(&record.headers, header_name::MESSAGE_ID, MESSAGE_ID)?;
    validate_outbound_identity(message_id)?;
    let stored_publication_id = header_value(&record.headers, header_name::NATS_MSG_ID);
    if has_ambiguous_original_id(&record.headers, message_id, stored_publication_id) {
        return Err(EnvelopeError::DeadLetterMessageIdMissing);
    }
    let event_type = required_header(&record.headers, header_name::EVENT_TYPE, EVENT_TYPE)?;
    validate_outbound_identity(event_type)?;
    let schema_version = parse_schema(required_header(
        &record.headers,
        header_name::EVENT_SCHEMA,
        EVENT_SCHEMA,
    )?)?;
    let occurred_at = parse_timestamp(required_header(
        &record.headers,
        header_name::CREATED_AT,
        CREATED_AT,
    )?)?;
    if is_zero_time(occurred_at) {
        return Err(EnvelopeError::DeadLetterCreatedAtRequired);
    }
    // Go includes an empty hash component when the stored record has no Nats-Msg-Id.
    let publication_id = record_id(
        "redrive-",
        &record.stream,
        record.stream_sequence,
        record.stored_at,
        stored_publication_id,
    )?;
    PreparedEvent::from_parts(PreparedEventParts {
        subject,
        message_id: message_id.to_owned(),
        publication_id,
        event_type: event_type.to_owned(),
        schema_version,
        occurred_at,
        payload: record.payload,
    })
}

fn has_ambiguous_original_id(
    headers: &HeaderMap,
    message_id: &str,
    stored_publication_id: &str,
) -> bool {
    // Go and older Rust transfers use their deterministic ID as a fallback
    // Message-Id. A genuine collision in a new Rust transfer carries provenance.
    message_id == stored_publication_id
        && header_value(headers, ORIGINAL_ID_PRESENT) != "1"
        && message_id
            .strip_prefix(DEAD_LETTER_ID_PREFIX)
            .is_some_and(|digest| {
                digest.len() == SHA256_HEX_LEN
                    && digest
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
}

/// Go-compatible deterministic transfer identity for dead-letter publication.
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
    Ok(prefixed_digest_hex(prefix, &hash.finalize().into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::tests::envelope_headers;
    use crate::wire::{decode_envelope, encoded_header_bytes};

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
            for (reason, ()) in EnumMap::<DeadLetterReason, ()>::default() {
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
    fn transfer_bound_includes_the_framing_of_a_headerless_source() {
        let record = transferred_record(&HeaderMap::new());
        let subject = header_value(&record.headers, header_name::ORIGINAL_SUBJECT);
        let bounds =
            dead_letter_bounds(record.payload.len(), subject.len(), record.stream.len()).unwrap();
        assert!(encoded_header_bytes(&record.headers) + record.payload.len() <= bounds.total);
    }

    #[test]
    fn transfer_keeps_payload_identity_when_oversized_trace_metadata_cannot_fit() {
        let mut original = envelope_headers();
        original.insert(crate::trace::TRACESTATE, "x".repeat(HEADER_LIMIT_BYTES));
        let record = transferred_record(&original);
        assert!(record.headers.get(crate::trace::TRACESTATE).is_none());
        let restored = restore_dead_letter(record).unwrap();
        assert_eq!(restored.message_id(), "event-1");
        assert_eq!(restored.payload().as_ref(), b"{}");
    }

    fn transferred_record(original: &HeaderMap) -> DeadLetterRecord {
        let transfer_id =
            dead_letter_id("SOURCE", 7, OffsetDateTime::UNIX_EPOCH, "event-1").unwrap();
        DeadLetterRecord {
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
    fn restore_accepts_a_genuine_id_shared_with_the_dlq_publication() {
        let mut external = transferred_record(&envelope_headers());
        external.headers.insert(header_name::NATS_MSG_ID, "event-1");
        assert_eq!(
            restore_dead_letter(external).unwrap().message_id(),
            "event-1"
        );

        let mut original = envelope_headers();
        let transfer_id =
            dead_letter_id("SOURCE", 7, OffsetDateTime::UNIX_EPOCH, "event-1").unwrap();
        original.insert(header_name::MESSAGE_ID, transfer_id.as_str());
        assert_eq!(
            restore_dead_letter(transferred_record(&original))
                .unwrap()
                .message_id(),
            transfer_id
        );
    }

    #[test]
    fn restore_refuses_the_message_id_transfer_filled_in() {
        let record = transferred_record(&without(MESSAGE_ID));
        assert_eq!(
            record.headers.get(header_name::MESSAGE_ID),
            record.headers.get(header_name::NATS_MSG_ID),
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

    #[test]
    fn restore_rejects_a_record_whose_identity_would_change_on_delivery() {
        let mut record = transferred_record(&envelope_headers());
        record.headers.insert(header_name::MESSAGE_ID, " event-1 ");
        assert_eq!(restore_failure(record), EnvelopeError::Identity);
    }

    #[test]
    fn restore_names_the_invalid_schema_or_time() {
        for (header, expected) in [
            (header_name::EVENT_SCHEMA, EnvelopeError::Schema),
            (header_name::CREATED_AT, EnvelopeError::CreatedAtInvalid),
        ] {
            let mut record = transferred_record(&envelope_headers());
            record.headers.insert(header, "");
            assert_eq!(restore_failure(record), expected);
        }
    }
}
