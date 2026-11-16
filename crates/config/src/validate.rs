use std::time::Duration;

use crate::load::variable_name;
use crate::secret_policy::is_secret_like_key;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {message}")]
pub struct ValidationError {
    pub key: String,
    pub message: String,
}

impl ValidationError {
    /// A violated rule for `key`. Composition roots use it for rules that
    /// depend on a transport constant this crate does not know.
    #[must_use]
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

/// Every rule a loaded snapshot violated, in section order. Never empty.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", join(.0))]
pub struct ValidationErrors(Vec<ValidationError>);

impl ValidationErrors {
    pub fn iter(&self) -> std::slice::Iter<'_, ValidationError> {
        self.0.iter()
    }

    /// Whether any violation names `key`.
    #[must_use]
    pub fn names(&self, key: &str) -> bool {
        self.0.iter().any(|error| error.key == key)
    }
}

impl<'a> IntoIterator for &'a ValidationErrors {
    type Item = &'a ValidationError;
    type IntoIter = std::slice::Iter<'a, ValidationError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

fn join(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// A section admission refused its input; the reason is recorded in the
/// [`Violations`] that issued this marker, which no other path constructs.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "removing every profile with conditional input leaves no admission that rejects"
)]
pub(crate) struct Rejected(());

/// Collects every violated rule of one admission, so an operator sees all of
/// them in one startup failure rather than one per restart.
#[derive(Debug, Default)]
pub(crate) struct Violations(Vec<ValidationError>);

impl Violations {
    pub(crate) fn check(&mut self, result: Result<(), ValidationError>) {
        if let Err(error) = result {
            self.0.push(error);
        }
    }

    #[allow(
        dead_code,
        reason = "removing every profile with conditional input leaves the method without a caller"
    )]
    pub(crate) fn take<T>(&mut self, result: Result<T, ValidationError>) -> Result<T, Rejected> {
        result.map_err(|error| self.reject(error))
    }

    #[allow(
        dead_code,
        reason = "removing every profile with conditional input leaves the method without a caller"
    )]
    pub(crate) fn reject(&mut self, error: ValidationError) -> Rejected {
        self.0.push(error);
        Rejected(())
    }

    /// The admitted value when no rule was violated. A [`Rejected`] value
    /// always arrives with a recorded violation, so it cannot pass.
    pub(crate) fn finish<T>(self, admitted: Result<T, Rejected>) -> Result<T, ValidationErrors> {
        match admitted {
            Ok(value) if self.0.is_empty() => Ok(value),
            Ok(_) | Err(_) => Err(ValidationErrors(self.0)),
        }
    }
}

/// Run one admission step alone, as [`crate::ConfigInput::admit`] runs it.
#[cfg(test)]
pub(crate) fn admit_with<T>(
    admit: impl FnOnce(&mut Violations) -> Result<T, Rejected>,
) -> Result<T, ValidationErrors> {
    let mut violations = Violations::default();
    let admitted = admit(&mut violations);
    violations.finish(admitted)
}

/// Run one section validator alone and return what it reported.
#[cfg(test)]
pub(crate) fn validate_with(
    validate: impl FnOnce(&mut Violations),
) -> Result<(), ValidationErrors> {
    admit_with(|violations| {
        validate(violations);
        Ok(())
    })
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

pub(crate) fn non_blank(key: &str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(key, "cannot be empty"));
    }
    Ok(())
}

#[allow(
    dead_code,
    reason = "removing every authentication profile leaves the rule without a caller"
)]
pub(crate) fn required_when(
    key: &str,
    value: &str,
    condition: &str,
) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(missing(key, condition));
    }
    Ok(())
}

#[allow(
    dead_code,
    reason = "removing every profile with a conditional key leaves the rule without a caller"
)]
pub(crate) fn required<T>(
    key: &str,
    value: Option<T>,
    condition: &str,
) -> Result<T, ValidationError> {
    value.ok_or_else(|| missing(key, condition))
}

/// `key` is absent although `condition` requires it. A secret-like key names
/// its variable: a config file may not carry its value, so the variable, or a
/// secrets-directory file of the same name, is the only place to set it.
pub(crate) fn missing(key: &str, condition: &str) -> ValidationError {
    let mut message = format!("is required when {condition}");
    if is_secret_like_key(key) {
        message.push_str("; set ");
        message.push_str(&variable_name(key));
    }
    ValidationError::new(key, message)
}

#[allow(
    dead_code,
    reason = "removing both webhook profiles leaves the shared rule without a caller"
)]
pub(crate) fn validate_endpoint_id(
    section: &str,
    endpoint_id: &str,
) -> Result<(), ValidationError> {
    if endpoint_id.is_empty() || endpoint_id.contains('\0') {
        return Err(ValidationError::new(
            section,
            "endpoint IDs must be nonempty and NUL-free",
        ));
    }
    Ok(())
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
            Duration::from_mins(10),
        );
        assert_eq!(result.is_ok(), valid);
    }

    #[test]
    fn duration_range_names_bounds() {
        let err = duration_range(
            "http.request_timeout",
            Duration::from_millis(10),
            Duration::from_millis(100),
            Duration::from_mins(10),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "http.request_timeout: must be between 100ms and 10m, got 10ms"
        );
    }
}
