//! Private request identity framing for HTTP idempotency.

use axum::http::{Method, Uri};
use infra_idempotency_store::{CallerIdentity, CallerKind, Digest, ScopeKey};
use sha2::{Digest as _, Sha256};

/// The longest decoded key in bytes.
pub(super) const MAX_KEY_BYTES: usize = 255;

const SCOPE_DOMAIN: &[u8] = b"http-idempotency/scope/v2";
const REQUEST_DOMAIN: &[u8] = b"http-idempotency/request/v1";

/// Select the sealed verified caller that owns a key.
pub(super) fn caller_identity(
    issuer: &str,
    subject: Option<&str>,
    client_id: Option<&str>,
) -> Option<CallerIdentity> {
    let (kind, value) = match (subject, client_id) {
        (Some(subject), _) => (CallerKind::Subject, subject),
        (None, Some(client_id)) => (CallerKind::Client, client_id),
        (None, None) => return None,
    };
    Some(CallerIdentity {
        issuer: issuer.to_owned(),
        kind,
        value: value.to_owned(),
    })
}

/// Decode exactly one raw Idempotency-Key field value.
///
/// An initial quote selects the Structured Fields grammar permanently. Other
/// values keep the compatible visible-ASCII grammar. Neither branch exposes
/// the raw input to diagnostics or callers.
pub(super) fn valid_key<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Option<Vec<u8>> {
    let mut values = values.into_iter();
    let value = trim_ows(values.next()?);
    if values.next().is_some() {
        return None;
    }
    let decoded = if value.first() == Some(&b'"') {
        let item = sfv::Parser::new(value).parse::<sfv::Item>().ok()?;
        let sfv::BareItem::String(value) = item.bare_item else {
            return None;
        };
        if !item.params.is_empty() {
            return None;
        }
        value.as_str().as_bytes().to_vec()
    } else if value.iter().all(|byte| (0x21..=0x7e).contains(byte)) {
        value.to_vec()
    } else {
        return None;
    };
    (1..=MAX_KEY_BYTES)
        .contains(&decoded.len())
        .then_some(decoded)
}

/// Scope one decoded key to the verified caller.
pub(super) fn scope_key(caller: &CallerIdentity, key: &[u8]) -> Option<ScopeKey> {
    let mut hasher = Sha256::new();
    hasher.update(SCOPE_DOMAIN);
    hasher.update([0]);
    frame(&mut hasher, caller.issuer.as_bytes())?;
    hasher.update([caller_tag(caller.kind)]);
    frame(&mut hasher, caller.value.as_bytes())?;
    frame(&mut hasher, key)?;
    Some(ScopeKey::from_digest(hasher.finalize().into()))
}

/// Fingerprint exactly the client-visible request tuple that the seam owns.
pub(super) fn request_digest(
    method: &Method,
    uri: &Uri,
    content_types: &[Vec<u8>],
    body: &[u8],
) -> Option<Digest> {
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_DOMAIN);
    hasher.update([0]);
    frame(&mut hasher, method.as_str().as_bytes())?;
    frame(&mut hasher, uri.path().as_bytes())?;
    match uri.query() {
        Some(query) => {
            hasher.update([1]);
            frame(&mut hasher, query.as_bytes())?;
        }
        None => hasher.update([0]),
    }
    let count = u64::try_from(content_types.len()).ok()?;
    hasher.update(count.to_be_bytes());
    for value in content_types {
        frame(&mut hasher, value)?;
    }
    frame(&mut hasher, body)?;
    Some(hasher.finalize().into())
}

fn trim_ows(value: &[u8]) -> &[u8] {
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|byte| !matches!(byte, b' ' | b'\t'))
        .map_or(start, |index| index + 1);
    &value[start..end]
}

const fn caller_tag(kind: CallerKind) -> u8 {
    match kind {
        CallerKind::Subject => 1,
        CallerKind::Client => 2,
    }
}

fn frame(hasher: &mut Sha256, bytes: &[u8]) -> Option<()> {
    let length = u64::try_from(bytes.len()).ok()?;
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Some(())
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, Uri};

    use super::*;

    fn decode_digest(hex: &str) -> Digest {
        let mut digest = [0; 32];
        for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
            let text = std::str::from_utf8(pair).expect("ASCII hex pair");
            *byte = u8::from_str_radix(text, 16).expect("hex digit");
        }
        digest
    }

    #[test]
    fn key_decoding_accepts_raw_and_structured_string_forms() {
        assert_eq!(valid_key([b"a/b=".as_slice()]), Some(b"a/b=".to_vec()));
        assert_eq!(
            valid_key([b" \t\"a b\\\\c\"\t ".as_slice()]),
            Some(b"a b\\c".to_vec())
        );
        assert_eq!(
            valid_key([b"\"same\"".as_slice()]),
            valid_key([b"same".as_slice()])
        );
    }

    #[test]
    fn key_decoding_refuses_ambiguous_or_invalid_forms() {
        for value in [
            b"".as_slice(),
            b"\"unterminated".as_slice(),
            b"\"key\";a=1".as_slice(),
            b"\"key\" trailing".as_slice(),
            b"\x7f".as_slice(),
            b"\xc3\xa9".as_slice(),
        ] {
            assert_eq!(valid_key([value]), None, "{value:?}");
        }
        assert_eq!(valid_key([b"first".as_slice(), b"second".as_slice()]), None);
        assert_eq!(valid_key([vec![b'a'; MAX_KEY_BYTES + 1].as_slice()]), None);
    }

    #[test]
    fn scope_and_request_match_the_pinned_framing_vectors() {
        let caller = caller_identity(
            "https://issuer.example",
            Some("fixture-subject"),
            Some("client"),
        )
        .expect("verified caller");
        assert_eq!(
            scope_key(&caller, b"k-123"),
            Some(ScopeKey::from_digest(decode_digest(
                "98df4043b9e795011fe4f84c3844bcfe9ade67032cc0044471d5097f7a30ad94"
            )))
        );
        assert_eq!(
            request_digest(
                &Method::POST,
                &"/widgets/%2F?a=1&b=2".parse::<Uri>().expect("URI"),
                &[b"application/json".to_vec()],
                br#"{"n":1}"#,
            ),
            Some(decode_digest(
                "2e4fb57a46095c45e886d290707bf645412366966bf10009314001fd087eb30b"
            ))
        );
    }

    #[test]
    fn request_framing_preserves_absence_order_and_exact_bytes() {
        let uri: Uri = "/widgets".parse().expect("URI");
        let baseline = request_digest(&Method::POST, &uri, &[], b"{}");
        assert_ne!(
            baseline,
            request_digest(
                &Method::POST,
                &"/widgets?".parse().expect("URI"),
                &[],
                b"{}"
            )
        );
        assert_ne!(
            baseline,
            request_digest(&Method::POST, &uri, &[Vec::new()], b"{}")
        );
        assert_ne!(baseline, request_digest(&Method::POST, &uri, &[], b"{ }"));
    }
}
