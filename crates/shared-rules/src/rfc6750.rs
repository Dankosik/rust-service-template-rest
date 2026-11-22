use crate::rfc9110::is_tchar;

const BEARER_SCHEME: &[u8] = b"Bearer";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoBearer {
    Missing,
    Malformed,
}

/// RFC 6750 §2.1
pub fn token_bytes<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Result<&'a [u8], NoBearer> {
    let mut values = values.into_iter();
    let value = values.next().ok_or(NoBearer::Missing)?;
    if values.next().is_some() {
        return Err(NoBearer::Malformed);
    }
    if let Some((prefix, rest)) = value.split_at_checked(BEARER_SCHEME.len())
        && prefix.eq_ignore_ascii_case(BEARER_SCHEME)
    {
        match rest.split_first() {
            Some((&b' ', after_space)) => {
                let mut token = after_space;
                while let Some(tail) = token.strip_prefix(b" ") {
                    token = tail;
                }
                return is_b64token(token)
                    .then_some(token)
                    .ok_or(NoBearer::Malformed);
            }
            None => return Err(NoBearer::Malformed),
            Some(_) => {}
        }
    }
    let other_scheme = match value.iter().enumerate().find(|&(_, &byte)| !is_tchar(byte)) {
        None => !value.is_empty(),
        Some((index, &byte)) => index > 0 && byte == b' ',
    };
    if other_scheme {
        Err(NoBearer::Missing)
    } else {
        Err(NoBearer::Malformed)
    }
}

/// RFC 6750 §2.1
#[must_use]
pub fn is_b64token(token: &[u8]) -> bool {
    let padding_len = token.iter().rev().take_while(|byte| **byte == b'=').count();
    let unpadded = &token[..token.len() - padding_len];
    !unpadded.is_empty()
        && unpadded.iter().fold(true, |valid, byte| {
            valid
                & (byte.is_ascii_alphanumeric()
                    | matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/'))
        })
}

#[cfg(test)]
mod tests {
    use super::{NoBearer, is_b64token, token_bytes};

    #[test]
    fn the_scheme_matches_any_case_and_spaces_separate_the_token() {
        assert_eq!(
            token_bytes([b"bEaReR   abc.DEF_~+/=".as_slice()]),
            Ok(b"abc.DEF_~+/=".as_slice())
        );
        assert_eq!(
            token_bytes([b"Bearer a.b.c".as_slice()]),
            Ok(b"a.b.c".as_slice())
        );
        let long = format!("Bearer {}", "a".repeat(64 * 1024));
        assert_eq!(
            token_bytes([long.as_bytes()]).map(<[u8]>::len),
            Ok(64 * 1024)
        );
    }

    #[test]
    fn b64token_admits_only_ascii_bytes_from_the_rfc6750_alphabet() {
        for byte in 0_u8..=u8::MAX {
            let expected = byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/');
            assert_eq!(is_b64token(&[byte]), expected, "{byte:#04x}");
            assert_eq!(
                is_b64token(&[b'a', byte]),
                expected || byte == b'=',
                "{byte:#04x}"
            );
            if is_b64token(&[byte]) || is_b64token(&[b'a', byte]) {
                assert!(byte.is_ascii(), "{byte:#04x}");
            }
        }
    }

    #[test]
    fn no_value_or_another_scheme_is_missing_and_never_read() {
        assert_eq!(token_bytes(std::iter::empty()), Err(NoBearer::Missing));
        for value in [
            b"Basic abc".as_slice(),
            b"Digest realm=\"api\", nonce=\"xyz\"",
            b"Negotiate",
            b"Digest realm=\"unterminated",
            b"Basic abc\n",
            b"BearerX abc",
            b"BearerX",
        ] {
            assert_eq!(token_bytes([value]), Err(NoBearer::Missing), "{value:?}");
        }
    }

    #[test]
    fn several_values_or_text_outside_the_grammar_is_malformed() {
        for value in [
            b"".as_slice(),
            b" ",
            b"Bearer",
            b"bEaReR",
            b"Bearer ",
            b"Bearer   ",
            b" Bearer abc",
            b"Bearer abc ",
            b"Bearer a b",
            b"Bearer\tabc",
            b"Bearer \tabc",
            b"Bearer \nabc",
            b"Bearer =",
            b"Bearer ===",
            b"Bearer ab=c",
            b"Bearer abc\x7f",
            b"Bearer \xff",
            b"Bearer@ abc",
            b"B@d abc",
        ] {
            assert_eq!(token_bytes([value]), Err(NoBearer::Malformed), "{value:?}");
        }
        assert_eq!(
            token_bytes([b"Bearer one".as_slice(), b"Bearer two".as_slice()]),
            Err(NoBearer::Malformed)
        );
        assert_eq!(
            token_bytes([b"Basic one".as_slice(), b"Basic two".as_slice()]),
            Err(NoBearer::Malformed)
        );
    }
}
