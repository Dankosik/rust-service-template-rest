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

#[cfg(test)]
mod tests {
    use super::{Compact, compact};

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
