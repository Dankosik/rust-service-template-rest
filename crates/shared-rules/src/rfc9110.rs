//! HTTP semantics, RFC 9110.

/// Whether the `Content-Type` value names `application/json`. The comparison
/// ignores ASCII case, parameters, and optional whitespace around the media
/// type (RFC 9110 §8.3.1).
#[must_use]
pub fn is_json_content_type(content_type: &str) -> bool {
    let media_type = content_type
        .split_once(';')
        .map_or(content_type, |(media_type, _)| media_type);
    media_type
        .trim_matches([' ', '\t'])
        .eq_ignore_ascii_case("application/json")
}

#[cfg(test)]
mod tests {
    use super::is_json_content_type;

    #[test]
    fn parameters_whitespace_and_case_do_not_change_the_media_type() {
        for value in [
            "application/json",
            "Application/JSON",
            "application/json; charset=utf-8",
            " application/json\t;charset=utf-8; q=1",
        ] {
            assert!(is_json_content_type(value), "{value:?}");
        }
    }

    #[test]
    fn another_or_missing_media_type_does_not_match() {
        for value in [
            "",
            ";application/json",
            "application/jsonx",
            "application/problem+json",
            "text/json",
            "application/ json",
        ] {
            assert!(!is_json_content_type(value), "{value:?}");
        }
    }
}
