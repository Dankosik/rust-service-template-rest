//! Build the snapshot from defaults, files, and the `APP__` variables: the
//! secrets directory's files, then the process environment over them.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::app::BuildInfo;
use crate::secret_policy::first_secret_like_key;
use crate::{Config, LoadOptions, ValidationError};

mod rejection;
mod variables;

use rejection::describe_rejection;
use variables::collect_variables;
pub(crate) use variables::{ADDRESSABLE_KEY_FORM, is_env_addressable, variable_name};

/// Variable namespace. `APP__HTTP__ADDR` sets `http.addr`.
pub const ENV_PREFIX: &str = "APP";
const ENV_SEPARATOR: &str = "__";

/// What a variable name must be, for the two errors that refuse one.
const NAME_FORM: &str = "each segment between `__` separators must be letters, digits, `_`, or `-`";

/// Why a snapshot could not be built. `Display` carries the whole cause, so
/// no variant also exposes it through `source()`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An `APP__` variable name that does not meet the name form.
    #[error("environment variable {name} is malformed: {NAME_FORM}")]
    MalformedEnvName { name: String },
    /// An `APP__` variable whose value is not valid Unicode.
    #[error("environment variable {name} holds a value that is not valid Unicode")]
    NonUnicodeEnvValue {
        /// The variable name; the value is never reported.
        name: String,
    },
    /// The secrets directory could not be read.
    #[error("secrets directory {}: {error}", path.display())]
    ReadSecretsDir {
        path: PathBuf,
        error: std::io::Error,
    },
    /// A file in the secrets directory could not be read.
    #[error("secret file {}: {error}", path.display())]
    ReadSecret {
        path: PathBuf,
        error: std::io::Error,
    },
    /// A secrets-directory file whose name is not a valid variable name.
    #[error("secret file {} is malformed: its name is a variable name, and {NAME_FORM}", path.display())]
    MalformedSecretName { path: PathBuf },
    /// Two variable names that address the same key within one carrier.
    #[error("{name} and {other} set the same key")]
    AmbiguousName { name: String, other: String },
    /// One variable sets a scalar at a path another variable uses as a table.
    #[error("{parent} and {child} set overlapping keys: one key is inside the other")]
    OverlappingVariables { parent: String, child: String },
    /// A later source would erase the keys in an earlier table.
    #[error(
        "{input} cannot replace configuration table `{key}` with a value. Override its keys individually"
    )]
    TableOverride { key: String, input: String },
    /// A config file could not be read.
    #[error("config file {}: {error}", path.display())]
    ReadFile {
        path: PathBuf,
        error: std::io::Error,
    },
    /// A config file is not valid TOML. The diagnostic carries no input text.
    #[error("config file {}: {error}", path.display())]
    ParseFile {
        path: PathBuf,
        error: toml::de::Error,
    },
    /// A config file gives a secret-like key a non-empty value.
    #[error("secret-like key `{key}` carries a value in config file {}; secrets come only from an `{ENV_PREFIX}{ENV_SEPARATOR}` variable", path.display())]
    SecretInFile {
        key: String,
        /// The config file that carries the value.
        path: PathBuf,
    },
    /// A config file key that no `APP__` variable can address.
    #[error("key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable; use {ADDRESSABLE_KEY_FORM} for table keys and keep them outside arrays", path.display())]
    UnaddressableKey {
        key: String,
        /// The config file that holds the key.
        path: PathBuf,
    },
    /// Merging the configuration sources failed.
    #[error("load configuration: {0}")]
    Merge(config::ConfigError),
    /// The message never carries a value a variable supplied.
    #[error("configuration is invalid: {0}")]
    Deserialize(String),
    /// A validation rule failed.
    #[error("configuration is invalid: {0}")]
    Validate(ValidationError),
}

impl From<ValidationError> for Error {
    fn from(error: ValidationError) -> Self {
        Self::Validate(error)
    }
}

/// Load, merge, and validate the snapshot.
///
/// # Errors
///
/// Fails on malformed, ambiguous, or overlapping `APP__` names, a variable value
/// that is not Unicode, an unreadable secrets directory, an unreadable or
/// unparsable file, a source replacing a table with a value, a file key no
/// variable can address, a non-empty secret-like value in a config file, an
/// unknown key anywhere, or a violated validation rule.
pub fn load(options: &LoadOptions, build: BuildInfo) -> Result<Config, Error> {
    load_from(options, build, std::env::vars_os())
}

/// [`load`] over an explicit environment, for tests.
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
    let mut snapshot: Config = merge(options, environment, AuthnAudienceOverride::Decode)?;
    snapshot.app.apply_build_info(build);
    snapshot.validate()?;
    Ok(snapshot)
}

// template:begin postgres:load-migration
/// Load the sections the migration binary reads.
///
/// # Errors
///
/// Enforces the same variable, file, and secret rules as [`load`]. Only
/// the sections in [`crate::MigrationConfig`] are decoded and validated.
pub fn load_migration(
    options: &LoadOptions,
    build: BuildInfo,
) -> Result<crate::MigrationConfig, Error> {
    load_migration_from(options, build, std::env::vars_os())
}

/// [`load_migration`] over an explicit environment, for tests.
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
    let mut snapshot: crate::MigrationConfig =
        merge(options, environment, AuthnAudienceOverride::Skip)?;
    snapshot.app.apply_build_info(build);
    snapshot.validate()?;
    Ok(snapshot)
}
// template:end postgres:load-migration

// template:begin jobs:load-jobs-operator
/// Load only PostgreSQL configuration for a jobs operator command.
///
/// # Errors
///
/// Enforces the same namespace, file and secret rules as [`load`], but only
/// decodes and validates the PostgreSQL section. Other sections are ignored.
pub fn load_jobs_operator(options: &LoadOptions) -> Result<crate::JobsOperatorConfig, Error> {
    load_jobs_operator_from(options, std::env::vars_os())
}

/// [`load_jobs_operator`] over an explicit environment, for tests.
pub(crate) fn load_jobs_operator_from<I, K, V>(
    options: &LoadOptions,
    environment: I,
) -> Result<crate::JobsOperatorConfig, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let snapshot: crate::JobsOperatorConfig =
        merge(options, environment, AuthnAudienceOverride::Skip)?;
    snapshot.validate()?;
    Ok(snapshot)
}
// template:end jobs:load-jobs-operator

#[derive(Clone, Copy)]
enum AuthnAudienceOverride {
    Decode,
    #[allow(
        dead_code,
        reason = "only the optional migration and jobs operator loaders skip the override"
    )]
    Skip,
}

/// Merge the files and the `APP__` variables, then decode the result.
fn merge<T, I, K, V>(
    options: &LoadOptions,
    environment: I,
    #[allow(
        unused_variables,
        reason = "only the authn profile needs list decoding"
    )]
    authn_audience_override: AuthnAudienceOverride,
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
    for path in options.files() {
        builder = builder.add_source(scan_file(path, &mut table_keys)?);
    }
    if let Some(variable) = variables
        .values()
        .find(|variable| table_keys.contains(&variable.key))
    {
        return Err(Error::TableOverride {
            key: variable.key.clone(),
            input: variable.spelled.to_ascii_uppercase(),
        });
    }
    // template:begin authn:load-audience-list-decode
    let audience_list = match authn_audience_override {
        AuthnAudienceOverride::Decode => variables::audience_list(&variables)?,
        AuthnAudienceOverride::Skip => None,
    };
    // template:end authn:load-audience-list-decode
    // `Environment::source` keeps keys that still carry the `APP__` prefix;
    // inserting the stripped key would make every override disappear.
    let (source, variable_keys): (config::Map<String, String>, Vec<String>) = variables
        .into_iter()
        .map(|(name, variable)| ((name, variable.value), variable.key))
        .unzip();
    let builder = builder.add_source(
        config::Environment::with_prefix(ENV_PREFIX)
                .prefix_separator(ENV_SEPARATOR)
                .separator(ENV_SEPARATOR)
                // An empty value is still an explicit override; validation decides.
                .ignore_empty(false)
                .source(Some(source)),
    );
    // template:begin authn:load-audience-list-override
    let builder = builder
        .set_override_option("authn.audience", audience_list)
        .map_err(Error::Merge)?;
    // template:end authn:load-audience-list-override
    let merged = builder.build().map_err(Error::Merge)?;

    let carrier = if options.secrets_dir.is_some() {
        "the environment or the secrets directory"
    } else {
        "the environment"
    };
    merged
        .try_deserialize()
        .map_err(|error| Error::Deserialize(describe_rejection(&error, &variable_keys, carrier)))
}

#[derive(Clone)]
struct ScannedFile {
    path: PathBuf,
    text: String,
}

impl std::fmt::Debug for ScannedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScannedFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl config::Source for ScannedFile {
    fn clone_into_box(&self) -> Box<dyn config::Source + Send + Sync> {
        Box::new(self.clone())
    }

    fn collect(&self) -> Result<config::Map<String, config::Value>, config::ConfigError> {
        let uri = self.path.to_string_lossy().into_owned();
        let text = self.text.strip_prefix('\u{feff}').unwrap_or(&self.text);
        config::Format::parse(&config::FileFormat::Toml, Some(&uri), text).map_err(|cause| {
            config::ConfigError::FileParse {
                uri: Some(uri),
                cause,
            }
        })
    }
}

/// Retain the checked bytes so a file replacement cannot change what is merged.
#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns this file read; startup can wait on the filesystem"
)]
fn scan_file(path: &Path, table_keys: &mut BTreeSet<String>) -> Result<ScannedFile, Error> {
    let text = std::fs::read_to_string(path).map_err(|error| Error::ReadFile {
        path: path.to_owned(),
        error,
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|error| {
        // Parsing fails before secret admission, so keep only the location.
        let message = match error.span() {
            Some(span) => format!("invalid TOML at byte {}", span.start),
            None => "invalid TOML".to_owned(),
        };
        Error::ParseFile {
            path: path.to_owned(),
            error: serde::de::Error::custom(message),
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
    Ok(ScannedFile {
        path: path.to_owned(),
        text,
    })
}

/// A scalar replacing a table can erase an unknown key before decoding sees it.
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

/// The first key, as a dotted path, that no `APP__` variable can name. A
/// file entry `Partner` and a variable segment `__PARTNER__` would be two
/// entries, and the file's one could never receive an environment-only
/// secret or an override.
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

/// An array has no index syntax in `APP__` names, and a later source can
/// replace it wholesale before typed decoding sees any keys inside it.
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
