/// Longest content type the client sends; far above any registered media type.
const MAX_CONTENT_TYPE_BYTES: usize = 1024;

/// A `Content-Type` value stored with the object: visible ASCII, spaces and tabs,
/// at most 1024 bytes, such as `application/json` or
/// `application/vnd.example+json;version=1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentType(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("content type is blank, too long, or contains unsupported bytes")]
pub struct InvalidContentType;

impl ContentType {
    /// Admit a content type before any request carries it.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidContentType`] for a blank value, one over 1024 bytes,
    /// or one containing a byte other than visible ASCII, space, or tab.
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
    use super::ContentType;

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
