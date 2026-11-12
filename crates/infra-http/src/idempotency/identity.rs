//! Private request identity framing for HTTP idempotency.
//!
//! Domain strings, caller tags, query-presence bytes, and length prefixes are
//! a persistent format: changing them changes identity against live records.
//! Preserve the exact framing pinned by the scope/request test vectors.

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName, Method, Uri};
use infra_idempotency_store::{CallerIdentity, CallerKind, Digest, ScopeKey};
use sha2::{Digest as _, Sha256};

pub(super) const MAX_DECODED_KEY_BYTES: usize = 255;

/// The accepted key grammar, shared by the generated parameter and the runtime
/// rejection.
pub(super) const KEY_GRAMMAR: &str = "An unquoted value is visible ASCII; a value beginning with `\"` is an IETF Structured Field string.";

const SCOPE_DOMAIN: &[u8] = b"http-idempotency/scope/v2";
const REQUEST_DOMAIN: &[u8] = b"http-idempotency/request/v1";
const REQUEST_WITH_HEADERS_DOMAIN: &[u8] = b"http-idempotency/request/v2";

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
pub(super) fn decode_key<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Option<Vec<u8>> {
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
    (1..=MAX_DECODED_KEY_BYTES)
        .contains(&decoded.len())
        .then_some(decoded)
}

/// Scope one decoded key to the verified caller.
pub(super) fn scope_key(caller: &CallerIdentity, key: &[u8]) -> ScopeKey {
    let mut hasher = Sha256::new();
    hasher.update(SCOPE_DOMAIN);
    hasher.update([0]);
    frame(&mut hasher, caller.issuer.as_bytes());
    hasher.update([caller_tag(caller.kind)]);
    frame(&mut hasher, caller.value.as_bytes());
    frame(&mut hasher, key);
    ScopeKey::from_digest(hasher.finalize().into())
}

/// Fingerprint exactly the client-visible request tuple that the seam owns.
pub(super) fn request_fingerprint(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    fingerprint_headers: &[HeaderName],
    body: &[u8],
    trailers: Option<&HeaderMap>,
) -> Digest {
    let mut hasher = Sha256::new();
    // Keep the old format for routes without extra headers. On other routes,
    // a live v1 record must refuse replay because it cannot prove equality.
    hasher.update(if fingerprint_headers.is_empty() {
        REQUEST_DOMAIN
    } else {
        REQUEST_WITH_HEADERS_DOMAIN
    });
    hasher.update([0]);
    frame(&mut hasher, method.as_str().as_bytes());
    frame(&mut hasher, uri.path().as_bytes());
    match uri.query() {
        Some(query) => {
            hasher.update([1]);
            frame(&mut hasher, query.as_bytes());
        }
        None => hasher.update([0]),
    }
    frame_header_values(&mut hasher, headers, &CONTENT_TYPE);
    if !fingerprint_headers.is_empty() {
        hasher.update((fingerprint_headers.len() as u64).to_be_bytes());
        for name in fingerprint_headers {
            frame(&mut hasher, name.as_str().as_bytes());
            frame_header_values(&mut hasher, headers, name);
        }
    }
    frame(&mut hasher, body);
    // An absent or empty trailer section keeps the persisted v1 fingerprint.
    if let Some(trailers) = trailers.filter(|trailers| !trailers.is_empty()) {
        hasher.update(b"trailers/v1\0");
        let mut fields = trailers.iter().collect::<Vec<_>>();
        // Stable sort removes ordering between field names while keeping the
        // order of repeated values for each name.
        fields.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        for (name, value) in fields {
            frame(&mut hasher, name.as_str().as_bytes());
            frame(&mut hasher, value.as_bytes());
        }
    }
    hasher.finalize().into()
}

fn frame_header_values(hasher: &mut Sha256, headers: &HeaderMap, name: &HeaderName) {
    let values = headers.get_all(name);
    hasher.update((values.iter().count() as u64).to_be_bytes());
    for value in values {
        frame(hasher, value.as_bytes());
    }
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

fn frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderValue, Method, Uri};

    use super::*;

    fn legacy_fingerprint(
        method: &Method,
        uri: &Uri,
        content_types: &[&[u8]],
        body: &[u8],
        trailers: Option<&HeaderMap>,
    ) -> Digest {
        let mut headers = HeaderMap::new();
        for value in content_types {
            headers.append(CONTENT_TYPE, HeaderValue::from_bytes(value).unwrap());
        }
        request_fingerprint(method, uri, &headers, &[], body, trailers)
    }

    fn decode_digest(hex: &str) -> Digest {
        let mut digest = [0; 32];
        const_hex::decode_to_slice(hex, &mut digest).expect("64 hex digits");
        digest
    }

    #[test]
    fn key_decoding_accepts_raw_and_structured_string_forms() {
        assert_eq!(decode_key([b"a/b=".as_slice()]), Some(b"a/b=".to_vec()));
        assert_eq!(
            decode_key([b" \t\"a b\\\\c\"\t ".as_slice()]),
            Some(b"a b\\c".to_vec())
        );
        assert_eq!(
            decode_key([b"\"same\"".as_slice()]),
            decode_key([b"same".as_slice()])
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
            assert_eq!(decode_key([value]), None, "{value:?}");
        }
        assert_eq!(
            decode_key([b"first".as_slice(), b"second".as_slice()]),
            None
        );
        assert_eq!(
            decode_key([vec![b'a'; MAX_DECODED_KEY_BYTES + 1].as_slice()]),
            None
        );
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
            ScopeKey::from_digest(decode_digest(
                "98df4043b9e795011fe4f84c3844bcfe9ade67032cc0044471d5097f7a30ad94"
            ))
        );
        assert_eq!(
            legacy_fingerprint(
                &Method::POST,
                &"/widgets/%2F?a=1&b=2".parse::<Uri>().expect("URI"),
                &[b"application/json".as_slice()],
                br#"{"n":1}"#,
                None,
            ),
            decode_digest("2e4fb57a46095c45e886d290707bf645412366966bf10009314001fd087eb30b")
        );
    }

    #[test]
    fn request_framing_preserves_absence_order_and_exact_bytes() {
        let uri: Uri = "/widgets".parse().expect("URI");
        let baseline = legacy_fingerprint(&Method::POST, &uri, &[], b"{}", None);
        assert_ne!(
            baseline,
            legacy_fingerprint(
                &Method::POST,
                &"/widgets?".parse().expect("URI"),
                &[],
                b"{}",
                None,
            )
        );
        assert_ne!(
            baseline,
            legacy_fingerprint(&Method::POST, &uri, &[b"".as_slice()], b"{}", None)
        );
        assert_ne!(
            baseline,
            legacy_fingerprint(&Method::POST, &uri, &[], b"{ }", None)
        );
        assert_ne!(
            legacy_fingerprint(
                &Method::POST,
                &uri,
                &[b"a".as_slice(), b"b".as_slice()],
                b"{}",
                None,
            ),
            legacy_fingerprint(
                &Method::POST,
                &uri,
                &[b"b".as_slice(), b"a".as_slice()],
                b"{}",
                None,
            )
        );
        assert_ne!(
            legacy_fingerprint(&Method::POST, &uri, &[b"ab".as_slice()], b"{}", None),
            legacy_fingerprint(
                &Method::POST,
                &uri,
                &[b"a".as_slice(), b"b".as_slice()],
                b"{}",
                None,
            )
        );
    }

    #[test]
    fn request_identity_distinguishes_trailer_values_but_ignores_field_name_order() {
        let uri: Uri = "/widgets".parse().expect("URI");
        let fingerprint = |trailers: Option<&HeaderMap>| {
            legacy_fingerprint(&Method::POST, &uri, &[], b"{}", trailers)
        };
        let mut first = HeaderMap::new();
        assert_eq!(fingerprint(None), fingerprint(Some(&first)));
        first.append("x-region", HeaderValue::from_static("east"));
        first.append("x-region", HeaderValue::from_static("west"));
        first.append("x-note", HeaderValue::from_static("note"));
        let mut reordered = HeaderMap::new();
        reordered.append("x-note", HeaderValue::from_static("note"));
        reordered.append("x-region", HeaderValue::from_static("east"));
        reordered.append("x-region", HeaderValue::from_static("west"));
        assert_eq!(fingerprint(Some(&first)), fingerprint(Some(&reordered)));
        assert_ne!(fingerprint(None), fingerprint(Some(&first)));

        reordered.insert("x-region", HeaderValue::from_static("west"));
        reordered.append("x-region", HeaderValue::from_static("east"));
        assert_ne!(fingerprint(Some(&first)), fingerprint(Some(&reordered)));

        reordered.insert("x-region", HeaderValue::from_static("east"));
        reordered.append("x-region", HeaderValue::from_static("south"));
        assert_ne!(fingerprint(Some(&first)), fingerprint(Some(&reordered)));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        #[test]
        fn an_admitted_key_round_trips_through_a_structured_field_string(
            key in proptest::collection::vec(0x20_u8..=0x7e, 1..=MAX_DECODED_KEY_BYTES),
        ) {
            let key = String::from_utf8(key).unwrap();
            let field = sfv::ItemSerializer::new()
                .bare_item(sfv::StringRef::from_str(&key).unwrap())
                .finish();
            proptest::prop_assert_eq!(
                decode_key([field.as_bytes()]),
                Some(key.into_bytes())
            );
        }
    }
}
