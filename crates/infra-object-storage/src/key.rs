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
    pub fn new(key: impl Into<String>) -> Result<Self, InvalidObjectKey> {
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

    /// The admitted key.
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

/// A `Content-Type` value stored with the object: visible ASCII and spaces,
/// at most 1024 bytes, such as `application/json` or
/// `application/vnd.example+json;version=1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentType(pub(crate) String);

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
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidContentType> {
        let value = value.into();
        let valid = !value.trim().is_empty()
            && value.len() <= MAX_CONTENT_TYPE_BYTES
            && value
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic());
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidContentType)
        }
    }

    /// The admitted value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
