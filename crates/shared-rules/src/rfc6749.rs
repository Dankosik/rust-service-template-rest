//! OAuth 2.0, RFC 6749.

/// RFC 6749 §3.3 `scope-token = 1*NQCHAR`, where `NQCHAR` is
/// `%x21 / %x23-5B / %x5D-7E`.
#[must_use]
pub fn is_scope_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| matches!(byte, 0x21 | 0x23..=0x5b | 0x5d..=0x7e))
}

#[cfg(test)]
mod tests {
    use super::is_scope_token;

    #[test]
    fn a_scope_token_is_visible_ascii_without_quote_or_backslash() {
        for character in (0_u8..=0x7f).map(char::from) {
            let expected = character.is_ascii_graphic() && !matches!(character, '"' | '\\');
            assert_eq!(
                is_scope_token(&character.to_string()),
                expected,
                "{character:?}"
            );
        }
    }

    #[test]
    fn a_scope_token_is_nonempty_and_ascii() {
        assert!(is_scope_token("orders:read"));
        assert!(!is_scope_token(""));
        assert!(!is_scope_token("read write"));
        assert!(!is_scope_token("lecture-é"));
    }
}
