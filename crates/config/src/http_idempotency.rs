use std::time::Duration;

use serde::Deserialize;
use serde_with::DeserializeAs;

use crate::de::{BlankAsNone, decoded_duration};
use crate::validate::{ValidationError, Violations, duration_range, required};

#[serde_with::serde_as]
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpIdempotencyConfig {
    /// How long a stored outcome stays live after the executing attempt
    /// writes it, measured on the database clock
    /// (`APP__HTTP_IDEMPOTENCY__RETENTION`). Missing, empty, or
    /// whitespace-only is vacant (`None`). A set value must be between 1
    /// minute and 30 days inclusive; that range is checked whether or not
    /// the pack is active.
    #[serde(default)]
    #[serde_as(as = "BlankAsNone<RetentionDuration>")]
    pub retention: Option<Duration>,
}

impl HttpIdempotencyConfig {
    pub const RETENTION_KEY: &str = "http_idempotency.retention";

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

    pub(crate) fn validate(&self, violations: &mut Violations) {
        if let Some(retention) = self.retention {
            violations.check(duration_range(
                Self::RETENTION_KEY,
                retention,
                Duration::from_mins(1)..=Duration::from_hours(30 * 24),
            ));
        }
    }
}

struct RetentionDuration;

impl<'de> DeserializeAs<'de, Duration> for RetentionDuration {
    fn deserialize_as<D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        decoded_duration(&text, "a duration such as 24h")
    }
}

#[cfg(test)]
mod tests;
