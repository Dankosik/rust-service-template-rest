//! Caller-signed service tokens.
//!
//! A calling service signs a short ES256 JWT for one receiver with its own
//! key ([`TokenSource`]). The receiver verifies it only against the keys its
//! [`TrustRegistry`] holds for that caller ([`Verifier`]) and learns who
//! called ([`Caller`]). Which caller may call which method is the
//! transport's table; this crate performs no I/O and knows no transport.

mod source;
mod trust;
mod verify;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::{borrow::Borrow, sync::Arc};

use shared_rules::rfc7515;

pub use source::{ReceiverTokens, SigningError, SigningOptions, TokenSource};
pub use trust::{TrustError, TrustRegistry};
pub use verify::{Caller, Failure, VERIFICATIONS_METRIC, Verifier};

/// The longest lifetime a service token may claim, `exp - iat`, in seconds.
const MAX_TOKEN_LIFETIME_SECONDS: u64 = 60;

/// Whole seconds since the Unix epoch; `None` for a clock before it.
fn unix_seconds() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// A service's name in the trust domain: its tokens' `iss` and `sub`, and the
/// audience it accepts. One to 63 lowercase ASCII letters, digits and
/// hyphens, neither first nor last a hyphen (an RFC 1123 label).
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Debug, derive_more::Display)]
#[debug("{_0:?}")]
#[display("{_0}")]
pub struct ServiceName(Arc<str>);

/// A name outside the service-name grammar. It never echoes the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub struct InvalidServiceName;

impl InvalidServiceName {
    /// The grammar's one wording, shared with the trust registry's refusal.
    pub(crate) const MESSAGE: &str = "a caller name is 1 to 63 lowercase letters, digits and hyphens, not starting or ending with a hyphen";
}

impl ServiceName {
    /// Admits a service name.
    ///
    /// # Errors
    /// Returns [`InvalidServiceName`] outside the grammar.
    pub fn parse(value: &str) -> Result<Self, InvalidServiceName> {
        let bytes = value.as_bytes();
        let label = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-';
        if (1..=63).contains(&bytes.len())
            && bytes.iter().all(label)
            && bytes.first() != Some(&b'-')
            && bytes.last() != Some(&b'-')
        {
            Ok(Self(Arc::from(value)))
        } else {
            Err(InvalidServiceName)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ServiceName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<ServiceName> for Arc<str> {
    fn from(name: ServiceName) -> Self {
        name.0
    }
}

/// The `typ` header that names one trust domain's service tokens, such as
/// `example-svc+jwt` (RFC 8725 section 3.11). Compared as a media type:
/// ASCII case-insensitively, with or without the `application/` prefix
/// (RFC 7515 section 4.1.9). Tokens carry the short lowercase form.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("{_0:?}")]
pub struct JwtType(Arc<str>);

/// A token type outside the media-type subtype grammar.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "the service token type is an RFC 6838 media subtype such as example-svc+jwt, optionally prefixed by application/"
)]
pub struct InvalidJwtType;

impl JwtType {
    /// Admits a token type and keeps its short lowercase form.
    ///
    /// # Errors
    /// Returns [`InvalidJwtType`] outside RFC 6838 `restricted-name` syntax.
    pub fn parse(value: &str) -> Result<Self, InvalidJwtType> {
        let short = rfc7515::typ_subtype(value).ok_or(InvalidJwtType)?;
        let name = short.as_bytes();
        let restricted = |byte: &u8| byte.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(byte);
        if (1..=127).contains(&name.len())
            && name[0].is_ascii_alphanumeric()
            && name.iter().all(restricted)
        {
            Ok(Self(Arc::from(short.to_ascii_lowercase())))
        } else {
            Err(InvalidJwtType)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn admits(&self, typ: &str) -> bool {
        rfc7515::typ_is(typ, &self.0)
    }
}

/// A `kid` this crate admits: 1 to 256 visible ASCII characters, so the
/// registry and a signer cannot disagree on an invisible difference.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("{_0:?}")]
pub struct KeyId(Arc<str>);

/// A key id outside the key-id grammar. It never echoes the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub struct InvalidKeyId;

impl InvalidKeyId {
    /// The grammar's one wording, shared with the trust registry's refusal.
    pub(crate) const MESSAGE: &str = "the key id is not 1 to 256 visible ASCII characters";
}

impl KeyId {
    /// Admits a key id.
    ///
    /// # Errors
    /// Returns [`InvalidKeyId`] outside the grammar.
    pub fn parse(value: &str) -> Result<Self, InvalidKeyId> {
        if (1..=256).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_graphic()) {
            Ok(Self(Arc::from(value)))
        } else {
            Err(InvalidKeyId)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_names_are_rfc1123_labels() {
        for name in ["a", "orders", "inventory", "a1-b2", &"x".repeat(63)] {
            assert_eq!(ServiceName::parse(name).unwrap().as_str(), name);
        }
        for name in [
            "",
            "-a",
            "a-",
            "Upper",
            "under_score",
            "dot.ted",
            "space d",
            &"x".repeat(64),
            "é",
        ] {
            assert_eq!(
                ServiceName::parse(name),
                Err(InvalidServiceName),
                "{name:?}"
            );
        }
    }

    #[test]
    fn key_ids_are_visible_ascii() {
        for kid in ["k", "orders/production/2026-10", &"x".repeat(256)] {
            assert_eq!(KeyId::parse(kid).unwrap().as_str(), kid);
        }
        for kid in [
            "",
            "two words",
            "tab\there",
            "é",
            "\u{7f}",
            &"x".repeat(257),
        ] {
            assert_eq!(KeyId::parse(kid), Err(InvalidKeyId), "{kid:?}");
        }
    }

    #[test]
    fn token_types_compare_as_media_types() {
        let typ = JwtType::parse("Example-SVC+JWT").unwrap();
        assert_eq!(typ.as_str(), "example-svc+jwt");
        assert_eq!(JwtType::parse("application/example-svc+jwt").unwrap(), typ);
        for presented in [
            "example-svc+jwt",
            "EXAMPLE-SVC+JWT",
            "application/example-svc+jwt",
            "Application/Example-Svc+Jwt",
        ] {
            assert!(typ.admits(presented), "{presented}");
        }
        for presented in [
            "JWT",
            "at+jwt",
            "example-svc",
            "text/example-svc+jwt",
            "application/x/example-svc+jwt",
            "",
        ] {
            assert!(!typ.admits(presented), "{presented}");
        }
        for value in [
            "",
            "application/",
            "+jwt",
            "a b",
            "a/b",
            "text/jwt",
            "a\u{7f}",
            &"x".repeat(128),
        ] {
            assert_eq!(JwtType::parse(value), Err(InvalidJwtType), "{value:?}");
        }
    }

    /// A value one grammar admits or refuses, judged by a regex written from
    /// the grammar's doc comment instead of by the parser's own checks.
    mod grammar {
        use super::*;
        use proptest::prelude::*;
        use regex::Regex;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            #[test]
            fn service_names_match_the_rfc1123_label_regex(
                value in prop_oneof![
                    any::<String>(),
                    "[a-z0-9-]{0,66}",
                    "[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?",
                ],
            ) {
                let label = Regex::new("^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$").unwrap();
                let parsed = ServiceName::parse(&value);
                prop_assert_eq!(parsed.is_ok(), label.is_match(&value));
                if let Ok(name) = parsed {
                    prop_assert_eq!(name.as_str(), value);
                }
            }

            #[test]
            fn key_ids_match_the_visible_ascii_regex(
                value in prop_oneof![
                    any::<String>(),
                    "[!-~]{0,260}",
                    "[!-~ \t\u{7f}\u{e9}]{0,8}",
                ],
            ) {
                let visible = Regex::new("^[!-~]{1,256}$").unwrap();
                let parsed = KeyId::parse(&value);
                prop_assert_eq!(parsed.is_ok(), visible.is_match(&value));
                if let Ok(kid) = parsed {
                    prop_assert_eq!(kid.as_str(), value);
                }
            }

            #[test]
            fn token_types_match_the_restricted_name_regex(
                value in prop_oneof![
                    any::<String>(),
                    "[A-Za-z0-9!#$&^_.+/-]{0,140}",
                    "[aA][pP]{2}[a-zA-Z]{0,8}/[A-Za-z0-9!#$&^_.+-]{0,130}",
                ],
            ) {
                let media_subtype = Regex::new(
                    "^((?i-u:application)/)?[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]{0,126}$",
                )
                .unwrap();
                let parsed = JwtType::parse(&value);
                prop_assert_eq!(parsed.is_ok(), media_subtype.is_match(&value));
                if let Ok(typ) = parsed {
                    let short = value.rsplit('/').next().unwrap();
                    prop_assert_eq!(typ.as_str(), short.to_ascii_lowercase());
                }
            }
        }
    }
}
