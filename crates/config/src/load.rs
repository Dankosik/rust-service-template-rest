use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::app::BuildInfo;
use crate::secret_policy::first_secret_like_key;
use crate::{Config, ConfigInput, LoadOptions, ValidationErrors};

mod rejection;
mod variables;

use rejection::describe_rejection;
use variables::collect_variables;
pub(crate) use variables::{ADDRESSABLE_KEY_FORM, is_env_addressable, variable_name};

const ENV_PREFIX: &str = "APP";
const ENV_SEPARATOR: &str = "__";

const NAME_FORM: &str = "each segment between `__` separators must be letters, digits, `_`, or `-`";

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
    #[error("secret-like key `{key}` carries a value in config file {}; secrets come only from an `{ENV_PREFIX}{ENV_SEPARATOR}` variable", path.display())]
    SecretInFile { key: String, path: PathBuf },
    #[error("key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable; use {ADDRESSABLE_KEY_FORM} for table keys and keep them outside arrays", path.display())]
    UnaddressableKey { key: String, path: PathBuf },
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

pub fn load(options: &LoadOptions, build: BuildInfo) -> Result<Config, Error> {
    load_from(options, build, std::env::vars_os())
}

pub(crate) fn load_from<I, K, V>(
    options: &LoadOptions,
    build: BuildInfo,
    environment: I,
) -> Result<Config, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut input: ConfigInput = merge(options, environment, AuthnAudienceHandling::Apply)?;
    input.app.apply_build_info(build);
    Ok(input.admit()?)
}

// template:begin postgres:load-migration
pub fn load_migration(
    options: &LoadOptions,
    build: BuildInfo,
) -> Result<crate::MigrationConfig, Error> {
    load_migration_from(options, build, std::env::vars_os())
}

pub(crate) fn load_migration_from<I, K, V>(
    options: &LoadOptions,
    build: BuildInfo,
    environment: I,
) -> Result<crate::MigrationConfig, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut input: crate::postgres::MigrationInput =
        merge(options, environment, AuthnAudienceHandling::Skip)?;
    input.app.apply_build_info(build);
    Ok(input.admit()?)
}
// template:end postgres:load-migration

// template:begin jobs:load-jobs-operator
pub fn load_jobs_operator(options: &LoadOptions) -> Result<crate::JobsOperatorConfig, Error> {
    load_jobs_operator_from(options, std::env::vars_os())
}

pub(crate) fn load_jobs_operator_from<I, K, V>(
    options: &LoadOptions,
    environment: I,
) -> Result<crate::JobsOperatorConfig, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let input: crate::jobs::JobsOperatorInput =
        merge(options, environment, AuthnAudienceHandling::Skip)?;
    Ok(input.admit()?)
}
// template:end jobs:load-jobs-operator

#[derive(Clone, Copy)]
enum AuthnAudienceHandling {
    Apply,
    #[allow(
        dead_code,
        reason = "only the optional migration and jobs operator loaders skip audience handling"
    )]
    Skip,
}

fn merge<T, I, K, V>(
    options: &LoadOptions,
    environment: I,
    #[allow(
        unused_variables,
        reason = "audience handling is only present with the authn profile"
    )]
    authn_audience_handling: AuthnAudienceHandling,
) -> Result<T, Error>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let variables = collect_variables(options.secrets_dir.as_deref(), environment)?;
    let mut builder = config::Config::builder();
    let mut table_keys = BTreeSet::new();
    // template:begin authn:load-audience-file-kind
    let mut file_audience_datetime_path: Option<&Path> = None;
    // template:end authn:load-audience-file-kind
    for path in options.files() {
        #[allow(
            unused_variables,
            reason = "only the authn profile checks audience types"
        )]
        let (file, table) = scan_file(path, &mut table_keys)?;
        // template:begin authn:load-audience-file-scan
        if let Some(audience) = table
            .get("authn")
            .and_then(|section| section.get("audience"))
        {
            let has_datetime = audience.is_datetime()
                || audience
                    .as_array()
                    .is_some_and(|items| items.iter().any(toml::Value::is_datetime));
            file_audience_datetime_path = has_datetime.then_some(path);
        }
        // template:end authn:load-audience-file-scan
        builder = builder.add_source(file);
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
    // template:begin authn:load-audience-list-decode
    let audience_list = match authn_audience_handling {
        AuthnAudienceHandling::Apply => {
            if let Some(path) = file_audience_datetime_path
                && !variables
                    .values()
                    .any(|variable| variable.key == variables::AUDIENCE_KEY)
            {
                return Err(Error::Deserialize(format!(
                    "{} in {}",
                    crate::authn::AUDIENCE_INPUT_FORM,
                    path.display(),
                )));
            }
            variables::audience_list(&variables)?
        }
        AuthnAudienceHandling::Skip => None,
    };
    // template:end authn:load-audience-list-decode
    let environment_origin = String::from("the environment");
    let mut variable_keys = Vec::with_capacity(variables.len());
    for variable in variables.into_values() {
        let value = config::Value::new(Some(&environment_origin), variable.value);
        builder = builder
            .set_override(&variable.key, value)
            .map_err(|error| Error::Merge(error.to_string()))?;
        variable_keys.push(variable.key);
    }
    // template:begin authn:load-audience-list-override
    let builder = builder
        .set_override_option(variables::AUDIENCE_KEY, audience_list)
        .map_err(|error| Error::Merge(error.to_string()))?;
    // template:end authn:load-audience-list-override
    let merged = builder
        .build()
        .map_err(|error| Error::Merge(error.to_string()))?;

    let carrier = if options.secrets_dir.is_some() {
        "the environment or the secrets directory"
    } else {
        &environment_origin
    };
    merged
        .try_deserialize()
        .map_err(|error| Error::Deserialize(describe_rejection(&error, &variable_keys, carrier)))
}

#[derive(Clone, derive_more::Debug)]
struct ScannedFile {
    path: PathBuf,
    #[debug(skip)]
    text: String,
}

impl config::Source for ScannedFile {
    fn clone_into_box(&self) -> Box<dyn config::Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<config::Map<String, config::Value>, config::ConfigError> {
        let uri = self.path.to_string_lossy().into_owned();
        config::Format::parse(&config::FileFormat::Toml, Some(&uri), &self.text).map_err(|cause| {
            config::ConfigError::FileParse {
                uri: Some(uri),
                cause,
            }
        })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns this file read; startup can wait on the filesystem"
)]
fn scan_file(
    path: &Path,
    table_keys: &mut BTreeSet<String>,
) -> Result<(ScannedFile, toml::Table), Error> {
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
    if let Some(key) = first_secret_like_key(&table) {
        return Err(Error::SecretInFile {
            key,
            path: path.to_owned(),
        });
    }
    if let Some(key) = first_table_override(&table, table_keys, &mut Vec::new()) {
        return Err(Error::TableOverride {
            key,
            input: path.display().to_string(),
        });
    }
    Ok((
        ScannedFile {
            path: path.to_owned(),
            text,
        },
        table,
    ))
}

fn first_table_override<'a>(
    table: &'a toml::Table,
    table_keys: &mut BTreeSet<String>,
    path: &mut Vec<&'a str>,
) -> Option<String> {
    for (key, value) in table {
        path.push(key);
        let key = path.join(".");
        let found = if let Some(nested) = value.as_table() {
            table_keys.insert(key);
            first_table_override(nested, table_keys, path)
        } else {
            table_keys.contains(&key).then_some(key)
        };
        path.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn first_unaddressable_key<'a>(table: &'a toml::Table, path: &mut Vec<&'a str>) -> Option<String> {
    for (key, value) in table {
        path.push(key);
        let found = if is_env_addressable(key) {
            match value {
                toml::Value::Table(nested) => first_unaddressable_key(nested, path),
                toml::Value::Array(_) => {
                    first_key_in_array(value).map(|suffix| format!("{}{suffix}", path.join(".")))
                }
                _ => None,
            }
        } else {
            Some(path.join("."))
        };
        path.pop();
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
