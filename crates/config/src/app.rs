//! Service identity: environment, version, commit, instance.

use serde::Deserialize;

use crate::validate::{ValidationError, non_empty};

/// Values the binary knows at build time and the loader uses as defaults
/// for `app.version` and `app.commit` when the snapshot leaves them empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    /// Usually `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
    /// The source revision stamped by the build; `unknown` when absent.
    pub commit: &'static str,
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
    /// Replica identity published as `service.instance.id`. Empty is an
    /// occupancy signal, not a load default: the composition root fills
    /// the hostname (the pod name on Kubernetes). Version and commit empty
    /// sentinels are replaced from [`BuildInfo`] before validation; this
    /// field is not. Without an instance identity every replica pushes the
    /// same resource and their cumulative counters collide into one series.
    pub instance_id: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            env: "local".to_owned(),
            version: String::new(),
            commit: String::new(),
            instance_id: String::new(),
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

    /// Named replica identity, if the snapshot set one.
    ///
    /// `None` means the composition root should use the hostname. This is
    /// not filled at load: hostname is a host probe, not a config default.
    #[must_use]
    pub fn instance_id(&self) -> Option<&str> {
        let trimmed = self.instance_id.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        non_empty("app.env", &self.env)?;
        non_empty("app.version", &self.version)?;
        non_empty("app.commit", &self.commit)?;
        Ok(())
    }
}
