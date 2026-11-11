//! Service identity: environment, version, commit, instance.

use serde::Deserialize;

use crate::validate::{ValidationError, non_blank};

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

/// Whether `env` (`app.env`) names a developer environment. Only there does
/// a section accept its plaintext, unauthenticated, or emulator escape hatch;
/// any other spelling, a production typo included, is refused them.
#[must_use]
pub(crate) fn is_local_development(env: &str) -> bool {
    matches!(env, "local" | "development")
}

/// Refuses an enabled escape hatch `key` outside a developer environment.
#[allow(
    dead_code,
    reason = "removing every profile with an escape hatch leaves the rule without a caller"
)]
pub(crate) fn local_development_only(
    key: &str,
    enabled: bool,
    app_env: &str,
) -> Result<(), ValidationError> {
    if enabled && !is_local_development(app_env) {
        return Err(ValidationError::new(
            key,
            format!("is local/development-only, but app.env is \"{app_env}\""),
        ));
    }
    Ok(())
}

/// The `app` section: service identity published as telemetry resource attributes.
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
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
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
        non_blank("app.env", &self.env)?;
        non_blank("app.version", &self.version)?;
        non_blank("app.commit", &self.commit)?;
        Ok(())
    }
}
