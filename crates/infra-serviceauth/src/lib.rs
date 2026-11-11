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

use std::{fmt, sync::Arc};

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
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CallerName(Arc<str>);

/// A name outside the caller-name grammar. It never echoes the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub struct InvalidCallerName;

impl InvalidCallerName {
    /// The grammar's one wording, shared with the trust registry's refusal.
    pub(crate) const MESSAGE: &str = "a caller name is 1 to 63 lowercase letters, digits and hyphens, not starting or ending with a hyphen";
}

impl CallerName {
    /// Admits a caller name.
    ///
    /// # Errors
    /// Returns [`InvalidCallerName`] outside the grammar.
    pub fn parse(value: &str) -> Result<Self, InvalidCallerName> {
        let bytes = value.as_bytes();
        let label = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-';
        if (1..=63).contains(&bytes.len())
            && bytes.iter().all(label)
            && bytes.first() != Some(&b'-')
            && bytes.last() != Some(&b'-')
        {
            Ok(Self(Arc::from(value)))
        } else {
            Err(InvalidCallerName)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CallerName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, formatter)
    }
}

impl fmt::Display for CallerName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The `typ` header that names one trust domain's service tokens, such as
/// `example-svc+jwt` (RFC 8725 section 3.11). Compared as a media type:
/// ASCII case-insensitively, with or without the `application/` prefix
/// (RFC 7515 section 4.1.9). Tokens carry the short lowercase form.
#[derive(Clone, Eq, PartialEq)]
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
        let short = Self::short(value).ok_or(InvalidJwtType)?;
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
        Self::short(typ).is_some_and(|short| short.eq_ignore_ascii_case(&self.0))
    }

    /// The value without an `application/` prefix; `None` for another type.
    fn short(value: &str) -> Option<&str> {
        match value.split_once('/') {
            None => Some(value),
            Some((top, rest)) if top.eq_ignore_ascii_case("application") && !rest.contains('/') => {
                Some(rest)
            }
            Some(_) => None,
        }
    }
}

impl fmt::Debug for JwtType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, formatter)
    }
}

/// A `kid` this crate admits: 1 to 256 visible ASCII characters, so the
/// registry and a signer cannot disagree on an invisible difference.
#[derive(Clone, Eq, PartialEq)]
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

impl fmt::Debug for KeyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_names_are_rfc1123_labels() {
        for name in ["a", "orders", "inventory", "a1-b2", &"x".repeat(63)] {
            assert_eq!(CallerName::parse(name).unwrap().as_str(), name);
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
            assert_eq!(CallerName::parse(name), Err(InvalidCallerName), "{name:?}");
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
}
