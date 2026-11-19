use serde::Deserialize;

use crate::de::{BlankAsNone, Keep, Mode, ModeMarker, StrictBool, StrictString, When};
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

#[serde_with::serde_as]
#[derive(Debug, Deserialize, educe::Educe)]
#[educe(Default(bound = false))]
#[serde(bound(deserialize = ""), deny_unknown_fields, default)]
pub(crate) struct AppFields<Meta: Mode = Keep, Instance: Mode = Keep> {
    #[serde_as(as = "StrictString")]
    #[educe(Default(expression = "unspecified".to_owned()))]
    pub(crate) env: String,
    #[serde_as(as = "StrictBool")]
    pub(crate) allow_insecure_development: bool,
    #[serde_as(as = "When<Meta, StrictString>")]
    version: String,
    #[serde_as(as = "When<Meta, StrictString>")]
    commit: String,
    #[serde(default)]
    #[serde_as(as = "When<Instance, BlankAsNone>")]
    instance_id: Option<String>,
    #[serde(skip)]
    _mode: ModeMarker<(Meta, Instance)>,
}

type AppInput = AppFields;

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

impl<Instance: Mode> From<AppFields<Keep, Instance>> for AppConfig {
    fn from(input: AppFields<Keep, Instance>) -> Self {
        Self {
            env: input.env,
            allow_insecure_development: input.allow_insecure_development,
            version: input.version,
            commit: input.commit,
            instance_id: input.instance_id,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::from(AppInput::default())
    }
}

// template:begin postgres:migration-app-input
pub(crate) fn deserialize_migration_app<'de, D>(deserializer: D) -> Result<AppConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    AppFields::<Keep, crate::de::Skip>::deserialize(deserializer).map(AppConfig::from)
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
