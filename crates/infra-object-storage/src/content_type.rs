use shared_rules::rfc9110::is_tchar;

const MAX_CONTENT_TYPE_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContentType(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("content type is not a supported media type or is too long")]
pub struct InvalidContentType;

impl ContentType {
    pub fn try_new(value: impl AsRef<str> + Into<String>) -> Result<Self, InvalidContentType> {
        let bytes = value.as_ref().as_bytes();
        if bytes.len() <= MAX_CONTENT_TYPE_BYTES && media_type(bytes).is_some() {
            Ok(Self(value.into()))
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

impl AsRef<str> for ContentType {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for ContentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::borrow::Borrow<str> for ContentType {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for ContentType {
    type Err = InvalidContentType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl TryFrom<String> for ContentType {
    type Error = InvalidContentType;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for ContentType {
    type Error = InvalidContentType;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

// RFC 9110 §§5.5, 5.6.5–5.6.6 and 8.3.1
fn media_type(value: &[u8]) -> Option<()> {
    if matches!(value.last(), Some(b' ' | b'\t')) {
        return None;
    }
    let subtype = after_token(value)?.strip_prefix(b"/")?;
    let mut rest = after_token(subtype)?;
    while !rest.is_empty() {
        rest = trim_start_ows(trim_start_ows(rest).strip_prefix(b";")?);
        if rest.is_empty() || rest.starts_with(b";") {
            continue;
        }
        let parameter_value = after_token(rest)?.strip_prefix(b"=")?;
        rest = match parameter_value.strip_prefix(b"\"") {
            Some(quoted) => after_quoted_string(quoted)?,
            None => after_token(parameter_value)?,
        };
    }
    Some(())
}

fn after_token(value: &[u8]) -> Option<&[u8]> {
    let end = value
        .iter()
        .position(|&byte| !is_tchar(byte))
        .unwrap_or(value.len());
    (end != 0).then(|| &value[end..])
}

fn trim_start_ows(mut value: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = value {
        value = rest;
    }
    value
}

fn after_quoted_string(value: &[u8]) -> Option<&[u8]> {
    let mut bytes = value.iter();
    loop {
        let text = match *bytes.next()? {
            b'"' => return Some(bytes.as_slice()),
            b'\\' => *bytes.next()?,
            byte => byte,
        };
        if !matches!(text, b'\t' | b' '..=b'~') {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ContentType;

    #[test]
    fn content_type_validates_media_type_syntax_and_preserves_the_field_value() {
        let maximum = format!("text/plain;x={}", "a".repeat(1011));
        for value in [
            "application/vnd.gonkagate.pricing+json;version=1",
            "text/plain; charset=utf-8",
            "text/plain;\tcharset=utf-8",
            "Text/Plain\t; title=\"a;\\\"b\\\\c\t\" ; empty=\"\";\t;",
            &maximum,
        ] {
            let parsed: ContentType = value.parse().unwrap();
            assert_eq!(parsed.as_str(), value);
            assert_eq!(parsed.as_ref(), value);
            assert_eq!(parsed.to_string(), value);
            assert_eq!(ContentType::try_from(value).unwrap(), parsed);
            assert_eq!(ContentType::try_from(value.to_owned()).unwrap(), parsed);
        }
        for value in [
            "",
            "   ",
            "pdf",
            "/plain",
            "text/",
            " text/plain",
            "text /plain",
            "text/plain, text/html",
            "text/plain; charset",
            "text/plain; charset=",
            "text/plain; charset =utf-8",
            "text/plain; charset= utf-8",
            "text/plain; charset=utf-8\t",
            "text/plain;\t",
            "text/plain; title=\"unterminated",
            "text/plain; title=\"escaped\\\"",
            "text/plain; title=\"escaped\\\u{000b}\"",
            "text/plain\r\nx: y",
            "tëxt/plain",
            "text/plain; title=\"é\"",
            &format!("{maximum}a"),
        ] {
            assert!(ContentType::try_new(value).is_err(), "{value:?}");
        }
    }
}
