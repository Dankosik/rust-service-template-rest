use serde::Deserialize;

use crate::de::{BlankAsNone, Keep, Mode, ModeMarker, StrictBool, When};
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

fn validate_local_development(env: &str, requested: bool) -> Result<bool, ValidationError> {
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
    #[educe(Default(expression = "unspecified".to_owned()))]
    pub(crate) env: String,
    #[serde_as(as = "StrictBool")]
    pub(crate) allow_insecure_development: bool,
    #[serde_as(as = "When<Meta>")]
    version: String,
    #[serde_as(as = "When<Meta>")]
    commit: String,
    #[serde_as(as = "When<Instance, BlankAsNone>")]
    instance_id: Option<String>,
    #[serde(skip)]
    _mode: ModeMarker<(Meta, Instance)>,
}

pub(crate) type AppInput = AppFields;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppConfig {
    env: String,
    local_development: bool,
    /// Service version published as `service.version`. Empty on input uses the
    /// build's own version.
    pub version: String,
    /// Source revision published as `vcs.ref.head.revision`. Empty on input
    /// uses the revision stamped into the binary.
    pub commit: String,
    /// Replica identity published as `service.instance.id`. Set it to a distinct
    /// value per replica. An omitted or blank value uses a process-local UUID.
    pub instance_id: Option<String>,
}

impl<Meta: Mode, Instance: Mode> AppFields<Meta, Instance> {
    pub(crate) fn admit_insecure_development(&self, v: &mut Violations) -> bool {
        v.record(validate_local_development(
            &self.env,
            self.allow_insecure_development,
        ))
        .unwrap_or(false)
    }
}

impl<Instance: Mode> AppFields<Keep, Instance> {
    pub(crate) fn admit(self, build: BuildInfo, v: &mut Violations) -> AppConfig {
        v.check(non_blank("app.env", &self.env));
        let local_development = self.admit_insecure_development(v);
        let version = or_build_value(self.version, build.version);
        let commit = or_build_value(self.commit, build.commit);
        v.check(non_blank("app.version", &version));
        v.check(non_blank("app.commit", &commit));
        AppConfig {
            env: self.env,
            local_development,
            version,
            commit,
            instance_id: self.instance_id,
        }
    }
}

fn or_build_value(configured: String, build: &str) -> String {
    if configured.trim().is_empty() {
        build.to_owned()
    } else {
        configured
    }
}

impl AppConfig {
    #[must_use]
    pub const fn env(&self) -> &str {
        self.env.as_str()
    }

    #[must_use]
    pub const fn is_local_development(&self) -> bool {
        self.local_development
    }
}
