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

use crate::de::blank_as_none;
use crate::validate::{ValidationError, duration_range, required};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpIdempotencyConfig {
    /// How long a stored success stays live after the executing attempt
    /// writes it, measured on the database clock
    /// (`APP__HTTP_IDEMPOTENCY__RETENTION`). Missing, empty, or
    /// whitespace-only is vacant (`None`). A set value must be between 1
    /// minute and 30 days inclusive; that range is checked whether or not
    /// the pack is active.
    #[serde(default, deserialize_with = "blank_duration_as_none")]
    pub retention: Option<Duration>,
}

impl HttpIdempotencyConfig {
    pub const RETENTION_KEY: &str = "http_idempotency.retention";

    /// The retention an idempotent operation needs before it may be served.
    ///
    /// # Errors
    ///
    /// Returns `postgres.enabled` (checked first) when PostgreSQL is
    /// disabled, or `http_idempotency.retention` when PostgreSQL is enabled
    /// but no retention is set.
    pub fn required_retention(&self, postgres_enabled: bool) -> Result<Duration, ValidationError> {
        if !postgres_enabled {
            return Err(ValidationError::new(
                "postgres.enabled",
                "must be true when an idempotent operation is served",
            ));
        }
        required(
            Self::RETENTION_KEY,
            self.retention,
            "an idempotent operation is served",
        )
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        if let Some(retention) = self.retention {
            duration_range(
                Self::RETENTION_KEY,
                retention,
                Duration::from_secs(60),
                Duration::from_hours(30 * 24),
            )?;
        }
        Ok(())
    }
}

fn blank_duration_as_none<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    blank_as_none::<_, String>(deserializer)?
        .map(|text| {
            humantime::parse_duration(&text).map_err(|_| {
                serde::de::Error::invalid_value(
                    serde::de::Unexpected::Str(&text),
                    &"a duration such as 24h",
                )
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests;
