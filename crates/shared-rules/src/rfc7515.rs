//! JSON Web Signature, RFC 7515.

/// The parts of a JWS in compact serialization (RFC 7515 §7.1),
/// `header.payload.signature`, each still base64url-encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Compact<'a> {
    /// `header.payload`: the bytes the signature covers.
    pub signing_input: &'a [u8],
    pub header: &'a [u8],
    pub payload: &'a [u8],
    pub signature: &'a [u8],
}

/// Splits `token` at its last two dots; `None` with fewer than two. Any
/// further dot stays in the header, which then fails base64url decoding.
#[must_use]
pub fn compact(token: &[u8]) -> Option<Compact<'_>> {
    let (signing_input, signature) = split_last_dot(token)?;
    let (header, payload) = split_last_dot(signing_input)?;
    Some(Compact {
        signing_input,
        header,
        payload,
        signature,
    })
}

fn split_last_dot(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let dot = bytes.iter().rposition(|byte| *byte == b'.')?;
    Some((&bytes[..dot], &bytes[dot + 1..]))
}

/// The `application/` subtype a `typ` header value names, as written; `None`
/// for a value under another top-level type. RFC 7515 §4.1.9 has a value
/// without `/` read as if `application/` preceded it. Use [`typ_is`] to
/// compare the subtype without regard to ASCII case.
#[must_use]
pub fn typ_subtype(typ: &str) -> Option<&str> {
    match typ.split_once('/') {
        None => Some(typ),
        Some((top, subtype))
            if top.eq_ignore_ascii_case("application") && !subtype.contains('/') =>
        {
            Some(subtype)
        }
        Some(_) => None,
    }
}

#[must_use]
pub fn typ_is(typ: &str, expected_subtype: &str) -> bool {
    typ_subtype(typ).is_some_and(|subtype| subtype.eq_ignore_ascii_case(expected_subtype))
}

#[cfg(test)]
mod tests {
    use super::{Compact, compact, typ_subtype};

    #[test]
    fn a_typ_names_its_application_subtype_with_or_without_the_prefix() {
        assert_eq!(typ_subtype("at+jwt"), Some("at+jwt"));
        assert_eq!(typ_subtype("application/at+jwt"), Some("at+jwt"));
        assert_eq!(typ_subtype("APPLICATION/JWT"), Some("JWT"));
        assert_eq!(typ_subtype(""), Some(""));
    }

    #[test]
    fn a_typ_under_another_top_level_type_names_no_application_subtype() {
        for typ in [
            "text/jwt",
            "application/at+jwt/x",
            "/jwt",
            "applications/jwt",
        ] {
            assert_eq!(typ_subtype(typ), None, "{typ}");
        }
    }

    #[test]
    fn a_compact_jws_has_three_parts_and_signs_the_first_two() {
        assert_eq!(
            compact(b"aGVhZA.Ym9keQ.c2ln"),
            Some(Compact {
                signing_input: b"aGVhZA.Ym9keQ",
                header: b"aGVhZA",
                payload: b"Ym9keQ",
                signature: b"c2ln",
            })
        );
        assert_eq!(
            compact(b".."),
            Some(Compact {
                signing_input: b".",
                header: b"",
                payload: b"",
                signature: b"",
            })
        );
    }

    #[test]
    fn fewer_than_two_dots_is_no_jws_and_extra_dots_stay_in_the_header() {
        assert_eq!(compact(b""), None);
        assert_eq!(compact(b"aGVhZA.Ym9keQ"), None);
        assert_eq!(
            compact(b"a.b.c.d").map(|parts| parts.header),
            Some(b"a.b".as_slice())
        );
    }
}
