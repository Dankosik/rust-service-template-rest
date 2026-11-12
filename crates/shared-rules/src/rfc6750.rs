//! OAuth 2.0 Bearer token usage, RFC 6750.

/// Why a request's `Authorization` values carry no usable Bearer token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoBearer {
    /// No value, or a scheme other than `Bearer`, whose credentials are never
    /// read.
    Missing,
    /// More than one value, or a `Bearer` value outside the grammar.
    Malformed,
}

/// The token of exactly one `Authorization` value of the form
/// `Bearer 1*SP b64token` (RFC 6750 §2.1).
///
/// The scheme matches ASCII case-insensitively. Tabs, leading or trailing
/// whitespace, control bytes, and non-ASCII bytes are malformed. The caller
/// bounds how many bytes it passes.
///
/// # Errors
///
/// Returns [`NoBearer::Missing`] without a value or for another scheme, and
/// [`NoBearer::Malformed`] for several values or a value outside the grammar.
pub fn token<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Result<&'a str, NoBearer> {
    let mut values = values.into_iter();
    let value = values.next().ok_or(NoBearer::Missing)?;
    if values.next().is_some() {
        return Err(NoBearer::Malformed);
    }
    let separator = value
        .iter()
        .position(|byte| *byte == b' ')
        .unwrap_or(value.len());
    let (scheme, rest) = value.split_at(separator);
    if scheme.is_empty() || !scheme.iter().all(|byte| is_tchar(*byte)) {
        return Err(NoBearer::Malformed);
    }
    if !scheme.eq_ignore_ascii_case(b"Bearer") {
        return Err(NoBearer::Missing);
    }
    let start = rest
        .iter()
        .position(|byte| *byte != b' ')
        .unwrap_or(rest.len());
    let token = &rest[start..];
    if !is_b64token(token) {
        return Err(NoBearer::Malformed);
    }
    // The b64token alphabet is ASCII, so this conversion cannot fail.
    std::str::from_utf8(token).map_err(|_| NoBearer::Malformed)
}

/// RFC 9110 `tchar`, the characters of an authentication scheme.
fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// `b64token = 1*( ALPHA / DIGIT / "-" / "." / "_" / "~" / "+" / "/" ) *"="`.
fn is_b64token(token: &[u8]) -> bool {
    let padding = token.iter().rev().take_while(|byte| **byte == b'=').count();
    let value = &token[..token.len() - padding];
    // A non-short-circuiting fold lets the compiler vectorize the class check;
    // tokens are checked on every authenticated request.
    !value.is_empty()
        && value.iter().fold(true, |valid, byte| {
            valid
                & (byte.is_ascii_alphanumeric()
                    | matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/'))
        })
}

#[cfg(test)]
mod tests {
    use super::{NoBearer, token};

    #[test]
    fn the_scheme_matches_any_case_and_spaces_separate_the_token() {
        assert_eq!(
            token([b"bEaReR   abc.DEF_~+/=".as_slice()]),
            Ok("abc.DEF_~+/=")
        );
        assert_eq!(token([b"Bearer a.b.c".as_slice()]), Ok("a.b.c"));
        let long = format!("Bearer {}", "a".repeat(64 * 1024));
        assert_eq!(token([long.as_bytes()]).map(str::len), Ok(64 * 1024));
    }

    #[test]
    fn no_value_or_another_scheme_is_missing_and_never_read() {
        assert_eq!(token(std::iter::empty()), Err(NoBearer::Missing));
        for value in [
            b"Basic abc".as_slice(),
            b"Digest realm=\"api\", nonce=\"xyz\"",
            b"Negotiate",
            b"Digest realm=\"unterminated",
            b"Basic abc\n",
        ] {
            assert_eq!(token([value]), Err(NoBearer::Missing), "{value:?}");
        }
    }

    #[test]
    fn several_values_or_text_outside_the_grammar_is_malformed() {
        for value in [
            b"Bearer".as_slice(),
            b"Bearer ",
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
            b"B@d abc",
        ] {
            assert_eq!(token([value]), Err(NoBearer::Malformed), "{value:?}");
        }
        assert_eq!(
            token([b"Bearer one".as_slice(), b"Bearer two".as_slice()]),
            Err(NoBearer::Malformed)
        );
    }
}
