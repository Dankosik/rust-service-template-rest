//! Build the snapshot from defaults, files, and the `APP__` variables: the
//! secrets directory's files, then the process environment over them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::app::BuildInfo;
use crate::de::VALUE_FREE;
use crate::secret_policy::first_secret_like_key;
use crate::{Config, LoadOptions, ValidationError};

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
    #[error("key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable, whose name is lowercased and split on `{ENV_SEPARATOR}`; use lowercase letters, digits, `_`, and `-`, without `{ENV_SEPARATOR}` or a trailing `_`", path.display())]
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
    let mut variable_keys = Vec::with_capacity(variables.len());
    // `Environment::source` keeps keys that still carry the `APP__` prefix;
    // inserting the stripped key would make every override disappear.
    let source: config::Map<String, String> = variables
        .into_iter()
        .map(|(name, variable)| {
            variable_keys.push(variable.key);
            (name, variable.value)
        })
        .collect();

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

/// Sections whose values stay out of a decode failure whichever source set
/// them; their `Debug` output redacts the same trust inputs.
const VALUE_FREE_SECTIONS: &[&str] = &[
    // template:begin client-integrations:load-value-free-section
    "integrations",
    // template:end client-integrations:load-value-free-section
];

/// Render a decode failure without a value a variable supplied.
///
/// Variables are the only secret source, and config-rs and serde quote the
/// value they refuse, sometimes lowercased, trimmed, or parsed. So a failure
/// at a key a variable sets is rebuilt from its key and its expected form,
/// never from the decoder's text, and so is one inside a section of
/// [`VALUE_FREE_SECTIONS`]. Only a message that names keys alone (an unknown
/// or missing field) or carries [`VALUE_FREE`] is shown as written, as is a
/// failure at any other key only a file sets. `variable_keys` are the dotted
/// keys the variables of this load set, and `carrier` names where those
/// variables come from.
fn describe_rejection(
    error: &config::ConfigError,
    variable_keys: &[String],
    carrier: &str,
) -> String {
    let (key, rejection) = rejection(error);
    // `urls[0]` is an element of the value at `urls`.
    let value_key = key.map(|key| key.split_once('[').map_or(key, |(head, _)| head));
    // A failure that names no key may come from any source.
    let variable_may_set = value_key.is_none_or(|key| {
        variable_keys
            .iter()
            .any(|variable_key| is_at_or_under(variable_key, key))
    });
    let value_free = value_key.is_some_and(|key| {
        VALUE_FREE_SECTIONS
            .iter()
            .any(|section| is_at_or_under(key, section))
    });
    let hide_value = variable_may_set || value_free;
    let place = key.map_or_else(String::new, |key| format!(" for key `{key}`"));
    let origin = if variable_may_set {
        format!(" in {carrier}")
    } else {
        String::new()
    };
    match rejection {
        Rejection::Expected(expected) if hide_value => {
            format!("invalid value, expected {expected}{place}{origin}")
        }
        Rejection::Unexplained if hide_value => format!("invalid value{place}{origin}"),
        Rejection::AsWritten | Rejection::Expected(_) | Rejection::Unexplained => {
            error.to_string().replace(VALUE_FREE, "")
        }
    }
}

/// What a decode failure may say about the value it refused.
enum Rejection<'a> {
    /// The message holds no value.
    AsWritten,
    /// The message may quote the value; this is the form it expected.
    Expected(&'a str),
    /// The message may quote the value and names no expected form.
    Unexplained,
}

/// The key a decode failure names, and what its message may be trusted for.
fn rejection(error: &config::ConfigError) -> (Option<&str>, Rejection<'_>) {
    const KEYS_ONLY: [&str; 3] = ["unknown field ", "missing field ", "duplicate field "];
    const QUOTES_VALUE: [&str; 3] = ["invalid type: ", "invalid value: ", "unknown variant "];
    match error {
        config::ConfigError::At { error, key, .. } => {
            let (inner_key, rejection) = rejection(error);
            (key.as_deref().or(inner_key), rejection)
        }
        config::ConfigError::Type { expected, key, .. } => {
            (key.as_deref(), Rejection::Expected(expected))
        }
        config::ConfigError::NotFound(_) => (None, Rejection::AsWritten),
        config::ConfigError::Message(message)
            if message.starts_with(VALUE_FREE)
                || KEYS_ONLY.iter().any(|form| message.starts_with(form)) =>
        {
            (None, Rejection::AsWritten)
        }
        // serde ends these forms with its own `, expected <form>`; a value
        // quoted earlier in the message cannot follow the last one.
        config::ConfigError::Message(message)
            if QUOTES_VALUE.iter().any(|form| message.starts_with(form)) =>
        {
            let expected = message.rsplit_once(", expected ");
            (
                None,
                expected.map_or(Rejection::Unexplained, |(_, expected)| {
                    Rejection::Expected(expected)
                }),
            )
        }
        _ => (None, Rejection::Unexplained),
    }
}

/// Whether the dotted `path` is `key` or lies beneath it.
fn is_at_or_under(path: &str, key: &str) -> bool {
    path.strip_prefix(key)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

fn variable_prefix() -> String {
    format!("{ENV_PREFIX}{ENV_SEPARATOR}")
}

/// A byte a key segment may hold once a variable name is lowercased: the
/// config-rs path identifier alphabet.
fn is_name_byte(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
}

/// Whether the part of a variable name after `APP__` is a key path: every
/// segment a plain identifier. config-rs would report an empty segment as an
/// unknown field with an empty name, and would read `NAME[0]` or `A.B` as a
/// path of its own, building a key no rule here expects a variable to set.
fn is_key_path(suffix: &str) -> bool {
    suffix.split(ENV_SEPARATOR).all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| is_name_byte(byte.to_ascii_lowercase()))
    })
}

/// Whether an `APP__` variable can address the key `name`. The loader
/// lowercases a variable name and splits it on `__`, so a key outside that
/// form never meets its environment counterpart: the file and the
/// environment would name two entries.
pub(crate) fn is_env_addressable(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(is_name_byte)
        && !name.contains(ENV_SEPARATOR)
        && !name.ends_with('_')
}

/// One variable of a carrier, kept under the lowercased name config-rs reads.
struct Variable {
    spelled: String,
    /// The dotted key the variable sets, such as `http.addr`.
    key: String,
    value: String,
}

type Variables = std::collections::BTreeMap<String, Variable>;

/// Record a variable whose name is `APP__` and `suffix`, refusing a second
/// spelling of its name: which of the two values won would depend on the
/// order the carrier listed them in.
fn admit(variables: &mut Variables, name: &str, suffix: &str, value: String) -> Result<(), Error> {
    let variable = Variable {
        spelled: name.to_owned(),
        key: suffix.to_lowercase().replace(ENV_SEPARATOR, "."),
        value,
    };
    match variables.insert(name.to_lowercase(), variable) {
        Some(other) => Err(Error::AmbiguousName {
            name: name.to_owned(),
            other: other.spelled,
        }),
        None => Ok(()),
    }
}

/// The `APP__` variables of one load under their lowercased names: the
/// secrets directory's files, then the process environment, whose value wins
/// for a name both carry.
fn collect_variables<I, K, V>(
    secrets_dir: Option<&Path>,
    environment: I,
) -> Result<Variables, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut variables = match secrets_dir {
        Some(dir) => read_secrets_dir(dir)?,
        None => Variables::new(),
    };
    variables.extend(read_environment(environment)?);
    Ok(variables)
}

/// The `APP__*` variables of the process environment. A name that is not
/// Unicode fails the key-path check as well: its replacement character is no
/// identifier byte.
fn read_environment<I, K, V>(environment: I) -> Result<Variables, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let prefix = variable_prefix();
    let mut variables = Variables::new();
    for (key, value) in environment {
        let key = key.into();
        let name = key.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        if !is_key_path(suffix) {
            return Err(Error::MalformedEnvName {
                name: name.into_owned(),
            });
        }
        // A lossy conversion would hand a credential on with its bytes changed.
        let Ok(value) = value.into().into_string() else {
            return Err(Error::NonUnicodeEnvValue {
                name: name.into_owned(),
            });
        };
        admit(&mut variables, &name, suffix, value)?;
    }
    Ok(variables)
}

/// The variables a secrets directory holds: each entry named `APP__...` is
/// one variable and its content the value. Every other entry is skipped,
/// which leaves out the `..data` links a Kubernetes volume keeps beside its
/// files. A value ends before its trailing line breaks, since most tools
/// write one; every other byte is kept, and a NUL byte is refused.
#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns finite secret-directory reads; startup can wait on the filesystem"
)]
fn read_secrets_dir(dir: &Path) -> Result<Variables, Error> {
    let unreadable = |error| Error::ReadSecretsDir {
        path: dir.to_owned(),
        error,
    };
    let prefix = variable_prefix();
    let mut variables = Variables::new();
    for entry in std::fs::read_dir(dir).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let file = entry.path();
        if !is_key_path(suffix) {
            return Err(Error::MalformedSecretName { path: file });
        }
        let value = match std::fs::read_to_string(&file) {
            // The environment cannot carry NUL, and `VALUE_FREE` relies on it.
            Ok(value) if value.contains('\0') => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the value contains a NUL byte, which no variable can hold",
            )),
            read => read,
        }
        .map_err(|error| Error::ReadSecret { path: file, error })?;
        admit(
            &mut variables,
            &name,
            suffix,
            value.trim_end_matches(['\r', '\n']).to_owned(),
        )?;
    }
    Ok(variables)
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
