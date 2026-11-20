use serde::Deserialize;

use crate::validate::{Violations, non_blank};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// One JSON object per line, flattened event and span fields, trace and
    /// span ids on every record inside a request.
    #[default]
    Json,
    Text,
}

#[derive(Clone, derive_more::Debug, Deserialize, PartialEq, Eq, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub struct LogConfig {
    /// A `tracing_subscriber::EnvFilter` directive, for example `info` or
    /// `info,sqlx=warn`. `RUST_LOG` is not read; `APP__LOG__LEVEL` is the
    /// override channel like every other key.
    #[educe(Default(expression = "info".to_owned()))]
    #[debug(skip)]
    pub level: String,
    pub format: LogFormat,
}

impl LogConfig {
    pub(crate) fn validate(&self, v: &mut Violations) {
        v.check(non_blank("log.level", &self.level));
    }
}
