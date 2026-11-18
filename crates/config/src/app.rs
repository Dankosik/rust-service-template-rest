use serde::Deserialize;

use crate::validate::{ValidationError, Violations, non_blank};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub commit: &'static str,
}

impl BuildInfo {
    #[must_use]
    pub const fn from_package_version(version: &'static str) -> Self {
        Self {
            version,
            commit: env!("VERGEN_GIT_SHA"),
        }
    }
}

pub(crate) fn insecure_development(env: &str, requested: bool) -> Result<bool, ValidationError> {
    if requested && !matches!(env, "local" | "development") {
        return Err(ValidationError::new(
            "app.allow_insecure_development",
            "requires app.env to be local or development",
        ));
    }
    Ok(requested)
}

#[allow(
    dead_code,
    reason = "removing every profile with an escape hatch leaves the rule without a caller"
)]
pub(crate) fn local_development_only(
    key: &str,
    enabled: bool,
    insecure_development: bool,
) -> Result<(), ValidationError> {
    if enabled && !insecure_development {
        return Err(ValidationError::new(
            key,
            "requires app.allow_insecure_development = true",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct AppConfig {
    #[serde(deserialize_with = "crate::de::string")]
    pub env: String,
    /// Permit development-only settings when `env` is `local` or `development`.
    #[serde(deserialize_with = "crate::de::boolean")]
    pub allow_insecure_development: bool,
    /// Service version published as `service.version`. Empty means "use the
    /// build's own version".
    #[serde(deserialize_with = "crate::de::string")]
    pub version: String,
    /// Source revision published as `vcs.ref.head.revision`. Empty means
    /// "use the revision stamped into the binary".
    #[serde(deserialize_with = "crate::de::string")]
    pub commit: String,
    /// Replica identity published as `service.instance.id`.
    ///
    /// `None` (missing, empty, or whitespace-only wire value) is a vacant
    /// replica id: the telemetry resource builder fills the hostname (the pod
    /// name on Kubernetes). Version and commit empty sentinels are replaced from
    /// [`BuildInfo`] before validation; this field is not. Without an
    /// instance identity every replica pushes the same resource and their
    /// cumulative counters collide into one series.
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
    pub instance_id: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            env: default_env(),
            allow_insecure_development: false,
            version: String::new(),
            commit: String::new(),
            instance_id: None,
        }
    }
}

fn default_env() -> String {
    "unspecified".to_owned()
}

// template:begin postgres:migration-app-input
pub(crate) fn deserialize_migration_app<'de, D>(deserializer: D) -> Result<AppConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MigrationAppInput {
        #[serde(default = "default_env", deserialize_with = "crate::de::string")]
        env: String,
        #[serde(default, deserialize_with = "crate::de::boolean")]
        allow_insecure_development: bool,
        #[serde(default, deserialize_with = "crate::de::string")]
        version: String,
        #[serde(default, deserialize_with = "crate::de::string")]
        commit: String,
        #[serde(default, rename = "instance_id")]
        _instance_id: serde::de::IgnoredAny,
    }

    let MigrationAppInput {
        env,
        allow_insecure_development,
        version,
        commit,
        ..
    } = MigrationAppInput::deserialize(deserializer)?;
    Ok(AppConfig {
        env,
        allow_insecure_development,
        version,
        commit,
        instance_id: None,
    })
}
// template:end postgres:migration-app-input

impl AppConfig {
    #[must_use]
    pub fn is_local_development(&self) -> bool {
        insecure_development(&self.env, self.allow_insecure_development).unwrap_or(false)
    }

    pub(crate) fn apply_build_info(&mut self, build: BuildInfo) {
        if self.version.trim().is_empty() {
            build.version.clone_into(&mut self.version);
        }
        if self.commit.trim().is_empty() {
            build.commit.clone_into(&mut self.commit);
        }
    }

    pub(crate) fn validate(&self, v: &mut Violations) {
        v.check(non_blank("app.env", &self.env));
        v.check(insecure_development(&self.env, self.allow_insecure_development).map(|_| ()));
        v.check(non_blank("app.version", &self.version));
        v.check(non_blank("app.commit", &self.commit));
    }
}
