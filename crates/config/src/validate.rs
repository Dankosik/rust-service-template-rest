use std::ops::RangeInclusive;
use std::time::Duration;

use itertools::Itertools as _;

use crate::load::variable_name;
use crate::secret_policy::is_secret_key;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{key}: {message}")]
pub struct ValidationError {
    pub key: String,
    pub message: String,
}

impl ValidationError {
    #[must_use]
    pub fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, derive_more::IntoIterator)]
#[error("{}", join_errors(.0))]
#[into_iterator(ref)]
pub struct ValidationErrors(Vec<ValidationError>);

impl ValidationErrors {
    pub fn iter(&self) -> std::slice::Iter<'_, ValidationError> {
        self.0.iter()
    }

    #[must_use]
    pub fn names(&self, key: &str) -> bool {
        self.0.iter().any(|error| error.key == key)
    }
}

pub(crate) fn join_errors<E: std::fmt::Display>(errors: &[E]) -> impl std::fmt::Display {
    errors.iter().format("; ")
}

#[derive(Debug)]
pub(crate) struct Rejected(());

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
    pub(crate) fn record<T>(&mut self, result: Result<T, ValidationError>) -> Result<T, Rejected> {
        result.map_err(|error| self.reject(error))
    }

    pub(crate) fn reject(&mut self, error: ValidationError) -> Rejected {
        self.0.push(error);
        Rejected(())
    }

    #[allow(dead_code, reason = "only optional profiles reject fields by mode")]
    pub(crate) fn reject_present(
        &mut self,
        fields: &[(&str, bool)],
        message: &str,
    ) -> Option<Rejected> {
        let mut rejected = None;
        for &(key, present) in fields {
            if present {
                rejected = Some(self.reject(ValidationError::new(key, message)));
            }
        }
        rejected
    }

    fn finish<T>(self, admitted: Result<T, Rejected>) -> Result<T, ValidationErrors> {
        match admitted {
            Ok(value) if self.0.is_empty() => Ok(value),
            Ok(_) | Err(_) => Err(ValidationErrors(self.0)),
        }
    }
}

pub(crate) fn admit_with<T>(
    admit: impl FnOnce(&mut Violations) -> Result<T, Rejected>,
) -> Result<T, ValidationErrors> {
    let mut violations = Violations::default();
    let admitted = admit(&mut violations);
    violations.finish(admitted)
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "only optional profiles call validate_with in unit tests"
)]
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
    range: RangeInclusive<Duration>,
) -> Result<(), ValidationError> {
    if !range.contains(&value) {
        return Err(ValidationError::new(
            key,
            format!(
                "must be between {} and {}, got {}",
                crate::de::display_duration(*range.start()),
                crate::de::display_duration(*range.end()),
                crate::de::display_duration(value)
            ),
        ));
    }
    Ok(())
}

pub(crate) fn int_range<T: Copy + PartialOrd + std::fmt::Display>(
    key: &str,
    value: T,
    range: RangeInclusive<T>,
) -> Result<(), ValidationError> {
    if !range.contains(&value) {
        return Err(ValidationError::new(
            key,
            format!(
                "must be between {} and {}, got {value}",
                range.start(),
                range.end()
            ),
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
pub(crate) fn required_non_blank(
    key: &str,
    value: Option<String>,
    condition: &str,
) -> Result<String, ValidationError> {
    value
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| missing(key, condition))
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

#[allow(
    dead_code,
    reason = "removing every profile with paired optional keys leaves the rule without a caller"
)]
pub(crate) fn optional_pair<A, B>(
    first_key: &str,
    first: Option<A>,
    second_key: &str,
    second: Option<B>,
) -> Result<Option<(A, B)>, ValidationError> {
    match (first, second) {
        (Some(first), Some(second)) => Ok(Some((first, second))),
        (Some(_), None) => Err(missing(second_key, &format!("{first_key} is set"))),
        (None, Some(_)) => Err(missing(first_key, &format!("{second_key} is set"))),
        (None, None) => Ok(None),
    }
}

pub(crate) fn missing(key: &str, condition: &str) -> ValidationError {
    let mut message = format!("is required when {condition}");
    if is_secret_key(key) {
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
        assert_eq!(int_range("limit", value, 1..=10).is_ok(), valid);
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
            Duration::from_millis(100)..=Duration::from_mins(10),
        );
        assert_eq!(result.is_ok(), valid);
    }

    #[test]
    fn duration_range_names_bounds() {
        let err = duration_range(
            "http.request_timeout",
            Duration::from_millis(10),
            Duration::from_millis(100)..=Duration::from_mins(10),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "http.request_timeout: must be between 100ms and 10m, got 10ms"
        );
    }
}
