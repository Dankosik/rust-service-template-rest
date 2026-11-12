//! URL text as written, before a parser normalizes it.

/// Whether the authority of `url` (the text between `://` and the first `/`,
/// `?` or `#`) contains an `@`.
///
/// `url::Url` drops an empty userinfo and reads `\` as a path separator, so a
/// parsed URL can report no userinfo for an authority that holds an `@`.
#[must_use]
pub fn authority_has_userinfo(url: &str) -> bool {
    let Some((_, authority)) = url.split_once("://") else {
        return false;
    };
    let authority_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
    authority[..authority_end].contains('@')
}

/// Whether `url` holds whitespace or a control character anywhere.
///
/// A WHATWG URL parser such as `url::Url` silently removes tabs and line
/// breaks anywhere and controls and spaces at either end, so it would admit a
/// URL other than the one written. Other Unicode whitespace and controls are
/// refused with them: the parser would percent-encode them into a path no
/// operator meant to configure.
#[must_use]
pub fn has_whitespace_or_control(url: &str) -> bool {
    url.chars()
        .any(|character| character.is_whitespace() || character.is_control())
}

#[cfg(test)]
mod tests {
    use super::{authority_has_userinfo, has_whitespace_or_control};

    #[test]
    fn whitespace_or_controls_anywhere_are_found() {
        for url in [
            "https://identity.example/to\tken",
            "https://identity.example/token\n",
            " https://identity.example/token",
            "https://identity.example/\u{a0}token",
            "https://identity.example/\u{85}token",
            "https://identity.example/\u{2028}token",
            "https://identity.example/\u{7f}token",
        ] {
            assert!(has_whitespace_or_control(url), "{url:?}");
        }
        assert!(!has_whitespace_or_control(
            "https://identity.example/token?scope=a+b#%20"
        ));
    }

    #[test]
    fn an_at_sign_in_the_authority_is_userinfo_even_when_a_parser_hides_it() {
        for url in [
            "https://user:secret@identity.example/token",
            "https://@identity.example/token",
            r"https://identity.example\@other.example/token",
        ] {
            assert!(authority_has_userinfo(url), "{url}");
        }
    }

    #[test]
    fn an_at_sign_after_the_authority_is_not_userinfo() {
        for url in [
            "https://identity.example/token@v1",
            "https://identity.example?subject=a@b",
            "https://identity.example#a@b",
            "identity.example@token",
        ] {
            assert!(!authority_has_userinfo(url), "{url}");
        }
    }
}
