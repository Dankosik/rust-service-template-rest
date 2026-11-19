use std::borrow::Cow;

/// RFC 6901 §3
#[must_use]
#[expect(
    clippy::string_slice,
    reason = "match_indices supplies UTF-8 boundaries and both matched delimiters are one byte"
)]
pub fn escape(token: &str) -> Cow<'_, str> {
    let extra = token
        .bytes()
        .filter(|byte| matches!(byte, b'~' | b'/'))
        .count();
    if extra == 0 {
        return Cow::Borrowed(token);
    }
    let mut escaped = String::with_capacity(token.len() + extra);
    let mut start = 0;
    for (index, symbol) in token.match_indices(['~', '/']) {
        escaped.push_str(&token[start..index]);
        escaped.push_str(if symbol == "~" { "~0" } else { "~1" });
        start = index + 1;
    }
    escaped.push_str(&token[start..]);
    Cow::Owned(escaped)
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn a_token_escapes_tilde_before_slash() {
        assert_eq!(escape("a/b"), "a~1b");
        assert_eq!(escape("m~n"), "m~0n");
        assert_eq!(escape("~1"), "~01");
        assert_eq!(escape("/~"), "~1~0");
        assert_eq!(escape("ключ/名~"), "ключ~1名~0");
    }

    #[test]
    fn a_token_without_tilde_or_slash_is_written_as_is() {
        for plain in ["", "orders.created"] {
            assert!(matches!(escape(plain), std::borrow::Cow::Borrowed(value) if value == plain));
        }
    }
}
