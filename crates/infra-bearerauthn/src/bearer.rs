//! Strict RFC 6750 bearer-envelope parsing.

use shared_rules::rfc6750::{self, NoBearer};

use crate::Failure;

/// The accepted bearer token text. Its contents are intentionally opaque and
/// its debug form is redacted.
#[derive(Eq, PartialEq, derive_more::Debug)]
#[debug("BearerToken([REDACTED])")]
pub struct BearerToken<'a> {
    text: &'a str,
}

impl<'a> BearerToken<'a> {
    pub(crate) fn as_bytes(&self) -> &'a [u8] {
        self.text.as_bytes()
    }

    /// The presented token text, for a verified engine to retain on its principal.
    pub(crate) fn to_secret(&self) -> secrecy::SecretString {
        self.text.to_owned().into()
    }
}

/// Parses exactly one Authorization header value as a bearer envelope, by
/// [`shared_rules::rfc6750::token`]: foreign schemes are missing Bearer
/// evidence without credential parsing, and anything outside RFC 6750 is
/// malformed.
///
/// # Errors
///
/// Returns the fixed failure class for a missing or malformed bearer envelope.
/// The caller owns the ingress byte bound.
pub fn parse_bearer<'a>(
    headers: impl IntoIterator<Item = &'a [u8]>,
) -> Result<BearerToken<'a>, Failure> {
    match rfc6750::token(headers) {
        Ok(text) => Ok(BearerToken { text }),
        Err(NoBearer::Missing) => Err(Failure::Missing),
        Err(NoBearer::Malformed) => Err(Failure::Malformed),
    }
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

    /// The grammar's cases are `shared_rules::rfc6750`'s; this crate maps its
    /// two refusals onto distinct failure classes.
    #[test]
    fn a_missing_envelope_is_not_a_malformed_one() {
        assert_eq!(parse_bearer(std::iter::empty()), Err(Failure::Missing));
        assert_eq!(
            parse_bearer([b"Basic abc".as_slice()]),
            Err(Failure::Missing)
        );
        assert_eq!(
            parse_bearer([b"Bearer ab=c".as_slice()]),
            Err(Failure::Malformed)
        );
        assert_eq!(
            parse_bearer([b"Bearer one".as_slice(), b"Bearer two".as_slice()]),
            Err(Failure::Malformed)
        );
    }

    #[test]
    fn accepts_tokens_beyond_the_removed_authentication_cap() {
        let header = format!("Bearer {}", "a".repeat(32 * 1024 + 1));
        assert!(parse_bearer([header.as_bytes()]).is_ok());
    }
}
