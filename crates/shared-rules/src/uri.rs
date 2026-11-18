#[must_use]
pub fn http_origin_port(scheme: &str, authority: &str, host: &str) -> Option<u16> {
    let default = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    if host.is_empty() || authority.contains('@') {
        return None;
    }
    match authority.strip_prefix(host)? {
        "" => Some(default),
        suffix => {
            let digits = suffix.strip_prefix(':')?;
            if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            digits.parse().ok()
        }
    }
}

#[must_use]
pub fn authority_has_userinfo(url: &str) -> bool {
    let Some((scheme, after_scheme)) = url.split_once(':') else {
        return false;
    };
    let authority = if ["ftp", "http", "https", "ws", "wss"]
        .iter()
        .any(|special| scheme.eq_ignore_ascii_case(special))
    {
        after_scheme.trim_start_matches(['/', '\\'])
    } else if let Some(authority) = after_scheme.strip_prefix("//") {
        authority
    } else {
        return false;
    };
    let (authority, _) = authority
        .split_once(['/', '?', '#'])
        .unwrap_or((authority, ""));
    authority.contains('@')
}

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
            "HTTPS:///@identity.example/token",
            r"https:\@identity.example/token",
            r"https://identity.example\@other.example/token",
            "nats://@identity.example",
        ] {
            assert!(authority_has_userinfo(url), "{url}");
        }
    }

    #[test]
    fn an_at_sign_after_the_authority_is_not_userinfo() {
        for url in [
            "https://identity.example/token@v1",
            "https:///identity.example/token@v1",
            "nats://identity.example/token@v1",
            "https://identity.example?subject=a@b",
            "https://identity.example#a@b",
            "identity.example@token",
        ] {
            assert!(!authority_has_userinfo(url), "{url}");
        }
    }
}
