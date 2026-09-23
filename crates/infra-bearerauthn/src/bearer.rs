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
/// Scheme matching is ASCII case-insensitive. The separator is one or more
/// ASCII spaces; tabs, leading/trailing whitespace, control bytes, non-ASCII
/// values, and any grammar outside RFC 6750 are malformed.
///
/// # Errors
///
/// Returns the fixed failure class for a missing, malformed, or oversized
/// bearer envelope.
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

    let Some((scheme, token)) = split_scheme(header) else {
        return Err(Failure::Malformed);
    };
    if !scheme.eq_ignore_ascii_case(b"Bearer") || !valid_token(token) {
        return Err(Failure::Malformed);
    }
    if token.len() > 32 * 1024 {
        return Err(Failure::Oversize);
    }

    Ok(BearerToken { bytes: token })
}

fn split_scheme(header: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = header.iter().position(|byte| *byte == b' ')?;
    if separator == 0 {
        return None;
    }
    let token_start = header[separator..].iter().position(|byte| *byte != b' ')? + separator;
    let scheme = &header[..separator];
    let token = &header[token_start..];
    if token.is_empty()
        || header
            .iter()
            .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
    {
        return None;
    }
    Some((scheme, token))
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
            b"Basic abc".as_slice(),
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
    fn rejects_token_over_the_32_kib_boundary_before_engines() {
        let at_limit = format!("Bearer {}", "a".repeat(32 * 1024));
        let over_limit = format!("Bearer {}", "a".repeat(32 * 1024 + 1));

        assert!(parse_bearer([at_limit.as_bytes()]).is_ok());
        assert_eq!(
            parse_bearer([over_limit.as_bytes()]),
            Err(Failure::Oversize)
        );
    }
}
