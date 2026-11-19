use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::app::BuildInfo;
use crate::secret_policy::first_secret_key;
use crate::{Config, ConfigInput, LoadOptions, ValidationErrors};

mod rejection;
mod variables;

use rejection::describe_rejection;
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
pub enum Error {
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
    #[error("secret file {} is malformed: its name is a variable name, and {NAME_FORM}", path.display())]
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
    #[error("secret key `{key}` carries a value in config file {}; use an `{ENV_PREFIX}{ENV_SEPARATOR}` environment variable or a matching file in --secrets-dir", path.display())]
    SecretInFile { key: String, path: PathBuf },
    #[error("key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable; use {ADDRESSABLE_KEY_FORM} for table keys and keep them outside arrays", path.display())]
    UnaddressableKey { key: String, path: PathBuf },
    #[error(
        "configuration inputs are invalid: {}",
        crate::validate::join_errors(.0)
    )]
    InputErrors(Vec<Error>),
    #[error("load configuration: {0}")]
    Merge(String),
    #[error("configuration is invalid: {0}")]
    Deserialize(String),
    #[error("configuration is invalid: {0}")]
    Validate(ValidationErrors),
}

impl From<ValidationErrors> for Error {
    fn from(errors: ValidationErrors) -> Self {
        Self::Validate(errors)
    }
}

pub fn load(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: crate::ShutdownTail,
) -> Result<Config, Error> {
    load_with_tail_from(options, build, Some(shutdown_tail), std::env::vars_os())
}

#[cfg(test)]
pub(crate) fn load_from(
    options: &LoadOptions,
    build: BuildInfo,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Config, Error> {
    load_with_tail_from(options, build, None, environment)
}

pub(crate) fn load_with_tail_from(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: Option<crate::ShutdownTail>,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Config, Error> {
    let mut input: ConfigInput = merge(options, environment)?;
    input.app.apply_build_info(build);
    Ok(input.admit(shutdown_tail)?)
}

// template:begin worker:load-worker
pub fn load_worker(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: crate::ShutdownTail,
) -> Result<crate::WorkerConfig, Error> {
    load_worker_with_tail_from(options, build, Some(shutdown_tail), std::env::vars_os())
}

#[cfg(test)]
pub(crate) fn load_worker_from(
    options: &LoadOptions,
    build: BuildInfo,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<crate::WorkerConfig, Error> {
    load_worker_with_tail_from(options, build, None, environment)
}

pub(crate) fn load_worker_with_tail_from(
    options: &LoadOptions,
    build: BuildInfo,
    shutdown_tail: Option<crate::ShutdownTail>,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<crate::WorkerConfig, Error> {
    let mut input: crate::WorkerInput = merge(options, environment)?;
    input.app.apply_build_info(build);
    Ok(input.admit(shutdown_tail)?)
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
    let mut input: crate::postgres::MigrationInput = merge(options, environment)?;
    input.app.apply_build_info(build);
    input.admit()
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
    let input: crate::jobs::JobsOperatorInput = merge(options, environment)?;
    input.admit()
}
// template:end jobs:load-jobs-operator

fn merge<T: DeserializeOwned>(
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
    let variables =
        collect_variables(options.secrets_dir.as_deref(), environment).unwrap_or_else(|error| {
            errors.push(error);
            BTreeMap::new()
        });
    let mut files = Vec::new();
    for path in options.files() {
        match scan_file(path) {
            Ok(file) => files.push(file),
            Err(error) => errors.push(error),
        }
    }
    match errors.len() {
        0 => Ok((files, variables)),
        1 => Err(errors.remove(0)),
        _ => Err(Error::InputErrors(errors)),
    }
}

fn merge_inputs<T: DeserializeOwned>(
    files: Vec<ScannedFile>,
    variables: variables::Variables,
) -> Result<T, Error> {
    reject_table_overrides(&files, &variables)?;
    reject_surviving_datetimes(&files, &variables)?;
    let mut builder = config::Config::builder();
    for file in files {
        builder = builder.add_source(file);
    }
    let mut variable_sources = Vec::with_capacity(variables.len());
    for variable in variables.into_values() {
        let value = config::Value::new(Some(&variable.source.as_str().to_owned()), variable.value);
        builder = builder
            .set_override(&variable.key, value)
            .map_err(|error| Error::Merge(error.to_string()))?;
        variable_sources.push((variable.key, variable.source));
    }
    let merged = builder
        .build()
        .map_err(|error| Error::Merge(error.to_string()))?;

    let rejection = |error| Error::Deserialize(describe_rejection(&error, &variable_sources));
    if let config::ValueKind::Table(sections) = &merged.cache.kind
        && let Some(key) = sections
            .keys()
            .find(|key| !RETAINED_SECTIONS.contains(&key.as_str()))
    {
        return Err(rejection(serde::de::Error::unknown_field(
            key,
            RETAINED_SECTIONS,
        )));
    }
    merged.try_deserialize().map_err(rejection)
}

fn reject_table_overrides(
    files: &[ScannedFile],
    variables: &variables::Variables,
) -> Result<(), Error> {
    let mut table_keys = BTreeSet::new();
    let mut key_segments = Vec::new();
    for file in files {
        if let Some(key) =
            record_tables_and_find_override(&file.table, &mut table_keys, &mut key_segments)
        {
            return Err(Error::TableOverride {
                key,
                input: file.path.display().to_string(),
            });
        }
    }
    if let Some(variable) = variables
        .values()
        .find(|variable| table_keys.contains(&variable.key))
    {
        return Err(Error::TableOverride {
            key: variable.key.clone(),
            input: variable.original_name.to_ascii_uppercase(),
        });
    }
    Ok(())
}

fn reject_surviving_datetimes(
    files: &[ScannedFile],
    variables: &variables::Variables,
) -> Result<(), Error> {
    let mut datetime_keys = BTreeMap::new();
    let mut key_segments = Vec::new();
    for file in files {
        update_datetime_keys(
            &file.table,
            &file.path,
            &mut key_segments,
            &mut datetime_keys,
        );
    }
    for variable in variables.values() {
        let mut key = variable.key.as_str();
        datetime_keys.remove(key);
        while let Some((parent, _)) = key.rsplit_once('.') {
            datetime_keys.remove(parent);
            key = parent;
        }
    }
    if let Some((key, path)) = datetime_keys.into_iter().next() {
        return Err(Error::Deserialize(format!(
            "key `{key}` in {} must not be an unquoted TOML datetime; quote it as a string",
            path.display()
        )));
    }
    Ok(())
}

#[derive(Clone, derive_more::Debug)]
struct ScannedFile {
    path: PathBuf,
    #[debug(skip)]
    table: toml::Table,
}

impl config::Source for ScannedFile {
    fn clone_into_box(&self) -> Box<dyn config::Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<config::Map<String, config::Value>, config::ConfigError> {
        let uri = self.path.to_string_lossy().into_owned();
        Ok(from_toml_table(Some(&uri), &self.table))
    }
}

fn from_toml_table(
    uri: Option<&String>,
    table: &toml::Table,
) -> config::Map<String, config::Value> {
    table
        .iter()
        .map(|(key, value)| (key.clone(), from_toml_value(uri, value)))
        .collect()
}

fn from_toml_value(uri: Option<&String>, value: &toml::Value) -> config::Value {
    match value {
        toml::Value::String(value) => config::Value::new(uri, value.clone()),
        toml::Value::Integer(value) => config::Value::new(uri, *value),
        toml::Value::Float(value) => config::Value::new(uri, *value),
        toml::Value::Boolean(value) => config::Value::new(uri, *value),
        toml::Value::Datetime(value) => config::Value::new(uri, value.to_string()),
        toml::Value::Array(items) => config::Value::new(
            uri,
            items
                .iter()
                .map(|item| from_toml_value(uri, item))
                .collect::<Vec<_>>(),
        ),
        toml::Value::Table(nested) => config::Value::new(uri, from_toml_table(uri, nested)),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns this file read; startup can wait on the filesystem"
)]
fn scan_file(path: &Path) -> Result<ScannedFile, Error> {
    let text = std::fs::read_to_string(path).map_err(|error| Error::ReadFile {
        path: path.to_owned(),
        error,
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|error| {
        let message = match error.span() {
            Some(span) => format!("invalid TOML at byte {}", span.start),
            None => "invalid TOML".to_owned(),
        };
        Error::ParseFile {
            path: path.to_owned(),
            message,
        }
    })?;
    if let Some(key) = first_unaddressable_key(&table, &mut Vec::new()) {
        return Err(Error::UnaddressableKey {
            key,
            path: path.to_owned(),
        });
    }
    if let Some(key) = first_secret_key(&table) {
        return Err(Error::SecretInFile {
            key,
            path: path.to_owned(),
        });
    }
    Ok(ScannedFile {
        path: path.to_owned(),
        table,
    })
}

fn update_datetime_keys<'a>(
    table: &'a toml::Table,
    file_path: &Path,
    key_segments: &mut Vec<&'a str>,
    datetime_keys: &mut BTreeMap<String, PathBuf>,
) {
    for (key, value) in table {
        key_segments.push(key);
        let dotted = key_segments.join(".");
        if let toml::Value::Table(nested) = value {
            datetime_keys.remove(&dotted);
            update_datetime_keys(nested, file_path, key_segments, datetime_keys);
        } else if contains_datetime(value) {
            datetime_keys.insert(dotted, file_path.to_owned());
        } else {
            datetime_keys.remove(&dotted);
        }
        key_segments.pop();
    }
}

fn contains_datetime(value: &toml::Value) -> bool {
    match value {
        toml::Value::Datetime(_) => true,
        toml::Value::Array(items) => items.iter().any(contains_datetime),
        _ => false,
    }
}

fn record_tables_and_find_override<'a>(
    table: &'a toml::Table,
    table_keys: &mut BTreeSet<String>,
    key_segments: &mut Vec<&'a str>,
) -> Option<String> {
    for (segment, value) in table {
        key_segments.push(segment);
        let dotted_key = key_segments.join(".");
        let found = if let Some(nested) = value.as_table() {
            table_keys.insert(dotted_key);
            record_tables_and_find_override(nested, table_keys, key_segments)
        } else {
            table_keys.contains(&dotted_key).then_some(dotted_key)
        };
        key_segments.pop();
        if found.is_some() {
            return found;
        }
    }
    None
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
