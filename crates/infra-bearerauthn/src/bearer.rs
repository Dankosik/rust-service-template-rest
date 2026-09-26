//! Strict RFC 6750 bearer-envelope parsing.

use std::fmt;

use crate::Failure;

/// The accepted bearer token bytes. Its contents are intentionally opaque and
/// its debug form is redacted.
#[derive(Eq, PartialEq)]
pub struct BearerToken<'a> {
    bytes: &'a [u8],
}

impl fmt::Debug for BearerToken<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

impl<'a> BearerToken<'a> {
    pub(crate) fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// Parses exactly one Authorization header value as a bearer envelope.
///
/// Foreign schemes are missing Bearer evidence without credential parsing.
/// Bearer matching is ASCII case-insensitive. Its separator is one or more
/// ASCII spaces; tabs, leading/trailing whitespace, control bytes, non-ASCII
/// values, and any grammar outside RFC 6750 are malformed.
///
/// # Errors
///
/// Returns the fixed failure class for a missing or malformed bearer envelope.
/// The caller owns the ingress byte bound.
pub fn parse_bearer<'a>(
    headers: impl IntoIterator<Item = &'a [u8]>,
) -> Result<BearerToken<'a>, Failure> {
    let mut headers = headers.into_iter();
    let Some(header) = headers.next() else {
        return Err(Failure::Missing);
    };
    if headers.next().is_some() {
        return Err(Failure::Malformed);
    }

    let separator = header
        .iter()
        .position(|byte| *byte == b' ')
        .unwrap_or(header.len());
    let scheme = &header[..separator];
    if scheme.is_empty() || !scheme.iter().all(|byte| is_tchar(*byte)) {
        return Err(Failure::Malformed);
    }
    if !scheme.eq_ignore_ascii_case(b"Bearer") {
        return Err(Failure::Missing);
    }
    if header
        .iter()
        .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
    {
        return Err(Failure::Malformed);
    }
    let credentials = header[separator..].trim_ascii_start();
    if !valid_token(credentials) {
        return Err(Failure::Malformed);
    }
    Ok(BearerToken { bytes: credentials })
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn valid_token(token: &[u8]) -> bool {
    let mut padding = false;
    let mut value_bytes = 0_usize;
    token.iter().all(|byte| match *byte {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'+' | b'/'
            if !padding =>
        {
            value_bytes += 1;
            true
        }
        b'=' if value_bytes > 0 => {
            padding = true;
            true
        }
        _ => false,
    }) && value_bytes > 0
}

#[cfg(test)]
mod tests {
    use super::parse_bearer;
    use crate::Failure;

    #[test]
    fn accepts_case_insensitive_scheme_and_space_run() {
        let token = parse_bearer([b"bEaReR   abc.DEF_~+/=".as_slice()]).unwrap();

        assert_eq!(token.as_bytes(), b"abc.DEF_~+/=");
        assert_eq!(format!("{token:?}"), "BearerToken([REDACTED])");
    }

    #[test]
    fn distinguishes_missing_from_malformed_envelopes() {
        assert_eq!(parse_bearer(std::iter::empty()), Err(Failure::Missing));
        for value in [
            b" Bearer abc".as_slice(),
            b"Bearer abc ".as_slice(),
            b"Bearer\tabc".as_slice(),
            b"Bearer ".as_slice(),
            b"Bearer =".as_slice(),
            b"Bearer ===".as_slice(),
            b"Bearer ab=c".as_slice(),
            b"Bearer abc\x7f".as_slice(),
            b"Bearer \xff".as_slice(),
        ] {
            assert_eq!(parse_bearer([value]), Err(Failure::Malformed), "{value:?}");
        }
        assert_eq!(
            parse_bearer([b"Bearer one".as_slice(), b"Bearer two".as_slice()]),
            Err(Failure::Malformed)
        );
    }

    #[test]
    fn foreign_scheme_credentials_are_not_interpreted() {
        for value in [
            b"Basic abc".as_slice(),
            b"Digest realm=\"api\", nonce=\"xyz\"",
            b"Negotiate",
            b"Digest realm=\"unterminated",
            b"Basic abc\n",
        ] {
            assert_eq!(parse_bearer([value]), Err(Failure::Missing), "{value:?}");
        }
        assert_eq!(
            parse_bearer([b"B@d abc".as_slice()]),
            Err(Failure::Malformed)
        );
    }

    #[test]
    fn accepts_tokens_beyond_the_removed_authentication_cap() {
        let header = format!("Bearer {}", "a".repeat(32 * 1024 + 1));
        assert!(parse_bearer([header.as_bytes()]).is_ok());
    }
}
