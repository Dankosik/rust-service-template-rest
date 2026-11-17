use shared_rules::rfc9110::is_tchar;

const MAX_CONTENT_TYPE_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentType(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("content type is not a supported media type or is too long")]
pub struct InvalidContentType;

impl ContentType {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InvalidContentType> {
        let value = value.into();
        let valid = value.len() <= MAX_CONTENT_TYPE_BYTES && media_type(value.as_bytes()).is_some();
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

// RFC 9110 §§5.5, 5.6.5–5.6.6 and 8.3.1
fn media_type(value: &[u8]) -> Option<()> {
    if matches!(value.last(), Some(b' ' | b'\t')) {
        return None;
    }
    let subtype = after_token(value)?.strip_prefix(b"/")?;
    let mut rest = after_token(subtype)?;
    while !rest.is_empty() {
        rest = trim_ows(trim_ows(rest).strip_prefix(b";")?);
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

fn trim_ows(mut value: &[u8]) -> &[u8] {
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
            assert_eq!(ContentType::try_new(value).unwrap().as_str(), value);
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
