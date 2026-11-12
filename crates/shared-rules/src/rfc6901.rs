//! JSON Pointer, RFC 6901.

use std::borrow::Cow;

/// `token` written as one reference token (RFC 6901 §3): `~` becomes `~0`,
/// then `/` becomes `~1`. Escaping `/` first would turn the `~` it writes
/// into `~01`.
#[must_use]
pub fn escape(token: &str) -> Cow<'_, str> {
    if token.contains(['~', '/']) {
        Cow::Owned(token.replace('~', "~0").replace('/', "~1"))
    } else {
        Cow::Borrowed(token)
    }
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
    }

    #[test]
    fn a_token_without_tilde_or_slash_is_written_as_is() {
        assert_eq!(escape(""), "");
        assert_eq!(escape("orders.created"), "orders.created");
    }
}
