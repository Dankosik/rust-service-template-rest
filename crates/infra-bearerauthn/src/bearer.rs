use shared_rules::rfc6750::{self, NoBearer};

use crate::Failure;

#[derive(Eq, PartialEq, derive_more::Debug)]
#[debug("PresentedToken([REDACTED])")]
pub struct PresentedToken<'a> {
    text: &'a str,
}

impl<'a> PresentedToken<'a> {
    pub(crate) fn as_str(&self) -> &'a str {
        self.text
    }

    pub(crate) fn as_bytes(&self) -> &'a [u8] {
        self.text.as_bytes()
    }

    #[must_use]
    pub fn into_delegation_credential(self) -> DelegationCredential {
        DelegationCredential(secrecy::SecretString::from(self.as_str()).into())
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("DelegationCredential([REDACTED])")]
pub struct DelegationCredential(std::sync::Arc<secrecy::SecretString>);

impl DelegationCredential {
    #[must_use]
    pub fn access_token(&self) -> &secrecy::SecretString {
        &self.0
    }
}

/// RFC 6750 §2.1
pub(crate) fn parse_bearer<'a>(
    authorization: impl IntoIterator<Item = &'a [u8]>,
) -> Result<PresentedToken<'a>, Failure> {
    match rfc6750::token_bytes(authorization) {
        Ok(bytes) => std::str::from_utf8(bytes)
            .map(|text| PresentedToken { text })
            .map_err(|_| Failure::Malformed),
        Err(NoBearer::Missing) => Err(Failure::Missing),
        Err(NoBearer::Malformed) => Err(Failure::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_bearer;
    use crate::Failure;

    #[test]
    fn a_presented_token_is_redacted_in_debug() {
        use secrecy::ExposeSecret as _;

        let token = parse_bearer([b"Bearer fixture-token".as_slice()]).unwrap();
        assert_eq!(format!("{token:?}"), "PresentedToken([REDACTED])");
        assert_eq!(token.as_bytes(), b"fixture-token");
        assert_eq!(token.as_str(), "fixture-token");
        let credential = token.into_delegation_credential();
        assert_eq!(
            format!("{credential:?}"),
            "DelegationCredential([REDACTED])"
        );
        assert_eq!(credential.access_token().expose_secret(), "fixture-token");
    }

    #[test]
    fn a_missing_envelope_is_not_a_malformed_one() {
        assert_eq!(parse_bearer(std::iter::empty()), Err(Failure::Missing));
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
