//! Process logger level and output format.

use serde::Deserialize;

use crate::validate::{ValidationError, non_blank};

/// Output format of the process logger, selected by `log.format`.
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
    /// `info,sqlx=warn`. `RUST_LOG` is not read; `APP__LOG__LEVEL` is the
    /// override channel like every other key.
    pub level: String,
    /// Output format of log records; `json` by default.
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
        // The subscriber's parser owns the directive grammar.
        non_blank("log.level", &self.level)
    }
}
