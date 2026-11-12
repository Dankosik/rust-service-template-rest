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

#[cfg(test)]
mod tests {
    use super::authority_has_userinfo;

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
