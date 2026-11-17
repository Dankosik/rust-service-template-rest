use shared_rules::rfc6750::{self, NoBearer};

use crate::Failure;

#[derive(Eq, PartialEq, derive_more::Debug)]
#[debug("PresentedToken([REDACTED])")]
pub struct PresentedToken<'a> {
    text: &'a str,
}

impl<'a> PresentedToken<'a> {
    pub(crate) fn as_bytes(&self) -> &'a [u8] {
        self.text.as_bytes()
    }

    pub(crate) fn to_secret(&self) -> secrecy::SecretString {
        self.text.into()
    }

    #[must_use]
    pub fn into_delegation_credential(self) -> DelegationCredential {
        DelegationCredential(self.to_secret().into())
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
    headers: impl IntoIterator<Item = &'a [u8]>,
) -> Result<PresentedToken<'a>, Failure> {
    match rfc6750::token(headers) {
        Ok(text) => Ok(PresentedToken { text }),
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
        assert_eq!(format!("{token:?}"), "PresentedToken([REDACTED])");
    }

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
