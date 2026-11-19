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

pub(crate) fn validate_local_development(
    env: &str,
    requested: bool,
) -> Result<bool, ValidationError> {
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
    local_development: bool,
) -> Result<(), ValidationError> {
    if enabled && !local_development {
        return Err(ValidationError::new(
            key,
            "requires app.allow_insecure_development = true",
        ));
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
struct BlankAsNoneString(
    #[serde(default, deserialize_with = "crate::de::blank_as_none")] Option<String>,
);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct AppFields<Version, Commit, InstanceId> {
    #[serde(deserialize_with = "crate::de::string")]
    pub(crate) env: String,
    #[serde(deserialize_with = "crate::de::boolean")]
    pub(crate) allow_insecure_development: bool,
    version: Version,
    commit: Commit,
    instance_id: InstanceId,
}

impl<Version: Default, Commit: Default, InstanceId: Default> Default
    for AppFields<Version, Commit, InstanceId>
{
    fn default() -> Self {
        Self {
            env: default_env(),
            allow_insecure_development: false,
            version: Version::default(),
            commit: Commit::default(),
            instance_id: InstanceId::default(),
        }
    }
}

type AppInput = AppFields<crate::de::StrictString, crate::de::StrictString, BlankAsNoneString>;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(from = "AppInput")]
pub struct AppConfig {
    pub env: String,
    /// Permit development-only settings when `env` is `local` or `development`.
    pub allow_insecure_development: bool,
    /// Service version published as `service.version`. Empty means "use the
    /// build's own version".
    pub version: String,
    /// Source revision published as `vcs.ref.head.revision`. Empty means
    /// "use the revision stamped into the binary".
    pub commit: String,
    /// Replica identity published as `service.instance.id`. Set it to a distinct
    /// value per replica. An omitted or blank value uses a process-local UUID.
    pub instance_id: Option<String>,
}

impl From<AppInput> for AppConfig {
    fn from(input: AppInput) -> Self {
        Self {
            env: input.env,
            allow_insecure_development: input.allow_insecure_development,
            version: input.version.0,
            commit: input.commit.0,
            instance_id: input.instance_id.0,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::from(AppInput::default())
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
    type MigrationAppInput =
        AppFields<crate::de::StrictString, crate::de::StrictString, serde::de::IgnoredAny>;

    let input = MigrationAppInput::deserialize(deserializer)?;
    Ok(AppConfig {
        env: input.env,
        allow_insecure_development: input.allow_insecure_development,
        version: input.version.0,
        commit: input.commit.0,
        instance_id: None,
    })
}
// template:end postgres:migration-app-input

impl AppConfig {
    #[must_use]
    pub fn is_local_development(&self) -> bool {
        validate_local_development(&self.env, self.allow_insecure_development).unwrap_or(false)
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
        v.check(validate_local_development(&self.env, self.allow_insecure_development).map(|_| ()));
        v.check(non_blank("app.version", &self.version));
        v.check(non_blank("app.commit", &self.commit));
    }
}
