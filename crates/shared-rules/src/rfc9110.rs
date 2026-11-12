//! HTTP semantics, RFC 9110.

/// Whether the `Content-Type` value `content_type` names the media type
/// `expected` (`type/subtype`): parameters after `;` and the optional
/// whitespace around the type are ignored, and the comparison ignores ASCII
/// case (RFC 9110 §8.3.1).
#[must_use]
pub fn media_type_is(content_type: &str, expected: &str) -> bool {
    let media_type = content_type
        .split_once(';')
        .map_or(content_type, |(media_type, _)| media_type);
    media_type
        .trim_matches([' ', '\t'])
        .eq_ignore_ascii_case(expected)
}

#[cfg(test)]
mod tests {
    use super::media_type_is;

    #[test]
    fn parameters_whitespace_and_case_do_not_change_the_media_type() {
        for value in [
            "application/json",
            "Application/JSON",
            "application/json; charset=utf-8",
            " application/json\t;charset=utf-8; q=1",
        ] {
            assert!(media_type_is(value, "application/json"), "{value:?}");
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
            assert!(!media_type_is(value, "application/json"), "{value:?}");
        }
    }
}
