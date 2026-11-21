mod source;
mod trust;
mod verify;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use std::{borrow::Borrow, str::FromStr, sync::Arc};

use shared_rules::rfc7515;

pub use source::{
    InvalidSigningKey, NoSigningKey, ReceiverTokens, SigningOptions, TokenIssueError, TokenSource,
};
pub use trust::{TrustError, TrustRegistry};
pub use verify::{Caller, Failure, Verifier};

const MAX_TOKEN_LIFETIME_SECONDS: u64 = 60;

fn unix_seconds() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// RFC 1123 §2.1
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Debug, derive_more::Display)]
#[debug("{_0:?}")]
#[display("{_0}")]
pub struct ServiceName(Arc<str>);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub struct InvalidServiceName;

impl InvalidServiceName {
    pub(crate) const MESSAGE: &str = "a service name is 1 to 63 lowercase letters, digits and hyphens, not starting or ending with a hyphen";
}

impl ServiceName {
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

impl AsRef<str> for ServiceName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for ServiceName {
    type Err = InvalidServiceName;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for ServiceName {
    type Error = InvalidServiceName;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ServiceName> for Arc<str> {
    fn from(name: ServiceName) -> Self {
        name.0
    }
}

/// RFC 8725 §3.11, RFC 7515 §4.1.9
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Debug, derive_more::Display)]
#[debug("{_0:?}")]
#[display("{_0}")]
pub struct JwtType(Arc<str>);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "the service token type is an RFC 6838 media subtype such as example-svc+jwt, optionally prefixed by application/"
)]
pub struct InvalidJwtType;

impl JwtType {
    pub fn parse(value: &str) -> Result<Self, InvalidJwtType> {
        let subtype = rfc7515::typ_subtype(value).ok_or(InvalidJwtType)?;
        let bytes = subtype.as_bytes();
        let is_subtype_byte =
            |byte: &u8| byte.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(byte);
        if (1..=127).contains(&bytes.len())
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(is_subtype_byte)
        {
            Ok(Self(Arc::from(subtype.to_ascii_lowercase())))
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

impl Borrow<str> for JwtType {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for JwtType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for JwtType {
    type Err = InvalidJwtType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for JwtType {
    type Error = InvalidJwtType;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, derive_more::Debug, derive_more::Display)]
#[debug("{_0:?}")]
#[display("{_0}")]
pub struct KeyId(Arc<str>);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub struct InvalidKeyId;

impl InvalidKeyId {
    pub(crate) const MESSAGE: &str = "the key id is not 1 to 256 visible ASCII characters";
}

impl KeyId {
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

impl Borrow<str> for KeyId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for KeyId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for KeyId {
    type Err = InvalidKeyId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for KeyId {
    type Error = InvalidKeyId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_names_are_rfc1123_labels() {
        for name in ["a", "orders", "inventory", "a1-b2", &"x".repeat(63)] {
            let parsed: ServiceName = name.parse().unwrap();
            assert_eq!(parsed.as_str(), name);
            assert_eq!(parsed.as_ref(), name);
            assert_eq!(ServiceName::parse(name).unwrap(), parsed);
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
            let parsed: KeyId = kid.parse().unwrap();
            assert_eq!(parsed.as_str(), kid);
            assert_eq!(parsed.as_ref(), kid);
            assert_eq!(parsed.to_string(), kid);
            assert_eq!(KeyId::parse(kid).unwrap(), parsed);
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
        let typ: JwtType = "Example-SVC+JWT".parse().unwrap();
        assert_eq!(typ.as_str(), "example-svc+jwt");
        assert_eq!(typ.as_ref(), "example-svc+jwt");
        assert_eq!(typ.to_string(), "example-svc+jwt");
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

    mod grammar {
        use super::*;
        use proptest::prelude::*;
        use regex::regex;

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
                let label = regex!("^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$");
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
                let visible = regex!("^[!-~]{1,256}$");
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
                let media_subtype = regex!(
                    "^((?i-u:application)/)?[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]{0,126}$"
                );
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
