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
    effective_token_bound: usize,
) -> Result<BearerToken<'a>, Failure> {
    let mut headers = headers.into_iter();
    let Some(header) = headers.next() else {
        return Err(Failure::Missing);
    };
    if headers.next().is_some() {
        return Err(Failure::Malformed);
    }

    if header.is_empty()
        || header
            .iter()
            .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
    {
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
    let credentials = header[separator..].trim_ascii_start();
    if !scheme.eq_ignore_ascii_case(b"Bearer") {
        return if separator == header.len()
            || valid_token(credentials)
            || valid_auth_parameters(credentials)
        {
            Err(Failure::Missing)
        } else {
            Err(Failure::Malformed)
        };
    }
    if credentials.len() > effective_token_bound.min(32 * 1024) {
        return Err(Failure::Oversize);
    }
    if !valid_token(credentials) {
        return Err(Failure::Malformed);
    }
    Ok(BearerToken { bytes: credentials })
}

fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

// Unsupported schemes may use token68 or the HTTP authentication parameter
// grammar; applying Bearer's token grammar to Digest would misclassify it.
fn valid_auth_parameters(mut bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.last() == Some(&b' ') {
        return false;
    }
    loop {
        let name_end = bytes
            .iter()
            .position(|byte| !is_tchar(*byte))
            .unwrap_or(bytes.len());
        if name_end == 0 {
            return false;
        }
        bytes = bytes[name_end..].trim_ascii_start();
        let Some(rest) = bytes.strip_prefix(b"=") else {
            return false;
        };
        bytes = rest.trim_ascii_start();
        if let Some(rest) = bytes.strip_prefix(b"\"") {
            let mut escaped = false;
            let mut end = None;
            for (index, byte) in rest.iter().copied().enumerate() {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    end = Some(index + 1);
                    break;
                }
            }
            let Some(end) = end else {
                return false;
            };
            bytes = &rest[end..];
        } else {
            let end = bytes
                .iter()
                .position(|byte| !is_tchar(*byte))
                .unwrap_or(bytes.len());
            if end == 0 {
                return false;
            }
            bytes = &bytes[end..];
        }
        bytes = bytes.trim_ascii_start();
        if bytes.is_empty() {
            return true;
        }
        let Some(rest) = bytes.strip_prefix(b",") else {
            return false;
        };
        bytes = rest.trim_ascii_start();
    }
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
        let token = parse_bearer([b"bEaReR   abc.DEF_~+/=".as_slice()], 32 * 1024).unwrap();

        assert_eq!(token.as_bytes(), b"abc.DEF_~+/=");
        assert_eq!(format!("{token:?}"), "BearerToken([REDACTED])");
    }

    #[test]
    fn distinguishes_missing_from_malformed_envelopes() {
        assert_eq!(
            parse_bearer(std::iter::empty(), 32 * 1024),
            Err(Failure::Missing)
        );
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
            assert_eq!(
                parse_bearer([value], 32 * 1024),
                Err(Failure::Malformed),
                "{value:?}"
            );
        }
        assert_eq!(
            parse_bearer(
                [b"Bearer one".as_slice(), b"Bearer two".as_slice()],
                32 * 1024
            ),
            Err(Failure::Malformed)
        );
    }

    #[test]
    fn repair_regression_unsupported_authentication_scheme_is_missing_bearer() {
        for value in [
            b"Basic abc".as_slice(),
            b"Digest realm=\"api\", nonce=\"xyz\"",
            b"Negotiate",
        ] {
            assert_eq!(
                parse_bearer([value], 32 * 1024),
                Err(Failure::Missing),
                "{value:?}"
            );
        }
        for value in [
            b"B@d abc".as_slice(),
            b"Basic abc\n",
            b"Digest realm=\"unterminated",
        ] {
            assert_eq!(
                parse_bearer([value], 32 * 1024),
                Err(Failure::Malformed),
                "{value:?}"
            );
        }
    }

    #[test]
    fn rejects_token_over_the_32_kib_boundary_before_engines() {
        let at_limit = format!("Bearer {}", "a".repeat(32 * 1024));
        let over_limit = format!("Bearer {}", "a".repeat(32 * 1024 + 1));

        assert!(parse_bearer([at_limit.as_bytes()], 32 * 1024).is_ok());
        assert_eq!(
            parse_bearer([over_limit.as_bytes()], 32 * 1024),
            Err(Failure::Oversize)
        );
    }
}
