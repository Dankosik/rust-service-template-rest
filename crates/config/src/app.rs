//! Service identity: environment, version, commit, instance.

use serde::Deserialize;

use crate::validate::{ValidationError, non_empty};

/// Values the binary knows at build time and the loader uses as defaults
/// for `app.version` and `app.commit` when the snapshot leaves them empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    /// Usually `env!("CARGO_PKG_VERSION")` of the calling package.
    pub version: &'static str,
    /// The source revision stamped by this crate's build script; `unknown`
    /// when absent.
    pub commit: &'static str,
}

impl BuildInfo {
    /// Package version from the calling binary, commit from this crate's
    /// `VERGEN_GIT_SHA` stamp.
    #[must_use]
    pub const fn from_package_version(version: &'static str) -> Self {
        Self {
            version,
            commit: env!("VERGEN_GIT_SHA"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct AppConfig {
    /// Deployment environment name published as
    /// `deployment.environment.name`.
    pub env: String,
    /// Service version published as `service.version`. Empty means "use the
    /// build's own version".
    pub version: String,
    /// Source revision published as `vcs.ref.head.revision`. Empty means
    /// "use the revision stamped into the binary".
    pub commit: String,
    /// Replica identity published as `service.instance.id`.
    ///
    /// `None` (missing, empty, or whitespace-only wire value) is a vacant
    /// replica id: the composition root fills the hostname (the pod name on
    /// Kubernetes). Version and commit empty sentinels are replaced from
    /// [`BuildInfo`] before validation; this field is not. Without an
    /// instance identity every replica pushes the same resource and their
    /// cumulative counters collide into one series.
    #[serde(default, deserialize_with = "occupied_string")]
    pub instance_id: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            env: "local".to_owned(),
            version: String::new(),
            commit: String::new(),
            instance_id: None,
        }
    }
}

impl AppConfig {
    pub(crate) fn apply_build_info(&mut self, build: BuildInfo) {
        if self.version.trim().is_empty() {
            build.version.clone_into(&mut self.version);
        }
        if self.commit.trim().is_empty() {
            build.commit.clone_into(&mut self.commit);
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        non_empty("app.env", &self.env)?;
        non_empty("app.version", &self.version)?;
        non_empty("app.commit", &self.commit)?;
        Ok(())
    }
}

/// Missing, empty, or whitespace-only replica id is vacant (`None`).
fn occupied_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.and_then(|s| {
        let trimmed = s.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }))
}
