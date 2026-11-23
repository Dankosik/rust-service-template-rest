use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::app::BuildInfo;
use crate::secret_policy::first_secret_key;
use crate::{Config, ConfigInput, LoadOptions, ValidationErrors};

mod tree;
mod variables;

use variables::collect_variables;
pub(crate) use variables::{ADDRESSABLE_KEY_FORM, is_env_addressable, variable_name};

const ENV_PREFIX: &str = "APP";
const ENV_SEPARATOR: &str = "__";

const NAME_FORM: &str = "each segment between `__` separators must be letters, digits, `_`, or `-`";

const RETAINED_SECTIONS: &[&str] = &[
    "app",
    "http",
    // template:begin grpc:config-section
    "grpc",
    // template:end grpc:config-section
    // template:begin inbound-webhooks:config-section
    "inbound_webhooks",
    // template:end inbound-webhooks:config-section
    "health",
    "log",
    "runtime",
    "observability",
    // template:begin messaging:config-section
    "messaging",
    // template:end messaging:config-section
    // template:begin cache:config-section
    "cache",
    // template:end cache:config-section
    // template:begin object-storage:config-section
    "object_storage",
    // template:end object-storage:config-section
    // template:begin clickhouse:config-section
    "clickhouse",
    // template:end clickhouse:config-section
    // template:begin authn:config-section
    "authn",
    // template:end authn:config-section
    // template:begin service-auth:config-section
    "service_auth",
    // template:end service-auth:config-section
    // template:begin postgres:config-section
    "postgres",
    // template:end postgres:config-section
    // template:begin http-idempotency:config-section
    "http_idempotency",
    // template:end http-idempotency:config-section
    // template:begin jobs:config-section
    "jobs",
    // template:end jobs:config-section
    // template:begin webhooks:config-section
    "webhooks",
    // template:end webhooks:config-section
];

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error(pub(crate) ErrorKind);

impl Error {
    #[must_use]
    pub const fn validation_errors(&self) -> Option<&ValidationErrors> {
        match &self.0 {
            ErrorKind::Validate(errors) => Some(errors),
            _ => None,
        }
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Self(kind)
    }
}

impl From<InputError> for Error {
    fn from(error: InputError) -> Self {
        Self(ErrorKind::Input(error))
    }
}

impl From<ValidationErrors> for Error {
    fn from(errors: ValidationErrors) -> Self {
        Self(ErrorKind::Validate(errors))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ErrorKind {
    #[error(transparent)]
    Input(InputError),
    #[error(
        "configuration inputs are invalid: {}",
        crate::validate::join_errors(.0)
    )]
    Inputs(Vec<InputError>),
    #[error("configuration is invalid: {0}")]
    Deserialize(String),
    #[error("configuration is invalid: {0}")]
    Validate(ValidationErrors),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InputError {
    #[error("environment variable {name} is malformed: {NAME_FORM}")]
    MalformedEnvName { name: String },
    #[error("environment variable {name} holds a value that is not valid Unicode")]
    NonUnicodeEnvValue { name: String },
    #[error("secrets directory {}: {error}", path.display())]
    ReadSecretsDir {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("secret file {}: {error}", path.display())]
    ReadSecret {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error(
        "secret file {} is malformed: its name is a variable name, and {NAME_FORM}",
        path.display()
    )]
    MalformedSecretName { path: PathBuf },
    #[error("{name} and {other} set the same key")]
    AmbiguousName { name: String, other: String },
    #[error("{parent} and {child} set overlapping keys: one key is inside the other")]
    OverlappingVariables { parent: String, child: String },
    #[error(
        "{input} cannot replace configuration table `{key}` with a value. Override its keys individually"
    )]
    TableOverride { key: String, input: String },
    #[error("config file {}: {error}", path.display())]
    ReadFile {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("config file {}: {message}", path.display())]
    ParseFile { path: PathBuf, message: String },
    #[error(
        "secret key `{key}` carries a value in config file {}; use an `{ENV_PREFIX}{ENV_SEPARATOR}` environment variable or a matching file in --secrets-dir",
        path.display()
    )]
    SecretInFile { key: String, path: PathBuf },
    #[error(
        "key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable; use {ADDRESSABLE_KEY_FORM} for table keys and keep them outside arrays",
        path.display()
    )]
    UnaddressableKey { key: String, path: PathBuf },
}

pub fn load(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: crate::ShutdownTail,
) -> Result<Config, Error> {
    load_with_tail_from(options, build, shutdown_tail, std::env::vars_os())
}

#[cfg(test)]
pub(crate) fn load_from(
    options: &LoadOptions,
    build: BuildInfo,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Config, Error> {
    load_with_tail_from(options, build, crate::http::NO_SHUTDOWN_TAIL, environment)
}

pub(crate) fn load_with_tail_from(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: crate::ShutdownTail,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Config, Error> {
    let input: ConfigInput = load_input(options, environment)?;
    Ok(input.admit(build, shutdown_tail)?)
}

// template:begin worker:load-worker
pub fn load_worker(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: crate::ShutdownTail,
) -> Result<crate::WorkerConfig, Error> {
    load_worker_with_tail_from(options, build, shutdown_tail, std::env::vars_os())
}

#[cfg(test)]
pub(crate) fn load_worker_from(
    options: &LoadOptions,
    build: BuildInfo,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<crate::WorkerConfig, Error> {
    load_worker_with_tail_from(options, build, crate::http::NO_SHUTDOWN_TAIL, environment)
}

pub(crate) fn load_worker_with_tail_from(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: crate::ShutdownTail,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<crate::WorkerConfig, Error> {
    let input: crate::WorkerInput = load_input(options, environment)?;
    Ok(input.admit(build, shutdown_tail)?)
}
// template:end worker:load-worker

// template:begin postgres:load-migration
#[derive(Debug, thiserror::Error)]
pub enum MigrationLoadError {
    #[error(transparent)]
    Config(#[from] Error),
    #[error("postgres.enabled must be true to run migrations")]
    PostgresDisabled { log: crate::LogConfig },
}

pub fn load_migration(
    options: &LoadOptions,
    build: BuildInfo,
) -> Result<crate::MigrationConfig, MigrationLoadError> {
    load_migration_from(options, build, std::env::vars_os())
}

pub(crate) fn load_migration_from(
    options: &LoadOptions,
    build: BuildInfo,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<crate::MigrationConfig, MigrationLoadError> {
    let input: crate::postgres::MigrationInput = load_input(options, environment)?;
    input.admit(build)
}
// template:end postgres:load-migration

// template:begin jobs:load-jobs-operator
#[derive(Debug, thiserror::Error)]
pub enum JobsOperatorLoadError {
    #[error(transparent)]
    Config(#[from] Error),
    #[error("postgres.enabled must be true to run jobs operator commands")]
    PostgresDisabled,
}

pub fn load_jobs_operator(
    options: &LoadOptions,
) -> Result<crate::JobsOperatorConfig, JobsOperatorLoadError> {
    load_jobs_operator_from(options, std::env::vars_os())
}

pub(crate) fn load_jobs_operator_from(
    options: &LoadOptions,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<crate::JobsOperatorConfig, JobsOperatorLoadError> {
    let input: crate::jobs::JobsOperatorInput = load_input(options, environment)?;
    input.admit()
}
// template:end jobs:load-jobs-operator

fn load_input<T: DeserializeOwned>(
    options: &LoadOptions,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<T, Error> {
    let (files, variables) = collect_inputs(options, environment)?;
    merge_inputs(files, variables)
}

fn collect_inputs(
    options: &LoadOptions,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<(Vec<ScannedFile>, variables::Variables), Error> {
    let mut errors = Vec::new();
    let variables = match collect_variables(options.secrets_dir.as_deref(), environment) {
        Ok(variables) => variables,
        Err(error) => {
            errors.push(error);
            variables::Variables::new()
        }
    };
    let mut files = Vec::new();
    for path in options.files() {
        match scan_file(path) {
            Ok(file) => files.push(file),
            Err(error) => errors.push(error),
        }
    }
    match errors.len() {
        0 => Ok((files, variables)),
        1 => Err(errors.remove(0).into()),
        _ => Err(ErrorKind::Inputs(errors).into()),
    }
}

fn merge_inputs<T: DeserializeOwned>(
    files: Vec<ScannedFile>,
    variables: variables::Variables,
) -> Result<T, Error> {
    let root = tree::merge(files, variables)?;
    if let Some((key, path)) = root.first_datetime() {
        return Err(ErrorKind::Deserialize(format!(
            "key `{key}` in {} must not be an unquoted TOML datetime; quote it as a string",
            path.display()
        ))
        .into());
    }
    if let Some(key) = root.first_unknown_section(RETAINED_SECTIONS) {
        return Err(ErrorKind::Deserialize(
            <tree::DecodeError as serde::de::Error>::unknown_field(key, RETAINED_SECTIONS).render(),
        )
        .into());
    }
    root.deserialize_into()
        .map_err(|error| ErrorKind::Deserialize(error.render()).into())
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "only profiles with secret-gated fields deserialize a test node directly"
)]
pub(crate) fn deserialize_toml_for_test<T: DeserializeOwned>(body: &str) -> Result<T, String> {
    let table: toml::Table = toml::from_str(body).expect("fixture must be valid TOML");
    tree::from_test_toml(table)
        .deserialize_into()
        .map_err(|error| error.render())
}

pub(super) struct ScannedFile {
    pub(super) path: PathBuf,
    pub(super) table: toml::Table,
}

#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns this file read; startup can wait on the filesystem"
)]
fn scan_file(path: &Path) -> Result<ScannedFile, InputError> {
    let text =
        Zeroizing::new(
            std::fs::read_to_string(path).map_err(|error| InputError::ReadFile {
                path: path.to_owned(),
                error,
            })?,
        );
    let table: toml::Table = toml::from_str(&text).map_err(|error| {
        let message = match error.span() {
            Some(span) => format!("invalid TOML at byte {}", span.start),
            None => "invalid TOML".to_owned(),
        };
        InputError::ParseFile {
            path: path.to_owned(),
            message,
        }
    })?;
    if let Some(key) = first_unaddressable_key(&table, &mut Vec::new()) {
        return Err(InputError::UnaddressableKey {
            key,
            path: path.to_owned(),
        });
    }
    if let Some(key) = first_secret_key(&table) {
        return Err(InputError::SecretInFile {
            key,
            path: path.to_owned(),
        });
    }
    Ok(ScannedFile {
        path: path.to_owned(),
        table,
    })
}

fn first_unaddressable_key<'a>(
    table: &'a toml::Table,
    key_segments: &mut Vec<&'a str>,
) -> Option<String> {
    for (key, value) in table {
        key_segments.push(key);
        let found = if is_env_addressable(key) {
            match value {
                toml::Value::Table(nested) => first_unaddressable_key(nested, key_segments),
                toml::Value::Array(_) => first_key_in_array(value)
                    .map(|suffix| format!("{}{suffix}", key_segments.join("."))),
                _ => None,
            }
        } else {
            Some(key_segments.join("."))
        };
        key_segments.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn first_key_in_array(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::Array(items) => items.iter().enumerate().find_map(|(index, item)| {
            first_key_in_array(item).map(|suffix| format!("[{index}]{suffix}"))
        }),
        toml::Value::Table(table) => table.keys().next().map(|key| format!(".{key}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
