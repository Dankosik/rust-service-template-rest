//! Validation error and the range helpers every section shares.

use std::time::Duration;

/// One violated configuration rule, named by the key an operator would set.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {message}")]
pub struct ValidationError {
    /// Dotted key, for example `http.request_timeout`.
    pub key: String,
    /// What the value had to satisfy.
    pub message: String,
}

impl ValidationError {
    /// A violated rule for `key`. Composition roots use it for rules that
    /// depend on a transport constant this crate does not know.
    #[must_use]
    pub fn new(key: &str, message: impl Into<String>) -> Self {
        Self {
            key: key.to_owned(),
            message: message.into(),
        }
    }
}

pub(crate) fn duration_range(
    key: &str,
    value: Duration,
    min: Duration,
    max: Duration,
) -> Result<(), ValidationError> {
    if value < min || value > max {
        return Err(ValidationError::new(
            key,
            format!(
                "must be between {} and {}, got {}",
                humantime::format_duration(min),
                humantime::format_duration(max),
                humantime::format_duration(value)
            ),
        ));
    }
    Ok(())
}

pub(crate) fn int_range(key: &str, value: u64, min: u64, max: u64) -> Result<(), ValidationError> {
    if value < min || value > max {
        return Err(ValidationError::new(
            key,
            format!("must be between {min} and {max}, got {value}"),
        ));
    }
    Ok(())
}

pub(crate) fn non_empty(key: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(key, "cannot be empty"));
    }
    Ok(())
}

/// Whether an `APP__` variable can address the key `name`. The loader
/// lowercases a variable name, splits it on `__`, and reads each segment as
/// a config-rs path identifier (ASCII letters, digits, `_`, `-`). A key
/// outside that form never meets its environment counterpart: the file and
/// the environment would name two entries.
pub(crate) fn is_env_addressable(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        && !name.contains("__")
        && !name.ends_with('_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::below_minimum(0, false)]
    #[case::minimum(1, true)]
    #[case::maximum(10, true)]
    #[case::above_maximum(11, false)]
    fn int_range_is_inclusive(#[case] value: u64, #[case] valid: bool) {
        assert_eq!(int_range("limit", value, 1, 10).is_ok(), valid);
    }

    #[rstest::rstest]
    #[case::below_minimum(99, false)]
    #[case::minimum(100, true)]
    #[case::maximum(600_000, true)]
    #[case::above_maximum(600_001, false)]
    fn duration_range_is_inclusive(#[case] milliseconds: u64, #[case] valid: bool) {
        let result = duration_range(
            "http.request_timeout",
            Duration::from_millis(milliseconds),
            Duration::from_millis(100),
            Duration::from_secs(600),
        );
        assert_eq!(result.is_ok(), valid);
    }

    #[rstest::rstest]
    #[case::word("partner", true)]
    #[case::underscore("partner_v2", true)]
    #[case::hyphen("partner-v2", true)]
    #[case::leading_underscore("_partner", true)]
    #[case::path_characters("partner/a", false)]
    #[case::dot("partner.a", false)]
    #[case::non_ascii("pärtner", false)]
    #[case::empty("", false)]
    #[case::uppercase("Partner", false)]
    #[case::segment_separator("partner__v2", false)]
    #[case::trailing_underscore("partner_", false)]
    fn env_addressable_names(#[case] name: &str, #[case] addressable: bool) {
        assert_eq!(is_env_addressable(name), addressable);
    }

    #[test]
    fn duration_range_names_bounds() {
        let err = duration_range(
            "http.request_timeout",
            Duration::from_millis(10),
            Duration::from_millis(100),
            Duration::from_secs(600),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "http.request_timeout: must be between 100ms and 10m, got 10ms"
        );
    }
}
