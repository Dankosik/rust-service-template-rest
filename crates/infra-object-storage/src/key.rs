//! Validated object keys and content types.

use std::fmt;

/// Longest key every supported provider stores.
const MAX_KEY_BYTES: usize = 1024;
/// Longest content type the client sends; far above any registered media type.
const MAX_CONTENT_TYPE_BYTES: usize = 1024;

/// An object key in the portable grammar: 1 to 1024 bytes of
/// `[A-Za-z0-9._~-]` in `/`-separated segments, with no empty, `.`, or `..`
/// segment and no leading or trailing `/`.
///
/// ASCII only because R2 normalizes Unicode keys, so two distinct UTF-8 keys
/// could name one object. Keys still reach provider logs and, at DEBUG, the
/// SDK's own logs: never put personal data in a key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ObjectKey(String);

/// The key does not match the portable grammar. The key itself is not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("object key does not match the portable key grammar")]
pub struct InvalidObjectKey;

impl ObjectKey {
    /// Admit a key.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidObjectKey`] when the key does not match the grammar.
    pub fn try_new(key: impl Into<String>) -> Result<Self, InvalidObjectKey> {
        let key = key.into();
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return Err(InvalidObjectKey);
        }
        let valid = key.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._~-".contains(&byte))
        });
        if valid {
            Ok(Self(key))
        } else {
            Err(InvalidObjectKey)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Only the length: a key can carry business identifiers.
impl fmt::Debug for ObjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectKey")
            .field("len", &self.0.len())
            .finish()
    }
}

/// A `Content-Type` value stored with the object: visible ASCII, spaces and tabs,
/// at most 1024 bytes, such as `application/json` or
/// `application/vnd.example+json;version=1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentType(String);

/// The content type is empty, too long, or not a valid header value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("content type is not a valid header value")]
pub struct InvalidContentType;

impl ContentType {
    /// Admit a content type before any request carries it.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidContentType`] for an empty value, one over 1024 bytes,
    /// or one with a control or non-ASCII byte.
    pub fn try_new(value: impl Into<String>) -> Result<Self, InvalidContentType> {
        let value = value.into();
        let valid = !value.trim().is_empty()
            && value.len() <= MAX_CONTENT_TYPE_BYTES
            && value
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t') || byte.is_ascii_graphic());
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidContentType)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentType, ObjectKey};

    #[test]
    fn keys_follow_the_portable_grammar() {
        for key in [
            "a",
            "pricing-evidence/v1/2026/09/29/abc_def.json",
            "parse-results/op-1.json",
            "a.b~c-d_e",
            &"k".repeat(1024),
        ] {
            ObjectKey::try_new(key).unwrap_or_else(|_| panic!("{key:?} should be admitted"));
        }
        for key in [
            "",
            "/leading",
            "trailing/",
            "double//slash",
            "./dot",
            "a/../b",
            "space here",
            "unicode-é",
            "query?x",
            "percent%20",
            &"k".repeat(1025),
        ] {
            assert!(
                ObjectKey::try_new(key).is_err(),
                "{key:?} should be refused"
            );
        }
    }

    #[test]
    fn key_debug_hides_the_key() {
        let key = ObjectKey::try_new("customer-42/invoice.pdf").unwrap();
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("customer"), "{rendered}");
    }

    #[test]
    fn content_types_are_header_values() {
        ContentType::try_new("application/vnd.gonkagate.pricing+json;version=1").unwrap();
        ContentType::try_new("text/plain; charset=utf-8").unwrap();
        ContentType::try_new("text/plain;\tcharset=utf-8").unwrap();
        for value in [
            "",
            "   ",
            "text/plain\r\nx: y",
            "tëxt/plain",
            &"a".repeat(1025),
        ] {
            assert!(ContentType::try_new(value).is_err(), "{value:?}");
        }
    }
}
