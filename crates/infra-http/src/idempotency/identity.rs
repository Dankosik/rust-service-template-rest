use std::borrow::Cow;

use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderName};
use infra_idempotency_store::{CallerIdentity, CallerKind, RequestFingerprint};
use itertools::Itertools as _;
use sha2::{Digest as _, Sha256};

use super::CommandInput;

pub(super) const MAX_DECODED_KEY_BYTES: usize = 255;

pub(super) const KEY_GRAMMAR: &str = "Send an IETF Structured Field string in double quotes. Parameters on the string do not change the key. Unquoted visible ASCII is also accepted for compatibility.";

const REQUEST_DOMAIN: &[u8] = b"http-idempotency/request/v1";
const REQUEST_WITH_HEADERS_DOMAIN: &[u8] = b"http-idempotency/request/v2";

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

pub(super) fn decode_key<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Option<Cow<'a, [u8]>> {
    let value = values.into_iter().exactly_one().ok()?;
    let value = str::from_utf8(value)
        .ok()?
        .trim_matches([' ', '\t'])
        .as_bytes();
    let decoded = if value.first() == Some(&b'"') {
        let item = sfv::Parser::new(value)
            .parse_item::<sfv::BareItemFromInput<'_>>()
            .ok()?;
        let sfv::BareItemFromInput::String(value) = item else {
            return None;
        };
        match value {
            Cow::Borrowed(value) => Cow::Borrowed(value.as_str().as_bytes()),
            Cow::Owned(value) => Cow::Owned(String::from(value).into_bytes()),
        }
    } else if value.iter().all(u8::is_ascii_graphic) {
        Cow::Borrowed(value)
    } else {
        return None;
    };
    (1..=MAX_DECODED_KEY_BYTES)
        .contains(&decoded.len())
        .then_some(decoded)
}

pub(super) fn request_fingerprint(
    input: &CommandInput,
    fingerprint_headers: &[HeaderName],
) -> RequestFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(if fingerprint_headers.is_empty() {
        REQUEST_DOMAIN
    } else {
        REQUEST_WITH_HEADERS_DOMAIN
    });
    hasher.update([0]);
    frame(&mut hasher, input.method().as_str().as_bytes());
    frame(&mut hasher, input.raw_path().as_bytes());
    match input.raw_query() {
        Some(query) => {
            hasher.update([1]);
            frame(&mut hasher, query.as_bytes());
        }
        None => hasher.update([0]),
    }
    frame_header_values(&mut hasher, input.headers(), &CONTENT_TYPE);
    if !fingerprint_headers.is_empty() {
        hasher.update((fingerprint_headers.len() as u64).to_be_bytes());
        for name in fingerprint_headers {
            frame(&mut hasher, name.as_str().as_bytes());
            frame_header_values(&mut hasher, input.headers(), name);
        }
    }
    frame(&mut hasher, input.body());
    let trailers = input.trailers();
    if !trailers.is_empty() {
        hasher.update(b"trailers/v1\0");
        let mut names = trailers
            .keys()
            .collect::<smallvec::SmallVec<[&HeaderName; 4]>>();
        names.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
        for name in names {
            for value in trailers.get_all(name) {
                frame(&mut hasher, name.as_str().as_bytes());
                frame(&mut hasher, value.as_bytes());
            }
        }
    }
    RequestFingerprint::from_bytes(hasher.finalize().into())
}

fn frame_header_values(hasher: &mut Sha256, headers: &HeaderMap, name: &HeaderName) {
    let values = headers.get_all(name);
    let iter = values.iter();
    let (lower, upper) = iter.size_hint();
    let count = if upper == Some(lower) {
        lower
    } else {
        iter.count()
    };
    hasher.update((count as u64).to_be_bytes());
    for value in values {
        frame(hasher, value.as_bytes());
    }
}

fn frame(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;
    use axum::http::{HeaderValue, Method, Request, Uri};
    use futures_util::FutureExt as _;
    use http_body_util::{BodyExt as _, Full};
    use infra_idempotency_store::Digest;

    use super::*;

    fn legacy_fingerprint(
        method: &Method,
        uri: &Uri,
        content_types: &[&[u8]],
        body: &[u8],
        trailers: Option<&HeaderMap>,
    ) -> Digest {
        let mut request = Request::builder().method(method).uri(uri).body(()).unwrap();
        for value in content_types {
            request
                .headers_mut()
                .append(CONTENT_TYPE, HeaderValue::from_bytes(value).unwrap());
        }
        let frames = Full::new(Bytes::copy_from_slice(body))
            .with_trailers(std::future::ready(trailers.cloned().map(Ok)));
        let collected = frames
            .collect()
            .now_or_never()
            .expect("fixture body is ready")
            .unwrap();
        *CommandInput::capture(request.into_parts().0, &[], collected)
            .1
            .as_bytes()
    }

    fn decode_digest(hex: &str) -> Digest {
        const_hex::decode_to_array(hex).expect("64 hex digits")
    }

    #[test]
    fn key_decoding_accepts_raw_and_structured_string_forms() {
        std::assert_matches!(
            decode_key([b" \ta/b=\t".as_slice()]),
            Some(Cow::Borrowed(b"a/b="))
        );
        std::assert_matches!(
            decode_key([b" \t\"a b\\\\c\"\t ".as_slice()]),
            Some(Cow::Owned(value)) if value.as_slice() == b"a b\\c"
        );
        std::assert_matches!(
            decode_key([b"\"same\";flag;choice=1;choice=\"last\"".as_slice()]),
            Some(Cow::Borrowed(b"same"))
        );
    }

    #[test]
    fn key_decoding_refuses_ambiguous_or_invalid_forms() {
        for value in [
            b"".as_slice(),
            b"\"unterminated".as_slice(),
            b"\"key\";a=".as_slice(),
            b"\"key\" trailing".as_slice(),
            b"\x7f".as_slice(),
            b"\rkey".as_slice(),
            b"key\n".as_slice(),
            b"\x0ckey".as_slice(),
            b"key\xff".as_slice(),
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
            infra_idempotency_store::ScopeKey::new(caller, b"k-123").digest(),
            &decode_digest("98df4043b9e795011fe4f84c3844bcfe9ade67032cc0044471d5097f7a30ad94")
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
        assert_eq!(
            fingerprint(Some(&first)),
            decode_digest("39fc017a345d7c3b438fb1442422d26a7c7e886c177537a474fdc79f5b975b8f")
        );
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
            let decoded = decode_key([field.as_bytes()]);
            proptest::prop_assert_eq!(decoded.as_deref(), Some(key.as_bytes()));
        }
    }
}
