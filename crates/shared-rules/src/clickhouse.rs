use std::num::NonZeroUsize;
use std::time::Duration;

pub const MIN_TIMEOUT: Duration = Duration::from_millis(10);
pub const MAX_TIMEOUT: Duration = Duration::from_mins(5);
pub const MAX_CONCURRENCY_CEILING: usize = 1024;
pub const MAX_RESULT_ROWS_CEILING: u64 = 10_000_000;
pub const MIN_RESULT_BYTES: usize = 64 * 1024;
pub const MAX_RESULT_BYTES: usize = 128 * 1024 * 1024;
pub const DEFAULT_HTTP_RESPONSE_BUFFER_BYTES: NonZeroUsize =
    NonZeroUsize::MIN.saturating_add(2 * 1024 * 1024 - 1);
pub const MIN_MEMORY_USAGE: u64 = 16 * 1024 * 1024;
pub const MAX_MEMORY_USAGE: u64 = 64 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UrlLexicalError {
    ForbiddenCharacters,
    MissingHost,
    UserinfoNotAllowed,
    EmptyPathSegment,
    DotPathSegment,
}

pub fn validate_url_lexical(raw: &str) -> Result<(), UrlLexicalError> {
    if crate::uri::has_whitespace_or_control(raw) || raw.contains(['?', '#', '\\']) {
        return Err(UrlLexicalError::ForbiddenCharacters);
    }
    if crate::uri::authority_has_userinfo(raw) {
        return Err(UrlLexicalError::UserinfoNotAllowed);
    }
    let Some((_, after_scheme)) = raw.split_once("://") else {
        return Err(UrlLexicalError::MissingHost);
    };
    let authority = after_scheme.split('/').next().unwrap_or("");
    if after_scheme.starts_with('/') || authority.is_empty() {
        return Err(UrlLexicalError::MissingHost);
    }
    if let Some((_, raw_path)) = after_scheme.split_once('/')
        && !raw_path.is_empty()
    {
        let trimmed = raw_path.trim_suffix('/');
        if trimmed.contains("//") {
            return Err(UrlLexicalError::EmptyPathSegment);
        }
        for segment in trimmed.split('/') {
            if segment.is_empty() {
                return Err(UrlLexicalError::EmptyPathSegment);
            }
            if is_dot_segment(segment) {
                return Err(UrlLexicalError::DotPathSegment);
            }
        }
    }
    Ok(())
}

#[must_use]
pub fn trim_path_prefix(path: &str) -> &str {
    path.trim_end_matches('/')
}

#[must_use]
pub fn strip_root_trailing_slash(mut normalized: String, clean_path: &str) -> String {
    if clean_path.is_empty() && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn is_dot_segment(segment: &str) -> bool {
    segment == "."
        || segment == ".."
        || segment.eq_ignore_ascii_case("%2e")
        || segment.eq_ignore_ascii_case("%2e%2e")
        || segment.eq_ignore_ascii_case(".%2e")
        || segment.eq_ignore_ascii_case("%2e.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_constants_match_contract_bounds() {
        assert_eq!(DEFAULT_HTTP_RESPONSE_BUFFER_BYTES.get(), 2 * 1024 * 1024);
        assert!(MIN_TIMEOUT < MAX_TIMEOUT);
        assert!(MIN_RESULT_BYTES <= DEFAULT_HTTP_RESPONSE_BUFFER_BYTES.get());
        assert!(DEFAULT_HTTP_RESPONSE_BUFFER_BYTES.get() <= MAX_RESULT_BYTES);
        const {
            assert!(MIN_MEMORY_USAGE < MAX_MEMORY_USAGE);
            assert!(MAX_CONCURRENCY_CEILING >= 1);
            assert!(MAX_RESULT_ROWS_CEILING >= 1);
        }
    }

    #[test]
    fn lexical_validation_accepts_clean_urls_and_rejects_smuggling_variants() {
        for valid in [
            "https://127.0.0.1:8443",
            "https://127.0.0.1:8443/",
            "https://ch.example:8443/ch",
            "https://ch.example:8443/a/b/",
            "https://[::1]:8443/prefix/",
            "http://127.0.0.1:8123",
            "https://ch.example:8443/user@v1",
        ] {
            assert_eq!(validate_url_lexical(valid), Ok(()), "{valid}");
        }

        for (invalid, expected) in [
            (
                "https://ch.example:8443?database=app",
                UrlLexicalError::ForbiddenCharacters,
            ),
            (
                "https://ch.example:8443#frag",
                UrlLexicalError::ForbiddenCharacters,
            ),
            (
                "https://ch.example:8443/a\\..\\b",
                UrlLexicalError::ForbiddenCharacters,
            ),
            (
                "https://ch.example:8443\n",
                UrlLexicalError::ForbiddenCharacters,
            ),
            (
                "https://user:pass@127.0.0.1:8443",
                UrlLexicalError::UserinfoNotAllowed,
            ),
            (
                "HTTPS:///@127.0.0.1:8443",
                UrlLexicalError::UserinfoNotAllowed,
            ),
            ("https://", UrlLexicalError::MissingHost),
            ("https:///127.0.0.1:8443", UrlLexicalError::MissingHost),
            ("https:/127.0.0.1:8443", UrlLexicalError::MissingHost),
            ("not-a-url", UrlLexicalError::MissingHost),
            (
                "https://ch.example:8443//",
                UrlLexicalError::EmptyPathSegment,
            ),
            (
                "https://ch.example:8443/a//b",
                UrlLexicalError::EmptyPathSegment,
            ),
            (
                "https://ch.example:8443/a/b//",
                UrlLexicalError::EmptyPathSegment,
            ),
            ("https://ch.example:8443/.", UrlLexicalError::DotPathSegment),
            (
                "https://ch.example:8443/..",
                UrlLexicalError::DotPathSegment,
            ),
            (
                "https://ch.example:8443/a/%2e/b",
                UrlLexicalError::DotPathSegment,
            ),
            (
                "https://ch.example:8443/a/%2E%2E/b",
                UrlLexicalError::DotPathSegment,
            ),
            (
                "https://ch.example:8443/a/.%2e/b",
                UrlLexicalError::DotPathSegment,
            ),
            (
                "https://ch.example:8443/a/%2e./b",
                UrlLexicalError::DotPathSegment,
            ),
        ] {
            assert_eq!(validate_url_lexical(invalid), Err(expected), "{invalid}");
        }
    }

    #[test]
    fn root_trailing_slash_cleanup_strips_only_empty_path() {
        let root = "https://127.0.0.1:8443/".to_owned();
        assert_eq!(
            strip_root_trailing_slash(root, trim_path_prefix("/")),
            "https://127.0.0.1:8443"
        );
        let prefix = "https://127.0.0.1:8443/ch".to_owned();
        assert_eq!(
            strip_root_trailing_slash(prefix, trim_path_prefix("/ch/")),
            "https://127.0.0.1:8443/ch"
        );
    }
}
