//! Exact-byte Standard Webhooks v1 framing and verification.
//!
//! This module owns only wire evidence: base64 key decoding, canonical signed
//! framing, timestamp admission, and HMAC verification. It deliberately has
//! no knowledge of endpoints, JSON, persistence, or HTTP routing.

use std::{
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use aws_lc_rs::hmac;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{HeaderMap, HeaderValue};

/// The fixed maximum raw webhook body accepted by either direction.
pub const MAX_BODY_BYTES: usize = 128 * 1024;
const TIMESTAMP_TOLERANCE_SECONDS: i128 = 300;
const SIGNATURE_PREFIX: &[u8] = b"v1,";

/// A decoded HMAC-SHA256 signing key.
///
/// Construction accepts the Standard Webhooks base64 form, with its optional
/// `whsec_` prefix. The encoded value is never retained or exposed.
#[derive(Clone)]
pub struct SigningKey(hmac::Key);

impl SigningKey {
    /// Decode one configured Standard Webhooks secret.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidKey`] when the value is not standard
    /// base64 or decodes to no bytes.
    pub fn from_encoded(encoded: &str) -> Result<Self, ProtocolError> {
        let encoded = encoded.strip_prefix("whsec_").unwrap_or(encoded);
        let decoded = STANDARD
            .decode(encoded)
            .map_err(|_| ProtocolError::InvalidKey)?;
        if decoded.is_empty() {
            return Err(ProtocolError::InvalidKey);
        }
        Ok(Self(hmac::Key::new(hmac::HMAC_SHA256, &decoded)))
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SigningKey([REDACTED])")
    }
}

/// The active signing key and an optional predecessor used during rotation.
#[derive(Clone)]
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
    /// This convenience constructor is useful at a composition root; workers
    /// that resolve immutable historical key references construct individual
    /// [`SigningKey`] values and then use [`Self::new`].
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidKey`] when either supplied secret is
    /// malformed or empty after decoding.
    pub fn from_encoded(active: &str, previous: Option<&str>) -> Result<Self, ProtocolError> {
        let active = SigningKey::from_encoded(active)?;
        let previous = previous.map(SigningKey::from_encoded).transpose()?;
        Ok(Self::new(active, previous))
    }

    /// Create the Standard Webhooks `webhook-signature` value for a message.
    ///
    /// The active signature comes first. A predecessor produces one additional
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
        let message = signed_message(message_id, timestamp, body)?;
        let mut signatures = String::from("v1,");
        signatures.push_str(&STANDARD.encode(hmac::sign(&self.active.0, &message).as_ref()));
        if let Some(previous) = &self.previous {
            signatures.push_str(" v1,");
            signatures.push_str(&STANDARD.encode(hmac::sign(&previous.0, &message).as_ref()));
        }
        Ok(signatures)
    }

    /// Verify signed headers and raw body against this key ring.
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
        verify(self, headers, body, now)
    }
}

impl fmt::Debug for KeyRing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KeyRing([REDACTED])")
    }
}

/// A verified Standard Webhooks identity.
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

    /// The signed timestamp after canonical decimal parsing.
    #[must_use]
    pub const fn timestamp(&self) -> i64 {
        self.timestamp
    }
}

/// Closed reasons for key construction or webhook verification failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// The configured key is not decodable Standard Webhooks base64 or is empty.
    #[error("webhook signing key is invalid")]
    InvalidKey,
    /// The raw body exceeds [`MAX_BODY_BYTES`].
    #[error("webhook body exceeds the maximum size")]
    BodyTooLarge,
    /// A required identity, timestamp, or signature header is absent.
    #[error("webhook signature evidence is incomplete")]
    MissingHeader,
    /// Repeated identity or timestamp headers disagree.
    #[error("webhook signature evidence is ambiguous")]
    ConflictingHeader,
    /// The message identifier is empty or contains the signed delimiter.
    #[error("webhook message identifier is invalid")]
    InvalidMessageId,
    /// The timestamp is not a signed i64 decimal value.
    #[error("webhook timestamp is invalid")]
    InvalidTimestamp,
    /// The signed timestamp is outside the permitted clock window.
    #[error("webhook timestamp is outside the permitted clock window")]
    TimestampOutOfWindow,
    /// No supplied v1 signature matched a configured key.
    #[error("webhook signature is invalid")]
    InvalidSignature,
}

/// Verify the Standard Webhooks v1 evidence carried by `headers` and `body`.
///
/// The function bounds the raw body before inspecting any header or parsing any
/// higher-level representation. It accepts any valid v1 candidate across the
/// active and optional predecessor keys.
///
/// # Errors
///
/// Returns a closed [`ProtocolError`] with no submitted or secret material.
pub fn verify(
    keys: &KeyRing,
    headers: &HeaderMap,
    body: &[u8],
    now: SystemTime,
) -> Result<VerifiedMessage, ProtocolError> {
    ensure_body_size(body)?;
    let message_id = single_header(headers, "webhook-id")?;
    ensure_message_id(message_id.as_bytes())?;
    let timestamp = parse_timestamp(single_header(headers, "webhook-timestamp")?)?;
    ensure_timestamp_window(timestamp, now)?;
    let message = signed_message(message_id.as_bytes(), timestamp, body)?;

    let mut saw_signature = false;
    for value in headers.get_all("webhook-signature").iter() {
        saw_signature = true;
        if signature_matches(&keys.active, &message, value)
            || keys
                .previous
                .as_ref()
                .is_some_and(|key| signature_matches(key, &message, value))
        {
            return Ok(VerifiedMessage {
                message_id: Bytes::copy_from_slice(message_id.as_bytes()),
                timestamp,
            });
        }
    }
    if !saw_signature {
        return Err(ProtocolError::MissingHeader);
    }
    Err(ProtocolError::InvalidSignature)
}

fn ensure_body_size(body: &[u8]) -> Result<(), ProtocolError> {
    (body.len() <= MAX_BODY_BYTES)
        .then_some(())
        .ok_or(ProtocolError::BodyTooLarge)
}

fn single_header<'a>(
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

fn ensure_message_id(message_id: &[u8]) -> Result<(), ProtocolError> {
    (!message_id.is_empty() && !message_id.contains(&b'.'))
        .then_some(())
        .ok_or(ProtocolError::InvalidMessageId)
}

fn parse_timestamp(value: &HeaderValue) -> Result<i64, ProtocolError> {
    value
        .to_str()
        .ok()
        .and_then(|raw| raw.parse().ok())
        .ok_or(ProtocolError::InvalidTimestamp)
}

fn ensure_timestamp_window(timestamp: i64, now: SystemTime) -> Result<(), ProtocolError> {
    let now = match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::from(duration.as_secs()),
        Err(error) => -i128::from(error.duration().as_secs()),
    };
    (i128::from(timestamp).abs_diff(now) <= TIMESTAMP_TOLERANCE_SECONDS as u128)
        .then_some(())
        .ok_or(ProtocolError::TimestampOutOfWindow)
}

fn signed_message(
    message_id: &[u8],
    timestamp: i64,
    body: &[u8],
) -> Result<Vec<u8>, ProtocolError> {
    ensure_body_size(body)?;
    ensure_message_id(message_id)?;
    let timestamp = timestamp.to_string();
    let capacity = message_id
        .len()
        .checked_add(timestamp.len())
        .and_then(|size| size.checked_add(body.len()))
        .and_then(|size| size.checked_add(2))
        .ok_or(ProtocolError::BodyTooLarge)?;
    let mut message = Vec::with_capacity(capacity);
    message.extend_from_slice(message_id);
    message.push(b'.');
    message.extend_from_slice(timestamp.as_bytes());
    message.push(b'.');
    message.extend_from_slice(body);
    Ok(message)
}

fn signature_matches(key: &SigningKey, message: &[u8], value: &HeaderValue) -> bool {
    value
        .as_bytes()
        .split(u8::is_ascii_whitespace)
        .any(|candidate| {
            candidate
                .strip_prefix(SIGNATURE_PREFIX)
                .and_then(|encoded| STANDARD.decode(encoded).ok())
                .is_some_and(|tag| hmac::verify(&key.0, message, &tag).is_ok())
        })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use http::{HeaderMap, HeaderValue};

    use super::{KeyRing, MAX_BODY_BYTES, ProtocolError};

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
    fn accepts_a_valid_v1_candidate_even_with_other_versions_or_bad_candidates() {
        let keys = KeyRing::from_encoded(ARBITRARY_BYTES_KEY, None).unwrap();
        let body = b"raw\0bytes";
        let good = keys.signatures(b"msg_1", 1, body).unwrap();
        let headers = headers("msg_1", "+0001", &format!("v2,nope {} v1,not-base64", good));

        assert!(
            keys.verify(&headers, body, UNIX_EPOCH + Duration::from_secs(1))
                .is_ok()
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
}
