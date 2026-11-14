//! Exact-byte Standard Webhooks v1 framing and verification.
//!
//! This module owns only wire evidence: base64 key decoding, canonical signed
//! framing, timestamp admission, and HMAC verification. It deliberately has
//! no knowledge of endpoints, JSON, persistence, or HTTP routing.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use arrayvec::ArrayString;
use aws_lc_rs::{constant_time, hmac};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue};

pub(crate) const WEBHOOK_ID: &str = "webhook-id";
pub(crate) const WEBHOOK_TIMESTAMP: &str = "webhook-timestamp";
pub(crate) const WEBHOOK_SIGNATURE: &str = "webhook-signature";

/// The fixed maximum raw webhook body accepted by either direction.
pub const MAX_BODY_BYTES: usize = 128 * 1024;
const TIMESTAMP_TOLERANCE_SECONDS: u64 = 300;
const SIGNATURE_PREFIX: &str = "v1,";
const MIN_KEY_BYTES: usize = 24;
const MAX_KEY_BYTES: usize = 64;
const TAG_BYTES: usize = 32;
/// A tag in padded Standard base64: four characters per started three bytes.
const ENCODED_TAG_BYTES: usize = TAG_BYTES.div_ceil(3) * 4;
/// `i64::MIN` in decimal, the longest canonical timestamp.
const MAX_TIMESTAMP_TEXT_BYTES: usize = 20;

/// The longest `webhook-id` accepted by verification.
pub const MAX_MESSAGE_ID_BYTES: usize = 255;

/// A decoded HMAC-SHA256 signing key.
///
/// Construction accepts the Standard Webhooks base64 form, with its optional
/// `whsec_` prefix. The encoded value is never retained or exposed.
#[derive(Clone, derive_more::Debug)]
#[debug("SigningKey([REDACTED])")]
pub struct SigningKey(hmac::Key);

impl SigningKey {
    /// Decode one configured Standard Webhooks secret.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidKey`] for a malformed secret.
    pub fn from_encoded(encoded: &str) -> Result<Self, InvalidKey> {
        let encoded = encoded.trim_prefix("whsec_");
        let decoded = STANDARD.decode(encoded).map_err(|_| InvalidKey)?;
        if !(MIN_KEY_BYTES..=MAX_KEY_BYTES).contains(&decoded.len()) {
            return Err(InvalidKey);
        }
        Ok(Self(hmac::Key::new(hmac::HMAC_SHA256, &decoded)))
    }
}

/// The active signing key and an optional predecessor used during rotation.
#[derive(Clone, derive_more::Debug)]
#[debug("KeyRing([REDACTED])")]
pub struct KeyRing {
    active: SigningKey,
    previous: Option<SigningKey>,
}

impl KeyRing {
    /// Build a key ring from already decoded, redacted keys.
    #[must_use]
    pub const fn new(active: SigningKey, previous: Option<SigningKey>) -> Self {
        Self { active, previous }
    }

    /// Decode the active key and optional predecessor for one endpoint.
    ///
    /// A composition root that resolves each key by reference decodes
    /// individual [`SigningKey`] values and uses [`Self::new`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidKey`] when either supplied secret is malformed.
    pub fn from_encoded(active: &str, previous: Option<&str>) -> Result<Self, InvalidKey> {
        let active = SigningKey::from_encoded(active)?;
        let previous = previous.map(SigningKey::from_encoded).transpose()?;
        Ok(Self::new(active, previous))
    }

    /// Create the Standard Webhooks `webhook-signature` value for a message.
    ///
    /// `timestamp` is the `webhook-timestamp` value in Unix seconds. The
    /// active signature comes first. A predecessor produces one additional
    /// space-separated v1 candidate.
    ///
    /// # Errors
    ///
    /// Returns a closed framing error for an over-limit body or an invalid
    /// message identifier.
    pub fn signatures(
        &self,
        message_id: &[u8],
        timestamp: i64,
        body: &[u8],
    ) -> Result<String, ProtocolError> {
        ensure_body_size(body)?;
        ensure_message_id(message_id)?;
        let candidate_bytes = SIGNATURE_PREFIX.len() + ENCODED_TAG_BYTES;
        let mut signatures = String::with_capacity(2 * candidate_bytes + 1);
        signatures.push_str(SIGNATURE_PREFIX);
        STANDARD.encode_string(
            tag(&self.active, message_id, timestamp, body),
            &mut signatures,
        );
        if let Some(previous) = &self.previous {
            signatures.push(' ');
            signatures.push_str(SIGNATURE_PREFIX);
            STANDARD.encode_string(tag(previous, message_id, timestamp, body), &mut signatures);
        }
        Ok(signatures)
    }

    /// The expected tag is computed once per key, so the work is at most two
    /// HMACs over the body however many candidates the caller supplies.
    ///
    /// # Errors
    ///
    /// Returns a closed error suitable for a sanitized protocol rejection. No
    /// body, identifier, signature, or key bytes appear in the error.
    pub fn verify(
        &self,
        headers: &HeaderMap,
        body: &[u8],
        now: SystemTime,
    ) -> Result<VerifiedMessage, ProtocolError> {
        ensure_body_size(body)?;
        let message_id = unambiguous_header(headers, WEBHOOK_ID)?;
        ensure_message_id(message_id.as_bytes())?;
        let timestamp = parse_timestamp(unambiguous_header(headers, WEBHOOK_TIMESTAMP)?)?;
        ensure_timestamp_window(timestamp, now)?;

        if !headers.contains_key(WEBHOOK_SIGNATURE) {
            return Err(ProtocolError::MissingHeader);
        }
        let matches = |key: &SigningKey| {
            let expected = tag(key, message_id.as_bytes(), timestamp, body);
            matches_v1_signature(headers, &expected)
        };
        if matches(&self.active) || self.previous.as_ref().is_some_and(matches) {
            return Ok(VerifiedMessage {
                message_id: Bytes::copy_from_slice(message_id.as_bytes()),
                timestamp,
            });
        }
        Err(ProtocolError::InvalidSignature)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMessage {
    message_id: Bytes,
    timestamp: i64,
}

impl VerifiedMessage {
    /// The original header bytes, preserved for durable identity.
    #[must_use]
    pub const fn message_id(&self) -> &Bytes {
        &self.message_id
    }

    /// The verified `webhook-timestamp` in Unix seconds.
    #[must_use]
    pub const fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

/// The configured key is not standard base64, or does not decode to 24–64 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("webhook signing key is invalid")]
pub struct InvalidKey;

/// Closed reasons for webhook framing or verification failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("webhook body exceeds the maximum size")]
    BodyTooLarge,
    #[error("webhook signature evidence is incomplete")]
    MissingHeader,
    /// Repeated identity or timestamp headers disagree.
    #[error("webhook signature evidence is ambiguous")]
    ConflictingHeader,
    /// The message identifier is empty, contains the signed delimiter, or exceeds [`MAX_MESSAGE_ID_BYTES`].
    #[error("webhook message identifier is invalid")]
    InvalidMessageId,
    #[error("webhook timestamp is invalid")]
    InvalidTimestamp,
    #[error("webhook timestamp is outside the permitted clock window")]
    TimestampOutOfWindow,
    #[error("webhook signature is invalid")]
    InvalidSignature,
}

impl ProtocolError {
    /// The stable, bounded label for this failure in logs and metrics.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::BodyTooLarge => "body_too_large",
            Self::MissingHeader => "missing_header",
            Self::ConflictingHeader => "conflicting_header",
            Self::InvalidMessageId => "invalid_message_id",
            Self::InvalidTimestamp => "invalid_timestamp",
            Self::TimestampOutOfWindow => "timestamp_out_of_window",
            Self::InvalidSignature => "invalid_signature",
        }
    }
}

pub(crate) fn ensure_body_size(body: &[u8]) -> Result<(), ProtocolError> {
    (body.len() <= MAX_BODY_BYTES).ok_or(ProtocolError::BodyTooLarge)
}

fn unambiguous_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<&'a HeaderValue, ProtocolError> {
    let mut values = headers.get_all(name).iter();
    let first = values.next().ok_or(ProtocolError::MissingHeader)?;
    if values.any(|value| value.as_bytes() != first.as_bytes()) {
        return Err(ProtocolError::ConflictingHeader);
    }
    Ok(first)
}

/// The length bound every message identity meets, signed or not.
pub(crate) fn ensure_message_id_len(message_id: &[u8]) -> Result<(), ProtocolError> {
    (!message_id.is_empty() && message_id.len() <= MAX_MESSAGE_ID_BYTES)
        .ok_or(ProtocolError::InvalidMessageId)
}

/// A signed identity also excludes the `.` that delimits the signed content.
fn ensure_message_id(message_id: &[u8]) -> Result<(), ProtocolError> {
    ensure_message_id_len(message_id)?;
    (!message_id.contains(&b'.')).ok_or(ProtocolError::InvalidMessageId)
}

fn parse_timestamp(value: &HeaderValue) -> Result<i64, ProtocolError> {
    value
        .to_str()
        .ok()
        .and_then(|raw| raw.parse().ok())
        .ok_or(ProtocolError::InvalidTimestamp)
}

fn ensure_timestamp_window(timestamp: i64, now: SystemTime) -> Result<(), ProtocolError> {
    // i128 holds the distance between any i64 timestamp and a clock before or
    // after the epoch without overflow.
    let now = match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::from(duration.as_secs()),
        Err(error) => -i128::from(error.duration().as_secs()),
    };
    (i128::from(timestamp).abs_diff(now) <= u128::from(TIMESTAMP_TOLERANCE_SECONDS))
        .ok_or(ProtocolError::TimestampOutOfWindow)
}

/// The v1 HMAC-SHA256 tag over `message-id "." canonical-timestamp "." body`.
fn tag(key: &SigningKey, message_id: &[u8], timestamp: i64, body: &[u8]) -> hmac::Tag {
    let mut context = hmac::Context::with_key(&key.0);
    context.update(message_id);
    context.update(b".");
    let mut text = ArrayString::<MAX_TIMESTAMP_TEXT_BYTES>::new();
    // The capacity holds every i64, so formatting cannot fail.
    let _ = write!(text, "{timestamp}");
    context.update(text.as_bytes());
    context.update(b".");
    context.update(body);
    context.sign()
}

fn matches_v1_signature(headers: &HeaderMap, expected: &hmac::Tag) -> bool {
    let mut scratch = [0; ENCODED_TAG_BYTES / 4 * 3];
    headers
        .get_all(WEBHOOK_SIGNATURE)
        .iter()
        .flat_map(|value| value.as_bytes().split(u8::is_ascii_whitespace))
        .filter_map(|token| token.strip_prefix(SIGNATURE_PREFIX.as_bytes()))
        .any(|encoded| {
            encoded.len() == ENCODED_TAG_BYTES
                && matches!(STANDARD.decode_slice(encoded, &mut scratch), Ok(TAG_BYTES))
                && constant_time::verify_slices_are_equal(expected.as_ref(), &scratch[..TAG_BYTES])
                    .is_ok()
        })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use http::{HeaderMap, HeaderValue};

    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::{KeyRing, MAX_BODY_BYTES, MAX_MESSAGE_ID_BYTES, ProtocolError, SigningKey};

    // Published Rust reference vector at
    // https://github.com/standard-webhooks/standard-webhooks/blob/59104160f10908d80ee571a81460e1c9c0c05355/libraries/rust/src/lib.rs#L175-L185
    const UPSTREAM_KEY: &str = "whsec_C2FVsBQIhrscChlQIMV+b5sSYspob7oD";
    const UPSTREAM_SIGNATURE: &str = "v1,tZ1I4/hDygAJgO5TYxiSd6Sd0kDW6hPenDe+bTa3Kkw=";
    const UPSTREAM_BODY: &[u8] = br#"{"email":"test@example.com","username":"test_user"}"#;
    // Fixed arbitrary-byte vector from the accepted design table.
    const ARBITRARY_BYTES_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const ARBITRARY_BYTES_SIGNATURE: &str = "v1,zS3Ns419EcpSLc66f4eI2flsBpaIFaRVByOzialbunY=";
    const ARBITRARY_BYTES_BODY: &[u8] = &[0, b'{', b'\"', b'x', b'\"', b':', b'1', b'}', 255];

    fn headers(id: &str, timestamp: &str, signature: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("webhook-id", HeaderValue::from_str(id).unwrap());
        headers.insert(
            "webhook-timestamp",
            HeaderValue::from_str(timestamp).unwrap(),
        );
        headers.insert(
            "webhook-signature",
            HeaderValue::from_str(signature).unwrap(),
        );
        headers
    }

    #[test]
    fn accepts_keys_from_24_through_64_bytes_and_rejects_the_adjacent_lengths() {
        for (len, accepted) in [(23, false), (24, true), (64, true), (65, false)] {
            let encoded = STANDARD.encode(vec![0x11; len]);
            assert_eq!(
                SigningKey::from_encoded(&encoded).is_ok(),
                accepted,
                "{len} bytes"
            );
        }
    }

    #[test]
    fn accepts_a_255_byte_message_id_and_rejects_256_on_verify() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let body = b"x";
        let accepted = "a".repeat(MAX_MESSAGE_ID_BYTES);
        let signature = keys.signatures(accepted.as_bytes(), 1, body).unwrap();
        assert!(
            keys.verify(
                &headers(&accepted, "1", &signature),
                body,
                UNIX_EPOCH + Duration::from_secs(1),
            )
            .is_ok()
        );

        let rejected = "a".repeat(MAX_MESSAGE_ID_BYTES + 1);
        assert_eq!(
            keys.verify(
                &headers(&rejected, "1", "v1,not-base64"),
                body,
                UNIX_EPOCH + Duration::from_secs(1),
            ),
            Err(ProtocolError::InvalidMessageId)
        );
    }

    #[test]
    fn interoperates_with_the_published_standard_webhooks_vector() {
        let keys = KeyRing::from_encoded(UPSTREAM_KEY, None).unwrap();

        assert_eq!(
            keys.signatures(
                b"msg_27UH4WbU6Z5A5EzD8u03UvzRbpk",
                1_649_367_553,
                UPSTREAM_BODY,
            )
            .unwrap(),
            UPSTREAM_SIGNATURE
        );
        let verified = keys
            .verify(
                &headers(
                    "msg_27UH4WbU6Z5A5EzD8u03UvzRbpk",
                    "+001649367553",
                    UPSTREAM_SIGNATURE,
                ),
                UPSTREAM_BODY,
                UNIX_EPOCH + Duration::from_secs(1_649_367_553),
            )
            .unwrap();
        assert_eq!(
            verified.message_id().as_ref(),
            b"msg_27UH4WbU6Z5A5EzD8u03UvzRbpk"
        );
        assert_eq!(verified.timestamp(), 1_649_367_553);
    }

    #[test]
    fn signs_and_verifies_fixed_non_utf8_body_bytes() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();

        assert_eq!(
            keys.signatures(b"msg_test", 1_700_000_000, ARBITRARY_BYTES_BODY)
                .unwrap(),
            ARBITRARY_BYTES_SIGNATURE
        );
        assert!(
            keys.verify(
                &headers("msg_test", "1700000000", ARBITRARY_BYTES_SIGNATURE),
                ARBITRARY_BYTES_BODY,
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            )
            .is_ok()
        );
    }

    #[test]
    fn rotation_signs_with_both_keys_and_accepts_an_isolated_predecessor_candidate() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, Some(UPSTREAM_KEY)).unwrap();
        let predecessor = KeyRing::from_encoded(UPSTREAM_KEY, None).unwrap();
        let body = ARBITRARY_BYTES_BODY;
        let predecessor_signature = predecessor
            .signatures(b"msg_test", 1_700_000_000, body)
            .unwrap();
        let candidates = keys.signatures(b"msg_test", 1_700_000_000, body).unwrap();

        assert_eq!(
            candidates.split_ascii_whitespace().count(),
            2,
            "active and predecessor signatures share one wire header"
        );
        assert!(candidates.ends_with(&predecessor_signature));
        assert!(
            keys.verify(
                &headers("msg_test", "1700000000", &predecessor_signature),
                body,
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_a_predecessor_signature_after_bad_candidates_in_repeated_fields() {
        let keys = KeyRing::from_encoded(UPSTREAM_KEY, Some(ARBITRARY_BYTES_KEY)).unwrap();
        let mut headers = headers("msg_test", "1700000000", "v2,nope");
        let field = [
            b"v1,\xff\t".as_slice(),
            ARBITRARY_BYTES_SIGNATURE.as_bytes(),
        ]
        .concat();
        headers.append(
            "webhook-signature",
            HeaderValue::from_bytes(&field).unwrap(),
        );

        assert!(
            keys.verify(
                &headers,
                ARBITRARY_BYTES_BODY,
                UNIX_EPOCH + Duration::from_secs(1_700_000_000)
            )
            .is_ok()
        );
    }

    #[test]
    fn distinguishes_an_empty_signature_field_from_a_missing_field() {
        let keys = KeyRing::from_encoded(UPSTREAM_KEY, None).unwrap();
        let mut headers = headers("msg_1", "0", "");
        assert_eq!(
            keys.verify(&headers, b"", UNIX_EPOCH),
            Err(ProtocolError::InvalidSignature)
        );
        headers.remove("webhook-signature");
        assert_eq!(
            keys.verify(&headers, b"", UNIX_EPOCH),
            Err(ProtocolError::MissingHeader)
        );
    }

    #[test]
    fn rejects_incomplete_and_extended_tags_even_after_decoding_another_candidate() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let encoded = ARBITRARY_BYTES_SIGNATURE.strip_prefix("v1,").unwrap();
        let tag = STANDARD.decode(encoded).unwrap();
        let mut wrong = tag.clone();
        wrong[0] ^= 1;
        let first = STANDARD.encode(wrong);
        let mut extended = tag.clone();
        extended.push(0);
        for candidate in [
            STANDARD.encode(&tag[..31]),
            STANDARD.encode(extended),
            format!("{}!", encoded.strip_suffix('=').unwrap()),
        ] {
            let fields = headers(
                "msg_test",
                "1700000000",
                &format!("v1,{first} v1,{candidate}"),
            );
            assert_eq!(
                keys.verify(
                    &fields,
                    ARBITRARY_BYTES_BODY,
                    UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                ),
                Err(ProtocolError::InvalidSignature)
            );
        }
    }

    // The header limit admits thousands of candidates, so the HMAC work must
    // stay at most one per key, not one per candidate.
    #[test]
    fn many_signature_candidates_do_not_multiply_the_hmac_work() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, Some(UPSTREAM_KEY)).unwrap();
        let body = vec![0; MAX_BODY_BYTES];
        let fastest = |signature: &str| {
            let headers = headers("msg_1", "1", signature);
            (0..5)
                .map(|_| {
                    let start = Instant::now();
                    assert_eq!(
                        keys.verify(&headers, &body, UNIX_EPOCH + Duration::from_secs(1)),
                        Err(ProtocolError::InvalidSignature)
                    );
                    start.elapsed()
                })
                .min()
                .unwrap()
        };

        let one = fastest("v1,");
        let many = fastest(&vec!["v1,"; 3800].join(" "));

        assert!(
            many < one * 50,
            "one candidate {one:?}, 3800 candidates {many:?}"
        );
    }

    #[test]
    fn accepts_identical_identity_headers_and_rejects_conflicting_identity_or_timestamp_headers() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let body = b"raw";
        let signature = keys.signatures(b"msg_1", 1, body).unwrap();
        let mut identical = headers("msg_1", "1", &signature);
        identical.append("webhook-id", HeaderValue::from_static("msg_1"));
        identical.append("webhook-timestamp", HeaderValue::from_static("1"));
        assert!(
            keys.verify(&identical, body, UNIX_EPOCH + Duration::from_secs(1))
                .is_ok()
        );

        let mut id_conflict = headers("msg_1", "1", &signature);
        id_conflict.append("webhook-id", HeaderValue::from_static("msg_2"));
        assert_eq!(
            keys.verify(&id_conflict, body, UNIX_EPOCH + Duration::from_secs(1)),
            Err(ProtocolError::ConflictingHeader)
        );

        let mut timestamp_conflict = headers("msg_1", "1", &signature);
        timestamp_conflict.append("webhook-timestamp", HeaderValue::from_static("2"));
        assert_eq!(
            keys.verify(
                &timestamp_conflict,
                body,
                UNIX_EPOCH + Duration::from_secs(1)
            ),
            Err(ProtocolError::ConflictingHeader)
        );

        let raw_id = b"msg\xff";
        let raw_signature = keys.signatures(raw_id, 1, body).unwrap();
        let mut raw_id_headers = headers("placeholder", "1", &raw_signature);
        raw_id_headers.insert(
            "webhook-id",
            HeaderValue::from_bytes(raw_id).expect("obs-text message ID is an HTTP field value"),
        );
        let verified = keys
            .verify(&raw_id_headers, body, UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        assert_eq!(verified.message_id().as_ref(), raw_id);
    }

    #[test]
    fn bounds_the_raw_body_before_any_header_is_required() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let body = vec![0; MAX_BODY_BYTES + 1];

        assert_eq!(
            keys.verify(&HeaderMap::new(), &body, UNIX_EPOCH),
            Err(ProtocolError::BodyTooLarge)
        );
    }

    #[test]
    fn accepts_inclusive_clock_edges_and_rejects_the_adjacent_seconds() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let body = b"x";
        let now = 10_000_i64;
        for (offset, accepted) in [(-301, false), (-300, true), (300, true), (301, false)] {
            let timestamp = now + offset;
            let signature = keys.signatures(b"edge", timestamp, body).unwrap();
            assert_eq!(
                keys.verify(
                    &headers("edge", &timestamp.to_string(), &signature),
                    body,
                    UNIX_EPOCH + Duration::from_secs(now.unsigned_abs()),
                )
                .is_ok(),
                accepted,
                "offset {offset}"
            );
        }
    }

    #[test]
    fn rejects_malformed_timestamps_before_signature_verification() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        for timestamp in ["", " ", "1 ", " 1", "+", "1.0", "9223372036854775808"] {
            assert_eq!(
                keys.verify(
                    &headers("msg_test", timestamp, "v1,not-base64"),
                    ARBITRARY_BYTES_BODY,
                    UNIX_EPOCH,
                ),
                Err(ProtocolError::InvalidTimestamp),
                "timestamp {timestamp:?}"
            );
        }
    }

    #[test]
    fn rejects_both_i64_timestamp_extremes_without_overflowing() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let body = b"x";
        for timestamp in [i64::MIN, i64::MAX] {
            let signature = keys.signatures(b"far", timestamp, body).unwrap();
            assert_eq!(
                keys.verify(
                    &headers("far", &timestamp.to_string(), &signature),
                    body,
                    UNIX_EPOCH,
                ),
                Err(ProtocolError::TimestampOutOfWindow)
            );
        }
    }

    #[test]
    fn refuses_unpadded_overpadded_and_non_canonical_candidates() {
        let keys = KeyRing::from_encoded(UPSTREAM_KEY, None).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let accepted = |signature: &str| {
            keys.verify(
                &headers("msg_1", "1700000000", signature),
                UPSTREAM_BODY,
                now,
            )
            .is_ok()
        };
        let signature = keys
            .signatures(b"msg_1", 1_700_000_000, UPSTREAM_BODY)
            .unwrap();
        assert!(accepted(&signature));
        let encoded = signature.strip_suffix('=').unwrap();
        assert!(!accepted(encoded), "missing padding");
        assert!(!accepted(&format!("{encoded}==")), "extra padding");
        // The last data character carries two trailing bits that must be zero.
        let last = encoded.chars().last().unwrap();
        let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let at = alphabet.find(last).unwrap();
        let sibling = alphabet.chars().nth(at ^ 1).unwrap();
        let prefix = &encoded[..encoded.len() - 1];
        assert!(!accepted(&format!("{prefix}{sibling}=")), "trailing bits");
    }

    mod signing_frame {
        use proptest::prelude::*;

        use super::*;

        /// Bytes a header value carries and the signed frame admits: no
        /// controls and no `.`.
        fn message_id() -> impl Strategy<Value = Vec<u8>> {
            proptest::collection::vec(
                prop_oneof![0x20_u8..0x2e, 0x2f_u8..0x7f, 0x80_u8..=0xff],
                1..=MAX_MESSAGE_ID_BYTES,
            )
        }

        fn header_map(id: &[u8], timestamp: &[u8], signature: &[u8]) -> Option<HeaderMap> {
            let mut headers = HeaderMap::new();
            headers.insert("webhook-id", HeaderValue::from_bytes(id).ok()?);
            headers.insert(
                "webhook-timestamp",
                HeaderValue::from_bytes(timestamp).ok()?,
            );
            headers.insert(
                "webhook-signature",
                HeaderValue::from_bytes(signature).ok()?,
            );
            Some(headers)
        }

        fn flip(bytes: &[u8], index: usize, mask: u8) -> Vec<u8> {
            let mut changed = bytes.to_vec();
            let index = index % changed.len();
            changed[index] ^= mask;
            changed
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn verify_accepts_what_signatures_produced_and_refuses_any_single_byte_change(
                id in message_id(),
                now_seconds in 1_000_u64..4_000_000_000,
                offset in -300_i64..=300,
                body in proptest::collection::vec(any::<u8>(), 0..2048),
                rotated in any::<bool>(),
                index in any::<usize>(),
                mask in 1_u8..=255,
            ) {
                let previous = rotated.then_some(ARBITRARY_BYTES_KEY);
                let keys = KeyRing::from_encoded(UPSTREAM_KEY, previous).unwrap();
                let now = UNIX_EPOCH + Duration::from_secs(now_seconds);
                let timestamp = i64::try_from(now_seconds).unwrap() + offset;
                let timestamp_text = timestamp.to_string().into_bytes();
                let signature = keys.signatures(&id, timestamp, &body).unwrap().into_bytes();

                let headers = header_map(&id, &timestamp_text, &signature).unwrap();
                let verified = keys.verify(&headers, &body, now).unwrap();
                prop_assert_eq!(verified.message_id().as_ref(), id.as_slice());
                prop_assert_eq!(verified.timestamp(), timestamp);

                if let Some(headers) = header_map(&flip(&id, index, mask), &timestamp_text, &signature) {
                    prop_assert!(keys.verify(&headers, &body, now).is_err(), "id");
                }
                if let Some(headers) = header_map(&id, &flip(&timestamp_text, index, mask), &signature) {
                    prop_assert!(keys.verify(&headers, &body, now).is_err(), "timestamp");
                }
                if !body.is_empty() {
                    let headers = header_map(&id, &timestamp_text, &signature).unwrap();
                    prop_assert!(
                        keys.verify(&headers, &flip(&body, index, mask), now).is_err(),
                        "body"
                    );
                }
                // One candidate only, so every byte of it, its `v1,` prefix, its
                // padding and its non-zero trailing bits included, must matter.
                let single = KeyRing::from_encoded(UPSTREAM_KEY, None).unwrap();
                let candidate = single.signatures(&id, timestamp, &body).unwrap().into_bytes();
                if let Some(headers) = header_map(&id, &timestamp_text, &flip(&candidate, index, mask)) {
                    prop_assert!(single.verify(&headers, &body, now).is_err(), "candidate");
                }
            }
        }
    }
}
