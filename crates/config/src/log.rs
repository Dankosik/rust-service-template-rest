//! Process logger level and output format.

use serde::Deserialize;

use crate::validate::{ValidationError, non_empty};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// One JSON object per line, flattened event and span fields, trace and
    /// span ids on every record inside a request.
    #[default]
    Json,
    /// Human-readable single-line output for local development.
    Text,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct LogConfig {
    /// A `tracing_subscriber::EnvFilter` directive, for example `info` or
    /// `info,hyper=warn`. `RUST_LOG` is not read; `APP__LOG__LEVEL` is the
    /// override channel like every other key.
    pub level: String,
    pub format: LogFormat,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            format: LogFormat::Json,
        }
    }
}

impl LogConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        non_empty("log.level", &self.level)?;
        // The directive grammar is owned by tracing-subscriber; only its own
        // parser can reject it, and the composition root does that before
        // installing the subscriber. Here we reject the values that cannot
        // be a directive at all.
        if self.level.chars().any(char::is_whitespace) {
            return Err(ValidationError::new(
                "log.level",
                "must be a filter directive without whitespace, for example `info` or `info,hyper=warn`",
            ));
        }
        Ok(())
    }
}
