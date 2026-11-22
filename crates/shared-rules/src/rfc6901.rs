use std::borrow::Cow;

/// RFC 6901 §3
#[must_use]
#[expect(
    clippy::string_slice,
    reason = "matched ASCII delimiters '~' and '/' are single-byte UTF-8 boundaries"
)]
pub fn escape(token: &str) -> Cow<'_, str> {
    let bytes = token.as_bytes();
    let Some(first_special) = bytes.iter().position(|&byte| is_special(byte)) else {
        return Cow::Borrowed(token);
    };
    let tail = &token[first_special + 1..];
    let tail_specials = tail.bytes().filter(|&byte| is_special(byte)).count();
    let mut escaped = String::with_capacity(token.len() + 1 + tail_specials);
    escaped.push_str(&token[..first_special]);
    escaped.push_str(escape_special(bytes[first_special]));
    if tail_specials == 0 {
        escaped.push_str(tail);
        return Cow::Owned(escaped);
    }
    let mut start = 0;
    for (index, byte) in tail.bytes().enumerate() {
        if is_special(byte) {
            escaped.push_str(&tail[start..index]);
            escaped.push_str(escape_special(byte));
            start = index + 1;
        }
    }
    escaped.push_str(&tail[start..]);
    Cow::Owned(escaped)
}

/// RFC 6901 §3
const fn is_special(byte: u8) -> bool {
    matches!(byte, b'~' | b'/')
}

const fn escape_special(special: u8) -> &'static str {
    match special {
        b'~' => "~0",
        _ => "~1",
    }
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn a_token_escapes_tilde_before_slash() {
        for (input, expected) in [
            ("a/b", "a~1b"),
            ("m~n", "m~0n"),
            ("~1", "~01"),
            ("/~", "~1~0"),
            ("a/b/c", "a~1b~1c"),
            ("a~b~c", "a~0b~0c"),
            ("m~n/p", "m~0n~1p"),
            ("a/b~c", "a~1b~0c"),
            (
                "prefix_without_delimiters/tail",
                "prefix_without_delimiters~1tail",
            ),
            ("~~~///", "~0~0~0~1~1~1"),
            ("ключ/名~", "ключ~1名~0"),
            ("ключ/名", "ключ~1名"),
            ("ключ/名~конец", "ключ~1名~0конец"),
        ] {
            let escaped = escape(input);
            assert_eq!(escaped, expected, "{input:?}");
            assert!(matches!(escaped, std::borrow::Cow::Owned(_)), "{input:?}");
        }
    }

    #[test]
    fn a_token_without_tilde_or_slash_is_written_as_is() {
        for plain in ["", "orders.created"] {
            assert!(matches!(escape(plain), std::borrow::Cow::Borrowed(value) if value == plain));
        }
    }
}
