use std::time::Duration;

use serde::Deserialize;

use crate::de::blank_as_none;
use crate::validate::{ValidationError, Violations, duration_range, required};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HttpIdempotencyConfig {
    /// How long a stored outcome stays live after the executing attempt
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

fn blank_duration_as_none<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    blank_as_none::<_, String>(deserializer)?
        .map(|text| crate::de::decoded_duration(&text, "a duration such as 24h"))
        .transpose()
}

#[cfg(test)]
mod tests;
