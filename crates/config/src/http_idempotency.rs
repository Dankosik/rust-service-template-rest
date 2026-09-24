//! HTTP idempotency retention: the one runtime knob the optional
//! PostgreSQL-backed idempotency pack adds.
//!
//! The section has no enabled switch of its own; `postgres.enabled` and
//! whether any operation is composed as idempotent together decide activity
//! (see [`HttpIdempotencyConfig::required_retention`]). There is no usable
//! default, so a derived service that activates the pack must set a value
//! explicitly. A set value keeps any sub-microsecond part it names; the
//! record store truncates it once, after this range check has already run.

use std::time::Duration;

use serde::Deserialize;

use crate::app::occupied_string;
use crate::postgres::PostgresConfig;
use crate::validate::{ValidationError, duration_range};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpIdempotencyConfig {
    /// How long a stored success stays live after the executing attempt
    /// writes it, measured on the database clock
    /// (`APP__HTTP_IDEMPOTENCY__RETENTION`). Missing, empty, or
    /// whitespace-only is vacant (`None`). A set value must be between 1
    /// minute and 30 days inclusive; that range is checked whether or not
    /// the pack is active.
    #[serde(default, deserialize_with = "occupied_duration")]
    pub retention: Option<Duration>,
}

impl HttpIdempotencyConfig {
    /// The retention an idempotent operation needs before it may be served.
    ///
    /// # Errors
    ///
    /// Returns `postgres.enabled` (checked first) when PostgreSQL is
    /// disabled, or `http_idempotency.retention` when PostgreSQL is enabled
    /// but no retention is set.
    pub fn required_retention(
        &self,
        postgres: &PostgresConfig,
    ) -> Result<Duration, ValidationError> {
        if !postgres.enabled {
            return Err(ValidationError::new(
                "postgres.enabled",
                "must be true when an idempotent operation is served",
            ));
        }
        self.retention.ok_or_else(|| {
            ValidationError::new(
                "http_idempotency.retention",
                "is required when an idempotent operation is served",
            )
        })
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if let Some(retention) = self.retention {
            duration_range(
                "http_idempotency.retention",
                retention,
                Duration::from_secs(60),
                Duration::from_hours(30 * 24),
            )?;
        }
        Ok(())
    }
}

/// Missing, empty, or whitespace-only text is vacant (`None`); a present
/// value is trimmed, then parsed as a human-form duration. Any
/// sub-microsecond part it names survives parsing unchanged.
fn occupied_duration<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    occupied_string(deserializer)?
        .map(|text| {
            humantime::parse_duration(&text)
                .map_err(|err| serde::de::Error::custom(format!("{text:?}: {err}")))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<HttpIdempotencyConfig, toml::de::Error> {
        toml::from_str(toml)
    }

    #[test]
    fn defaults_are_vacant() {
        let config = HttpIdempotencyConfig::default();
        assert_eq!(config.retention, None);
        config.validate().unwrap();
    }

    #[rstest::rstest]
    #[case::seconds("90s", Duration::from_secs(90))]
    #[case::minutes("5m", Duration::from_mins(5))]
    #[case::hours_and_minutes("2h 30m", Duration::from_mins(2 * 60 + 30))]
    #[case::days_no_space("7days", Duration::from_hours(7 * 24))]
    #[case::padded_with_whitespace("  1h  ", Duration::from_secs(3600))]
    fn accepted_forms_parse_to_the_named_duration(#[case] text: &str, #[case] expected: Duration) {
        let config = parse(&format!("retention = {text:?}")).unwrap();
        assert_eq!(config.retention, Some(expected));
    }

    #[test]
    fn a_malformed_value_fails_to_deserialize() {
        assert!(parse("retention = \"not-a-duration\"").is_err());
    }

    #[rstest::rstest]
    #[case::just_below_minimum("59s", false)]
    #[case::minimum("1m", true)]
    #[case::maximum("30days", true)]
    #[case::just_above_maximum("30days 1s", false)]
    fn retention_bounds_are_inclusive(#[case] text: &str, #[case] valid: bool) {
        let config = parse(&format!("retention = {text:?}")).unwrap();
        assert_eq!(config.validate().is_ok(), valid, "{text}");
    }

    #[rstest::rstest]
    #[case::missing("")]
    #[case::empty("retention = \"\"")]
    #[case::whitespace("retention = \"   \"")]
    fn vacancy_forms_are_none(#[case] toml: &str) {
        let config = parse(toml).unwrap();
        assert_eq!(config.retention, None);
        config.validate().unwrap();
    }

    #[test]
    fn unknown_keys_in_the_section_fail() {
        assert!(parse("retention = \"1m\"\nbogus = \"x\"").is_err());
    }

    #[test]
    fn required_retention_names_postgres_enabled_first() {
        let config = HttpIdempotencyConfig::default();
        let err = config
            .required_retention(&PostgresConfig::default())
            .unwrap_err();
        assert_eq!(err.key, "postgres.enabled");
    }

    #[test]
    fn required_retention_names_the_retention_once_postgres_is_enabled() {
        let config = HttpIdempotencyConfig::default();
        let postgres = PostgresConfig {
            enabled: true,
            ..PostgresConfig::default()
        };
        let err = config.required_retention(&postgres).unwrap_err();
        assert_eq!(err.key, "http_idempotency.retention");
    }

    #[test]
    fn required_retention_succeeds_once_both_are_set() {
        let config = HttpIdempotencyConfig {
            retention: Some(Duration::from_secs(3600)),
        };
        let postgres = PostgresConfig {
            enabled: true,
            ..PostgresConfig::default()
        };
        assert_eq!(
            config.required_retention(&postgres).unwrap(),
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn a_sub_microsecond_part_loads_unchanged() {
        let config = parse("retention = \"1m 1ns\"").unwrap();
        assert_eq!(config.retention, Some(Duration::new(60, 1)));
    }

    #[test]
    fn the_retention_key_is_not_treated_as_secret_like() {
        assert!(!crate::secret_policy::is_secret_like_key(
            "http_idempotency.retention"
        ));
    }
}
