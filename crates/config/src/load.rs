//! Build the snapshot from defaults, files, and the `APP__` variables: the
//! secrets directory's files, then the process environment over them.

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
    /// Two variable names that address the same key once lowercased.
    #[error(
        "{name} and {other} set the same key: a variable name is lowercased before it is split"
    )]
    AmbiguousName { name: String, other: String },
    /// A config file could not be read.
    #[error("config file {}: {error}", path.display())]
    ReadFile {
        path: PathBuf,
        error: std::io::Error,
    },
    /// A config file is not valid TOML.
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
    #[error("key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable, whose name is lowercased and split on `{ENV_SEPARATOR}`; use {ADDRESSABLE_KEY_FORM}", path.display())]
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
/// Fails on a malformed or ambiguous `APP__` variable name, a variable value
/// that is not Unicode, an unreadable secrets directory, an unreadable or
/// unparsable file, a file key no variable can address, a non-empty
/// secret-like value in a config file, an unknown key anywhere, or a
/// violated validation rule.
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
    let mut snapshot: Config = merge(options, environment)?;
    snapshot.app.apply_build_info(build);
    snapshot.validate()?;
    Ok(snapshot)
}

// template:begin postgres:load-migration
/// Load only the sections the migration binary reads, so a migration run
/// needs no other section's secrets and is not stopped by their rules.
///
/// # Errors
///
/// Fails as [`load`] does, for the sections [`crate::MigrationConfig`] holds.
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
    let mut snapshot: crate::MigrationConfig = merge(options, environment)?;
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
    let snapshot: crate::JobsOperatorConfig = merge(options, environment)?;
    snapshot.validate()?;
    Ok(snapshot)
}
// template:end jobs:load-jobs-operator

/// Merge the files and the `APP__` variables, then decode the result.
fn merge<T, I, K, V>(options: &LoadOptions, environment: I) -> Result<T, Error>
where
    T: DeserializeOwned,
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let variables = collect_variables(options.secrets_dir.as_deref(), environment)?;
    // `Environment::source` keeps keys that still carry the `APP__` prefix;
    // inserting the stripped key would make every override disappear.
    let (source, variable_keys): (config::Map<String, String>, Vec<String>) = variables
        .into_iter()
        .map(|(name, variable)| ((name, variable.value), variable.key))
        .unzip();

    let mut builder = config::Config::builder();
    for path in options.files() {
        scan_file(path)?;
        builder = builder.add_source(
            // config-rs reads the path itself so its errors name the file; the
            // pre-scan read above serves only the file rules.
            config::File::from(path.to_path_buf())
                .format(config::FileFormat::Toml)
                .required(true),
        );
    }
    let merged = builder
        .add_source(
            config::Environment::with_prefix(ENV_PREFIX)
                .prefix_separator(ENV_SEPARATOR)
                .separator(ENV_SEPARATOR)
                // An empty value is still an explicit override; validation decides.
                .ignore_empty(false)
                .source(Some(source)),
        )
        .build()
        .map_err(Error::Merge)?;

    let carrier = if options.secrets_dir.is_some() {
        "the environment or the secrets directory"
    } else {
        "the environment"
    };
    merged
        .try_deserialize()
        .map_err(|error| Error::Deserialize(describe_rejection(&error, &variable_keys, carrier)))
}

/// The rules a config file must meet before it is merged: every key is one
/// a variable can also address, and no secret-like key carries a value.
#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns this file read; startup can wait on the filesystem"
)]
fn scan_file(path: &Path) -> Result<(), Error> {
    let text = std::fs::read_to_string(path).map_err(|error| Error::ReadFile {
        path: path.to_owned(),
        error,
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|error| Error::ParseFile {
        path: path.to_owned(),
        error,
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
    Ok(())
}

/// The first key, as a dotted path, that no `APP__` variable can name. A
/// file entry `Partner` and a variable segment `__PARTNER__` would be two
/// entries, and the file's one could never receive an environment-only
/// secret or an override.
fn first_unaddressable_key<'a>(table: &'a toml::Table, path: &mut Vec<&'a str>) -> Option<String> {
    for (key, value) in table {
        path.push(key);
        let found = if is_env_addressable(key) {
            value
                .as_table()
                .and_then(|nested| first_unaddressable_key(nested, path))
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

#[cfg(test)]
mod tests;
