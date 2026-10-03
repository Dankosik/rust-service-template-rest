//! Build the snapshot from defaults, files, and the `APP__` variables: the
//! secrets directory's files, then the process environment over them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::app::BuildInfo;
use crate::de::VALUE_FREE;
use crate::secret_policy::first_secret_like_key;
use crate::validate::is_env_addressable;
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
    #[error(
        "{name} and {other} set the same key: a variable name is lowercased before it is split"
    )]
    AmbiguousName { name: String, other: String },
    #[error("config file {}: {error}", path.display())]
    ReadFile {
        path: PathBuf,
        error: std::io::Error,
    },
    #[error("config file {}: {error}", path.display())]
    ParseFile {
        path: PathBuf,
        error: toml::de::Error,
    },
    #[error("secret-like key `{key}` carries a value in config file {}; secrets come only from an `{ENV_PREFIX}{ENV_SEPARATOR}` variable", path.display())]
    SecretInFile { key: String, path: PathBuf },
    #[error("key `{key}` in config file {} cannot be set by an `{ENV_PREFIX}{ENV_SEPARATOR}` variable, whose name is lowercased and split on `{ENV_SEPARATOR}`; use lowercase letters, digits, `_`, and `-`, without `{ENV_SEPARATOR}` or a trailing `_`", path.display())]
    UnaddressableKey { key: String, path: PathBuf },
    #[error("load configuration: {0}")]
    Merge(config::ConfigError),
    /// The message never carries a value a variable supplied.
    #[error("configuration is invalid: {0}")]
    Deserialize(String),
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
    let namespace = collect_namespace(options.secrets_dir.as_deref(), environment)?;

    let mut builder = config::Config::builder();
    for path in options.files() {
        scan_file(path)?;
        builder = builder.add_source(
            // config-rs reads the path itself so its errors name the file; the
            // pre-scan read above serves only the file rules.
            config::File::from(path.clone())
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
                .source(Some(namespace.clone())),
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
        .map_err(|error| Error::Deserialize(describe_rejection(&error, &namespace, carrier)))
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
/// failure at any other key only a file sets. `carrier` names where the
/// variables of this load come from.
fn describe_rejection(
    error: &config::ConfigError,
    namespace: &config::Map<String, String>,
    carrier: &str,
) -> String {
    let (key, rejection) = rejection(error);
    // `urls[0]` is an element of the value at `urls`.
    let value_key = key.map(|key| key.split('[').next().unwrap_or(key));
    let from_variable = value_key.is_none_or(|key| {
        namespace
            .keys()
            .any(|name| is_at_or_under(&variable_path(name), key))
    });
    let value_free = value_key.is_some_and(|key| {
        VALUE_FREE_SECTIONS
            .iter()
            .any(|section| is_at_or_under(key, section))
    });
    let place = key.map_or_else(String::new, |key| format!(" for key `{key}`"));
    let origin = if from_variable {
        format!(" in {carrier}")
    } else {
        String::new()
    };
    match rejection {
        Rejection::Expected(expected) if from_variable || value_free => {
            format!("invalid value, expected {expected}{place}{origin}")
        }
        Rejection::Unexplained if from_variable || value_free => {
            format!("invalid value{place}{origin}")
        }
        _ => error.to_string().replace(VALUE_FREE, ""),
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

/// The dotted key a lowercased variable name sets.
fn variable_path(name: &str) -> String {
    let prefix = namespace_prefix().to_lowercase();
    name.strip_prefix(&prefix)
        .unwrap_or(name)
        .replace(ENV_SEPARATOR, ".")
}

fn namespace_prefix() -> String {
    format!("{ENV_PREFIX}{ENV_SEPARATOR}")
}

/// Whether the part of a variable name after `APP__` is a key path: every
/// segment a plain identifier. config-rs would report an empty segment as an
/// unknown field with an empty name, and would read `NAME[0]` or `A.B` as a
/// path of its own, building a key no rule here expects a variable to set.
fn is_key_path(path: &str) -> bool {
    path.split(ENV_SEPARATOR).all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

/// One carrier's variables: the lowercased name config-rs reads, then the
/// name as spelled and its value.
type Variables = std::collections::BTreeMap<String, (String, String)>;

/// Record a variable, refusing a second spelling of its name: which of the
/// two values won would depend on the order the carrier listed them in.
fn admit(variables: &mut Variables, name: &str, value: String) -> Result<(), Error> {
    match variables.insert(name.to_lowercase(), (name.to_owned(), value)) {
        Some((other, _)) => Err(Error::AmbiguousName {
            name: name.to_owned(),
            other,
        }),
        None => Ok(()),
    }
}

/// The `APP__` variables of one load under their lowercased names: the
/// secrets directory's files, then the process environment, whose value wins
/// for a name both carry.
fn collect_namespace<I, K, V>(
    secrets_dir: Option<&Path>,
    environment: I,
) -> Result<config::Map<String, String>, Error>
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
    // `Environment::source` keeps keys that still carry the `APP__` prefix;
    // inserting the stripped path would make every override disappear.
    Ok(variables
        .into_iter()
        .map(|(name, (_, value))| (name, value))
        .collect())
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
    let prefix = namespace_prefix();
    let mut variables = Variables::new();
    for (key, value) in environment {
        let key = key.into();
        let name = key.to_string_lossy();
        let Some(path) = name.strip_prefix(&prefix) else {
            continue;
        };
        if !is_key_path(path) {
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
        admit(&mut variables, &name, value)?;
    }
    Ok(variables)
}

/// The variables a secrets directory holds: each entry named `APP__...` is
/// one variable and its content the value. Every other entry is skipped,
/// which leaves out the `..data` links a Kubernetes volume keeps beside its
/// files. A value ends before its trailing line breaks, since most tools
/// write one; every other byte is kept, and a NUL byte is refused.
fn read_secrets_dir(dir: &Path) -> Result<Variables, Error> {
    let unreadable = |error| Error::ReadSecretsDir {
        path: dir.to_owned(),
        error,
    };
    let prefix = namespace_prefix();
    let mut variables = Variables::new();
    for entry in std::fs::read_dir(dir).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let Some(path) = name.strip_prefix(&prefix) else {
            continue;
        };
        let file = entry.path();
        if !is_key_path(path) {
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
            value.trim_end_matches(['\r', '\n']).to_owned(),
        )?;
    }
    Ok(variables)
}

/// The rules a config file must meet before it is merged: every key is one
/// a variable can also address, and no secret-like key carries a value.
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
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::LogFormat;
    // template:begin authn:load-authn-config-import
    use crate::AuthnConfig;
    // template:end authn:load-authn-config-import
    // template:begin oidc-jwt:load-token-profile-import
    use crate::TokenProfile;
    // template:end oidc-jwt:load-token-profile-import

    const BUILD: BuildInfo = BuildInfo {
        version: "1.2.3",
        commit: "abc123",
    };

    fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn defaults_alone_produce_a_valid_snapshot() {
        let cfg = load_from(&LoadOptions::default(), BUILD, env(&[])).unwrap();
        assert_eq!(cfg.http.addr, "0.0.0.0:8080".parse().unwrap());
        assert_eq!(cfg.app.version, "1.2.3");
        assert_eq!(cfg.app.commit, "abc123");
        assert_eq!(cfg.log.format, LogFormat::Json);
        assert_eq!(cfg.app.instance_id, None);
        assert_eq!(cfg.runtime.worker_threads, None);
    }

    #[test]
    fn runtime_workers_load_from_the_environment_and_zero_names_its_key() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__RUNTIME__WORKER_THREADS", "4")]),
        )
        .unwrap();
        assert_eq!(
            cfg.runtime.worker_threads.map(std::num::NonZeroUsize::get),
            Some(4)
        );
        assert_eq!(cfg.runtime.effective_worker_threads(), 4);

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__RUNTIME__WORKER_THREADS", "0")]),
        )
        .unwrap_err();
        assert!(matches!(&err, Error::Deserialize(_)), "{err}");
        assert!(err.to_string().contains("runtime.worker_threads"), "{err}");
    }

    #[test]
    fn shipped_local_configuration_loads_without_environment_overrides() {
        let cfg = load_from(
            &LoadOptions {
                config: Some(
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../env/config/local.toml"),
                ),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap();
        assert_eq!(cfg.log.format, LogFormat::Text);
    }

    #[test]
    fn empty_instance_id_is_vacant_none() {
        let empty = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__APP__INSTANCE_ID", "")]),
        )
        .unwrap();
        assert_eq!(empty.app.instance_id, None);
        let set = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__APP__INSTANCE_ID", "  pod-a  ")]),
        )
        .unwrap();
        assert_eq!(set.app.instance_id.as_deref(), Some("pod-a"));
    }

    #[test]
    fn empty_dsn_and_otlp_headers_are_vacant_none() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                // template:begin postgres:load-empty-dsn-env
                ("APP__POSTGRES__DSN", ""),
                ("APP__POSTGRES__PASSWORD_FILE", "  "),
                // template:end postgres:load-empty-dsn-env
                ("APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS", "  "),
                ("APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT", "  "),
            ]),
        )
        .unwrap();
        // template:begin postgres:load-empty-dsn-assertion
        assert!(!cfg.postgres.has_dsn());
        assert_eq!(cfg.postgres.password_file, None);
        // template:end postgres:load-empty-dsn-assertion
        assert!(!cfg.observability.otel.exporter.has_headers());
        assert_eq!(cfg.observability.otel.exporter.otlp_endpoint, None);
    }

    // template:begin grpc:load-listener-environment
    #[test]
    fn grpc_environment_enables_an_explicit_plaintext_listener() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__GRPC__ENABLED", "true"),
                ("APP__GRPC__ADDR", "127.0.0.1:0"),
                ("APP__GRPC__SECURITY", "plaintext"),
            ]),
        )
        .unwrap();
        assert!(cfg.grpc.enabled);
        assert_eq!(cfg.grpc.listen_addr().unwrap().port(), 0);
        assert_eq!(cfg.grpc.security, Some(crate::GrpcSecurity::Plaintext));
    }

    #[test]
    fn grpc_limits_load_from_the_environment_and_a_bad_one_names_its_key() {
        let listener = [
            ("APP__GRPC__ENABLED", "true"),
            ("APP__GRPC__ADDR", "127.0.0.1:0"),
            ("APP__GRPC__SECURITY", "plaintext"),
        ];
        let limits = [
            ("APP__GRPC__REQUEST_TIMEOUT", "3s"),
            ("APP__GRPC__MAX_IN_FLIGHT", "32"),
            ("APP__GRPC__MAX_CONNECTIONS", "0"),
            ("APP__GRPC__MAX_CONNECTION_AGE", "5m"),
        ];
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[listener.as_slice(), limits.as_slice()].concat()),
        )
        .unwrap();
        assert_eq!(cfg.grpc.request_timeout, Duration::from_secs(3));
        assert_eq!(
            cfg.grpc.in_flight_cap().map(std::num::NonZeroU32::get),
            Some(32)
        );
        assert_eq!(cfg.grpc.connection_cap(), None);
        assert_eq!(cfg.grpc.connection_age(), Some(Duration::from_mins(5)));

        let over_drain = [("APP__GRPC__REQUEST_TIMEOUT", "11s")];
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[listener.as_slice(), over_drain.as_slice()].concat()),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "grpc.request_timeout"),
            "{err}"
        );
    }

    #[test]
    fn grpc_private_key_is_environment_only_and_debug_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let listener = write(
            &dir,
            "grpc.toml",
            "[grpc]\nenabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"\ncertificate = \"public-cert\"\n",
        );
        let cfg = load_from(
            &LoadOptions {
                config: Some(listener),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[("APP__GRPC__PRIVATE_KEY", "private-key-material")]),
        )
        .unwrap();
        assert!(!format!("{cfg:?}").contains("private-key-material"));

        let leaked = write(
            &dir,
            "leaked.toml",
            "[grpc]\nprivate_key = \"private-key-material\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "grpc.private_key"),
            "{err}"
        );
    }
    // template:end grpc:load-listener-environment

    // template:begin http-idempotency:load-http-idempotency-environment
    #[test]
    fn http_idempotency_environment_sets_the_retention() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__HTTP_IDEMPOTENCY__RETENTION", "2h")]),
        )
        .unwrap();
        assert_eq!(
            cfg.http_idempotency.retention,
            Some(Duration::from_hours(2))
        );
    }
    // template:end http-idempotency:load-http-idempotency-environment

    // template:begin jobs:load-jobs-environment
    #[test]
    fn jobs_environment_sets_max_workers() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__JOBS__MAX_WORKERS", "8")]),
        )
        .unwrap();
        assert_eq!(cfg.jobs.max_workers.get(), 8);
    }
    // template:end jobs:load-jobs-environment

    // template:begin webhooks:load-webhooks-environment
    #[test]
    fn webhooks_environment_builds_endpoint_local_signing_keys() {
        use secrecy::ExposeSecret as _;

        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__WEBHOOKS__ENDPOINTS__PARTNER__URL",
                    "https://partner.example/events",
                ),
                (
                    "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
                    "fixture-current-secret",
                ),
                (
                    "APP__WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_SECRET",
                    "fixture-previous-secret",
                ),
                ("APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES", "4"),
            ]),
        )
        .unwrap();
        assert_eq!(
            cfg.webhooks
                .max_concurrent_deliveries
                .map(std::num::NonZeroU32::get),
            Some(4)
        );
        let endpoint = cfg.webhooks.endpoints.get("partner").unwrap();
        assert_eq!(endpoint.url, "https://partner.example/events");
        assert_eq!(endpoint.secret.expose_secret(), "fixture-current-secret");
        assert_eq!(
            endpoint.previous_secret.as_ref().unwrap().expose_secret(),
            "fixture-previous-secret"
        );
        assert!(!format!("{cfg:?}").contains("fixture-current-secret"));
        assert!(!format!("{cfg:?}").contains("fixture-previous-secret"));
    }

    #[test]
    fn webhooks_secret_in_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[webhooks.endpoints.partner]\nsecret = \"whsec_private\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "webhooks.endpoints.partner.secret"),
            "{err}"
        );
    }

    #[test]
    fn webhooks_secret_without_its_key_segment_is_not_echoed() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[(
                "APP__WEBHOOKS__ENDPOINTS__PARTNER",
                "fixture-current-secret",
            )]),
        )
        .unwrap_err();
        let rendered = err.to_string();
        assert!(matches!(&err, Error::Deserialize(_)), "{rendered}");
        assert!(
            rendered.contains("webhooks.endpoints.partner"),
            "{rendered}"
        );
        assert!(!rendered.contains("fixture-current-secret"), "{rendered}");
        assert!(!format!("{err:?}").contains("fixture-current-secret"));
    }

    #[test]
    fn webhooks_endpoint_id_the_environment_cannot_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "mixed-case.toml",
            "[webhooks.endpoints.Partner]\nurl = \"https://partner.example/events\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[(
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
                "fixture-current-secret",
            )]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::UnaddressableKey { key, .. } if key == "webhooks.endpoints.Partner"),
            "{err}"
        );
    }

    #[test]
    fn webhooks_hyphenated_endpoint_id_meets_its_environment_secret() {
        use secrecy::ExposeSecret as _;

        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "hyphen.toml",
            "[webhooks.endpoints.partner-v2]\nurl = \"https://partner.example/events\"\n",
        );
        let cfg = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[(
                "APP__WEBHOOKS__ENDPOINTS__PARTNER-V2__SECRET",
                "fixture-current-secret",
            )]),
        )
        .unwrap();
        assert_eq!(cfg.webhooks.endpoints.len(), 1);
        let endpoint = cfg.webhooks.endpoints.get("partner-v2").unwrap();
        assert_eq!(endpoint.secret.expose_secret(), "fixture-current-secret");
    }

    #[test]
    fn webhooks_blank_environment_secrets_are_refused() {
        let base = [
            (
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__URL",
                "https://partner.example/events",
            ),
            (
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
                "fixture-current-secret",
            ),
        ];
        for (name, value, expected_key) in [
            (
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
                "",
                "webhooks.endpoints.partner.secret",
            ),
            (
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_SECRET",
                "  ",
                "webhooks.endpoints.partner.previous_secret",
            ),
        ] {
            let mut variables = base.to_vec();
            if name.ends_with("__SECRET") {
                variables[1] = (name, value);
            } else {
                variables.push((name, value));
            }
            let err = load_from(&LoadOptions::default(), BUILD, env(&variables)).unwrap_err();
            assert!(
                matches!(&err, Error::Validate(validation) if validation.key == expected_key),
                "{err}"
            );
            assert_eq!(
                err.to_string(),
                format!("configuration is invalid: {expected_key}: cannot be empty")
            );
            assert!(!format!("{err:?}").contains("fixture-current-secret"));
        }
    }
    // template:end webhooks:load-webhooks-environment

    // template:begin messaging:load-messaging-environment
    #[test]
    fn messaging_environment_decodes_lists_and_redacts_credentials() {
        use secrecy::ExposeSecret as _;

        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__MESSAGING__URLS",
                    "tls://nats-a.example:4222,tls://nats-b.example:4222",
                ),
                ("APP__MESSAGING__CREDENTIALS", "fixture-credentials"),
                ("APP__MESSAGING__ROOT_CA_PATH", "/etc/nats/root-ca.pem"),
                ("APP__MESSAGING__SOURCE_STREAM", "events"),
                ("APP__MESSAGING__MAX_PAYLOAD_BYTES", "2 MiB"),
                ("APP__MESSAGING__CONSUMER_DURABLE", "service-events"),
                ("APP__MESSAGING__CONSUMER_FILTER_SUBJECT", "events.>"),
                ("APP__MESSAGING__DLQ_SUBJECT", "events.dlq"),
                ("APP__MESSAGING__CONSUMER_CONCURRENCY", "2"),
            ]),
        )
        .unwrap();
        assert_eq!(
            cfg.messaging.urls,
            vec![
                "tls://nats-a.example:4222".to_owned(),
                "tls://nats-b.example:4222".to_owned(),
            ]
        );
        assert_eq!(
            cfg.messaging.credentials.as_ref().unwrap().expose_secret(),
            "fixture-credentials"
        );
        assert_eq!(
            cfg.messaging.root_ca_path.as_deref(),
            Some(Path::new("/etc/nats/root-ca.pem"))
        );
        assert_eq!(cfg.messaging.max_payload_bytes, bytesize::ByteSize::mib(2));
        assert_eq!(cfg.messaging.consumer_concurrency.get(), 2);
        assert!(!cfg.messaging.trusted_network);
        assert!(!format!("{cfg:?}").contains("fixture-credentials"));
    }

    #[test]
    fn messaging_trusted_network_admits_plaintext_in_production() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__APP__ENV", "production"),
                ("APP__MESSAGING__URLS", "nats://nats.internal:4222"),
                ("APP__MESSAGING__TRUSTED_NETWORK", "true"),
                ("APP__MESSAGING__CREDENTIALS", "fixture-credentials"),
                ("APP__MESSAGING__SOURCE_STREAM", "events"),
            ]),
        )
        .unwrap();
        assert!(cfg.messaging.trusted_network);
        assert!(cfg.messaging.plaintext_admitted());
        cfg.messaging.validate_producer("production").unwrap();
    }

    #[test]
    fn messaging_credentials_file_is_a_path_a_file_or_the_environment_may_set() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = write(
            &dir,
            "messaging.toml",
            "[messaging]\ncredentials_file = \"/run/secrets/from-file.creds\"\n",
        );
        let options = LoadOptions {
            config: Some(overlay),
            ..LoadOptions::default()
        };
        let cfg = load_from(&options, BUILD, env(&[])).unwrap();
        assert_eq!(
            cfg.messaging.credentials_file.as_deref(),
            Some(Path::new("/run/secrets/from-file.creds"))
        );

        let cfg = load_from(
            &options,
            BUILD,
            env(&[(
                "APP__MESSAGING__CREDENTIALS_FILE",
                "/run/secrets/nats.creds",
            )]),
        )
        .unwrap();
        assert_eq!(
            cfg.messaging.credentials_file.as_deref(),
            Some(Path::new("/run/secrets/nats.creds"))
        );

        let cfg = load_from(
            &options,
            BUILD,
            env(&[("APP__MESSAGING__CREDENTIALS_FILE", " ")]),
        )
        .unwrap();
        assert_eq!(cfg.messaging.credentials_file, None);
    }

    #[test]
    fn messaging_empty_root_ca_path_unsets_the_file_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "ca.toml",
            "[messaging]\nroot_ca_path = \"/etc/nats/root-ca.pem\"\n",
        );
        let cfg = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[("APP__MESSAGING__ROOT_CA_PATH", "")]),
        )
        .unwrap();
        assert_eq!(cfg.messaging.root_ca_path, None);
    }

    #[test]
    fn messaging_urls_accept_a_file_list_and_refuse_other_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let list = write(
            &dir,
            "list.toml",
            "[messaging]\nurls = [\"tls://nats-a.example:4222\", \"tls://nats-b.example:4222\"]\n",
        );
        let cfg = load_from(
            &LoadOptions {
                config: Some(list.clone()),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap();
        assert_eq!(cfg.messaging.urls.len(), 2);

        let overridden = load_from(
            &LoadOptions {
                config: Some(list),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[("APP__MESSAGING__URLS", "tls://nats-c.example:4222")]),
        )
        .unwrap();
        assert_eq!(overridden.messaging.urls, ["tls://nats-c.example:4222"]);

        let number = write(&dir, "number.toml", "[messaging]\nurls = 4222\n");
        let err = load_from(
            &LoadOptions {
                config: Some(number),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("must be a list of strings or a comma-separated string"),
            "{err}"
        );

        for blank in ["", "tls://nats-a.example:4222,"] {
            let err = load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[("APP__MESSAGING__URLS", blank)]),
            )
            .unwrap_err();
            assert!(
                matches!(&err, Error::Validate(error) if error.key == "messaging.urls"),
                "{err}"
            );
        }
    }

    #[test]
    fn messaging_url_list_parsing_preserves_scalar_secret_bytes() {
        use secrecy::ExposeSecret as _;

        for secret in ["00123", "TRUE"] {
            let cfg = load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[
                    ("APP__MESSAGING__URLS", "tls://nats.example:4222"),
                    ("APP__MESSAGING__CREDENTIALS", secret),
                ]),
            )
            .unwrap();
            assert_eq!(cfg.messaging.urls, ["tls://nats.example:4222"]);
            assert_eq!(
                cfg.messaging.credentials.as_ref().unwrap().expose_secret(),
                secret
            );
        }
    }

    #[test]
    fn messaging_credentials_in_a_file_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[messaging]\ncredentials = \"fixture-credentials\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "messaging.credentials"),
            "{err}"
        );
    }

    #[test]
    fn messaging_unknown_environment_key_fails_loading() {
        assert!(matches!(
            load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[("APP__MESSAGING__UNKNOWN", "x")]),
            ),
            Err(Error::Deserialize(_))
        ));
    }
    // template:end messaging:load-messaging-environment

    // template:begin cache:load-cache-environment
    #[test]
    fn cache_environment_reads_the_dsn_and_redacts_it() {
        use secrecy::ExposeSecret as _;

        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__CACHE__DSN", "redis://:hunter2@127.0.0.1:6379"),
                ("APP__CACHE__COMMAND_TIMEOUT", "200ms"),
                ("APP__APP__ENV", "local"),
                ("APP__CACHE__ALLOW_PLAINTEXT", "true"),
                ("APP__CACHE__ALLOW_UNAUTHENTICATED", "true"),
            ]),
        )
        .unwrap();
        assert_eq!(
            cfg.cache.dsn.as_ref().unwrap().expose_secret(),
            "redis://:hunter2@127.0.0.1:6379"
        );
        assert_eq!(cfg.cache.command_timeout, Duration::from_millis(200));
        assert!(!format!("{cfg:?}").contains("hunter2"));
    }

    #[test]
    fn cache_empty_root_ca_path_unsets_the_file_value() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "ca.toml",
            "[cache]\nroot_ca_path = \" /etc/cache/root-ca.pem \"\n",
        );
        let options = LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        };
        let set = load_from(&options, BUILD, env(&[])).unwrap();
        assert_eq!(
            set.cache.root_ca_path.as_deref(),
            Some(Path::new("/etc/cache/root-ca.pem"))
        );
        let unset = load_from(&options, BUILD, env(&[("APP__CACHE__ROOT_CA_PATH", " ")])).unwrap();
        assert_eq!(unset.cache.root_ca_path, None);
    }

    #[test]
    fn cache_dsn_in_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[cache]\ndsn = \"redis://:hunter2@127.0.0.1:6379\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "cache.dsn"),
            "{err}"
        );
    }

    #[test]
    fn cache_password_file_is_a_path_a_file_or_the_environment_may_set() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = write(
            &dir,
            "cache.toml",
            "[cache]\npassword_file = \"/run/secrets/from-file\"\n",
        );
        let options = LoadOptions {
            config: Some(overlay),
            ..LoadOptions::default()
        };
        let cfg = load_from(&options, BUILD, env(&[])).unwrap();
        assert_eq!(
            cfg.cache.password_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/from-file"))
        );

        let cfg = load_from(
            &options,
            BUILD,
            env(&[("APP__CACHE__PASSWORD_FILE", "/run/secrets/cache-password")]),
        )
        .unwrap();
        assert_eq!(
            cfg.cache.password_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/cache-password"))
        );

        // A blank variable unsets the key, as it does for every optional path.
        let cfg = load_from(&options, BUILD, env(&[("APP__CACHE__PASSWORD_FILE", " ")])).unwrap();
        assert_eq!(cfg.cache.password_file, None);
    }

    #[test]
    fn cache_client_certificate_paths_are_set_from_the_environment_together() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__CACHE__CLIENT_CERT_PATH", "/run/tls/client.crt"),
                ("APP__CACHE__CLIENT_KEY_PATH", "/run/tls/client.key"),
            ]),
        )
        .unwrap();
        assert_eq!(
            cfg.cache.client_cert_path.as_deref(),
            Some(std::path::Path::new("/run/tls/client.crt"))
        );
        assert_eq!(
            cfg.cache.client_key_path.as_deref(),
            Some(std::path::Path::new("/run/tls/client.key"))
        );

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__CACHE__CLIENT_CERT_PATH", "/run/tls/client.crt"),
                ("APP__CACHE__CLIENT_KEY_PATH", ""),
            ]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("cache.client_key_path"), "{err}");
    }

    #[test]
    fn cache_unknown_environment_key_fails_loading() {
        assert!(matches!(
            load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[("APP__CACHE__UNKNOWN", "x")]),
            ),
            Err(Error::Deserialize(_))
        ));
    }
    // template:end cache:load-cache-environment

    // template:begin object-storage:load-object-storage-environment
    #[test]
    fn object_storage_environment_maps_railway_bucket_variables() {
        use secrecy::ExposeSecret as _;

        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__OBJECT_STORAGE__PROVIDER", "railway"),
                ("APP__OBJECT_STORAGE__BUCKET", "results-jdhhd8oe18xi"),
                ("APP__OBJECT_STORAGE__ENDPOINT", "https://t3.storageapi.dev"),
                ("APP__OBJECT_STORAGE__REGION", "auto"),
                ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "tid_example"),
                ("APP__OBJECT_STORAGE__SECRET_ACCESS_KEY", "hunter2"),
                ("APP__OBJECT_STORAGE__MAX_OBJECT_BYTES", "2 MiB"),
                ("APP__OBJECT_STORAGE__MAX_CONCURRENCY", "32"),
                ("APP__OBJECT_STORAGE__OPERATION_TIMEOUT", "10s"),
            ]),
        )
        .unwrap();
        let storage = &cfg.object_storage;
        assert_eq!(storage.provider, crate::ObjectStorageProvider::Railway);
        assert_eq!(storage.bucket, "results-jdhhd8oe18xi");
        assert_eq!(
            storage.secret_access_key.as_ref().unwrap().expose_secret(),
            "hunter2"
        );
        assert_eq!(storage.max_object_bytes.as_u64(), 2 * 1024 * 1024);
        assert_eq!(storage.max_concurrency, 32);
        assert_eq!(storage.operation_timeout, Duration::from_secs(10));
        assert!(!format!("{cfg:?}").contains("hunter2"));
    }

    #[test]
    fn object_storage_environment_selects_credentials_and_addressing() {
        let amazon = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__OBJECT_STORAGE__PROVIDER", "amazon_s3"),
                ("APP__OBJECT_STORAGE__BUCKET", "document-results"),
                ("APP__OBJECT_STORAGE__REGION", "eu-central-1"),
                ("APP__OBJECT_STORAGE__EXPECTED_BUCKET_OWNER", "123456789012"),
                ("APP__OBJECT_STORAGE__CREDENTIALS", "workload_identity"),
            ]),
        )
        .unwrap();
        assert_eq!(
            amazon.object_storage.credentials,
            crate::ObjectStorageCredentials::WorkloadIdentity
        );

        let generic = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__OBJECT_STORAGE__PROVIDER", "s3_compatible"),
                ("APP__OBJECT_STORAGE__BUCKET", "document-results"),
                (
                    "APP__OBJECT_STORAGE__ENDPOINT",
                    "https://ceph.internal.example:8443",
                ),
                ("APP__OBJECT_STORAGE__PATH_STYLE", "true"),
                ("APP__OBJECT_STORAGE__ACCESS_KEY_ID", "example"),
                ("APP__OBJECT_STORAGE__SECRET_ACCESS_KEY", "hunter2"),
            ]),
        )
        .unwrap();
        assert_eq!(
            generic.object_storage.provider,
            crate::ObjectStorageProvider::S3Compatible
        );
        assert!(generic.object_storage.path_style);
    }

    #[test]
    fn object_storage_secret_in_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[object_storage]\nsecret_access_key = \"hunter2\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "object_storage.secret_access_key"),
            "{err}"
        );
    }

    #[test]
    fn object_storage_unknown_provider_fails_loading() {
        assert!(matches!(
            load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[("APP__OBJECT_STORAGE__PROVIDER", "minio")]),
            ),
            Err(Error::Deserialize(_))
        ));
    }
    // template:end object-storage:load-object-storage-environment

    // template:begin outbound-auth:load-integrations-environment
    #[test]
    fn oauth_environment_builds_a_named_tuple_and_redacts_its_values() {
        use secrecy::ExposeSecret as _;

        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/oauth2/token?tenant=blue",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__SCOPES",
                    "billing.read billing.write",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__AUDIENCE",
                    "https://billing-api.example",
                ),
            ]),
        )
        .unwrap();

        let oauth = cfg
            .integrations
            .get("billing")
            .and_then(|integration| integration.oauth.as_ref())
            .expect("named OAuth tuple");
        assert_eq!(
            oauth.token_url,
            "https://identity.example/oauth2/token?tenant=blue"
        );
        assert_eq!(oauth.client_id, "billing-service");
        assert_eq!(oauth.private_key.expose_secret(), "test-private-key");
        assert_eq!(oauth.key_id, "key-1");
        assert_eq!(oauth.algorithm, crate::OAuthAlgorithm::Rs256);
        assert_eq!(oauth.assertion_audience, "https://identity.example");
        assert_eq!(oauth.scopes.as_slice(), ["billing.read", "billing.write"]);
        assert_eq!(
            oauth.audience.as_deref(),
            Some("https://billing-api.example")
        );

        let debug = format!("{cfg:?}");
        for value in [
            "test-private-key",
            "billing.read",
            "https://billing-api.example",
            "identity.example/oauth2/token",
        ] {
            assert!(
                !debug.contains(value),
                "configuration debug output exposed {value}"
            );
        }
    }

    #[test]
    fn oauth_without_an_algorithm_fails_naming_the_key() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ]),
        )
        .unwrap_err();
        assert!(matches!(&err, Error::Deserialize(_)), "{err}");
        assert!(
            err.to_string()
                .contains("missing configuration field \"integrations.billing.oauth.algorithm\""),
            "{err}"
        );
    }

    #[test]
    fn oauth_exchange_cache_capacity_has_a_default_an_environment_form_and_a_range() {
        let load = |capacity: Option<&'static str>| {
            let mut variables = vec![
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "ES256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ];
            if let Some(capacity) = capacity {
                variables.push((
                    "APP__INTEGRATIONS__BILLING__OAUTH__EXCHANGE_CACHE_CAPACITY",
                    capacity,
                ));
            }
            load_from(&LoadOptions::default(), BUILD, env(&variables))
        };
        let capacity = |cfg: Config| {
            cfg.integrations["billing"]
                .oauth
                .as_ref()
                .expect("named OAuth tuple")
                .exchange_cache_capacity
        };
        assert_eq!(capacity(load(None).unwrap()), 1024);
        assert_eq!(capacity(load(Some("4096")).unwrap()), 4096);

        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "capacity.toml",
            "[integrations.billing.oauth]\nexchange_cache_capacity = 2048\n",
        );
        let from_file = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "ES256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ]),
        )
        .unwrap();
        assert_eq!(capacity(from_file), 2048);

        for rejected in ["0", "65537"] {
            let err = load(Some(rejected)).unwrap_err();
            assert!(
                matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.exchange_cache_capacity" && error.message == "must be from 1 to 65536"),
                "{err}"
            );
        }
        let err = load(Some("many")).unwrap_err();
        assert!(matches!(&err, Error::Deserialize(_)), "{err}");
        let rendered = err.to_string();
        assert!(
            rendered.contains(
                "invalid value, expected an integer for key `integrations.billing.oauth.exchange_cache_capacity`"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("many"), "{rendered}");
    }

    fn load_oauth_provider_concurrency(
        file_value: Option<&str>,
        environment_value: Option<&str>,
    ) -> Result<Config, Error> {
        let dir = tempfile::tempdir().unwrap();
        let mut content = String::from(
            "[integrations.billing.oauth]\n\
             token_url = \"https://identity.example/token\"\n\
             client_id = \"billing-service\"\n\
             key_id = \"key-1\"\n\
             algorithm = \"ES256\"\n\
             assertion_audience = \"https://identity.example\"\n",
        );
        if let Some(value) = file_value {
            use std::fmt::Write as _;

            writeln!(content, "provider_concurrency = {value}").unwrap();
        }
        let file = write(&dir, "provider-concurrency.toml", &content);
        let mut variables = vec![(
            "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
            "test-private-key",
        )];
        if let Some(value) = environment_value {
            variables.push((
                "APP__INTEGRATIONS__BILLING__OAUTH__PROVIDER_CONCURRENCY",
                value,
            ));
        }
        load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&variables),
        )
    }

    #[test]
    fn oauth_provider_concurrency_defaults_and_loads_file_and_environment_boundaries() {
        for (file, environment, expected) in [
            (None, None, 32),
            (Some("7"), None, 7),
            (Some("\"9\""), None, 9),
            (Some("7"), Some("11"), 11),
            (Some("1"), None, 1),
            (None, Some("1"), 1),
            (Some("4294967295"), None, u32::MAX),
            (None, Some("4294967295"), u32::MAX),
        ] {
            let cfg = load_oauth_provider_concurrency(file, environment).unwrap();
            assert_eq!(
                cfg.integrations["billing"]
                    .oauth
                    .as_ref()
                    .unwrap()
                    .provider_concurrency,
                expected,
                "file={file:?}, environment={environment:?}"
            );
        }
    }

    #[test]
    fn oauth_provider_concurrency_rejects_invalid_scalars_without_echoing_values() {
        for (file, environment) in [(Some("0"), None), (None, Some("0"))] {
            let err = load_oauth_provider_concurrency(file, environment).unwrap_err();
            assert!(
                matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.provider_concurrency" && error.message == "must be greater than zero"),
                "{err}"
            );
        }
        for (file, environment) in [
            ("-1", "-1"),
            ("1.5", "1.5"),
            ("1.0", "1.0"),
            ("true", "true"),
            ("4294967296", "4294967296"),
            ("\"private-sentinel\"", "private-sentinel"),
            ("\"\"", ""),
            ("[1]", "[1]"),
        ] {
            for (file, environment) in [(Some(file), None), (None, Some(environment))] {
                let err = load_oauth_provider_concurrency(file, environment).unwrap_err();
                assert!(matches!(&err, Error::Deserialize(_)), "{err}");
                let rendered = err.to_string();
                assert!(
                    rendered.contains(
                        "must be an integer from 0 to 4294967295 for key `integrations.billing.oauth.provider_concurrency`"
                    ),
                    "{rendered}"
                );
                assert!(!rendered.contains("private-sentinel"), "{rendered}");
            }
        }
    }

    #[test]
    fn oauth_algorithm_accepts_ps256_and_es256() {
        for (value, expected) in [
            ("PS256", crate::OAuthAlgorithm::Ps256),
            ("ES256", crate::OAuthAlgorithm::Es256),
        ] {
            let cfg = load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[
                    (
                        "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                        "https://identity.example/token",
                    ),
                    (
                        "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                        "billing-service",
                    ),
                    (
                        "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                        "test-private-key",
                    ),
                    ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                    (
                        "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                        "https://identity.example",
                    ),
                    ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", value),
                ]),
            )
            .unwrap();
            let oauth = cfg
                .integrations
                .get("billing")
                .and_then(|integration| integration.oauth.as_ref())
                .expect("named OAuth tuple");
            assert_eq!(oauth.algorithm, expected, "algorithm {value}");
        }
    }

    #[test]
    fn oauth_bad_algorithm_fails_without_echoing_the_value() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "HS256"),
            ]),
        )
        .unwrap_err();
        assert!(matches!(&err, Error::Deserialize(_)), "{err}");
        let rendered = err.to_string();
        assert!(
            rendered.contains(
                "must be RS256, PS256 or ES256 for key `integrations.billing.oauth.algorithm`"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("HS256"), "{rendered}");
    }

    #[test]
    fn oauth_client_secret_is_refused_as_an_unknown_key() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_SECRET",
                    "test-client-secret",
                ),
            ]),
        )
        .unwrap_err();
        assert!(matches!(&err, Error::Deserialize(_)), "{err}");
        let rendered = err.to_string();
        assert!(
            rendered.contains("unknown field `client_secret`")
                && rendered.contains("for key `integrations.billing.oauth`"),
            "{rendered}"
        );
        assert!(!rendered.contains("test-client-secret"), "{rendered}");
    }

    #[test]
    fn oauth_private_key_in_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[integrations.billing.oauth]\nprivate_key = \"test-private-key\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "integrations.billing.oauth.private_key"),
            "{err}"
        );
    }

    #[test]
    fn oauth_invalid_values_fail_with_static_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let invalid = write(
            &dir,
            "invalid.toml",
            "[integrations.billing.oauth]\ntoken_url = \"http://identity.example/token?private=value\"\nclient_id = \"billing-service\"\nscopes = [\"bad scope\"]\naudience = \"private-audience\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(invalid),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.token_url" && error.message == "must use HTTPS"),
            "{err}"
        );
        let rendered = err.to_string();
        for value in [
            "identity.example/token",
            "private=value",
            "private-audience",
        ] {
            assert!(
                !rendered.contains(value),
                "configuration error exposed {value}"
            );
        }
    }

    #[test]
    fn oauth_invalid_types_have_static_key_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let wrong_type = write(
            &dir,
            "wrong-type.toml",
            "[integrations.billing.oauth]\ntoken_url = [\"private-sentinel\"]\nclient_id = \"billing-service\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(wrong_type),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ]),
        )
        .unwrap_err();
        assert!(matches!(&err, Error::Deserialize(_)), "{err}");
        let rendered = err.to_string();
        assert!(
            rendered.contains(
                "invalid value, expected a string for key `integrations.billing.oauth.token_url`"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("private-sentinel"), "{rendered}");
        assert!(!rendered.contains("in the environment"), "{rendered}");

        for (name, contents, expected_key) in [
            (
                "invalid-integration.toml",
                "[integrations]\nbilling = \"private-sentinel\"\n",
                "integrations.billing",
            ),
            (
                "invalid-oauth.toml",
                "[integrations.billing]\noauth = \"private-sentinel\"\n",
                "integrations.billing.oauth",
            ),
        ] {
            let malformed = write(&dir, name, contents);
            let err = load_from(
                &LoadOptions {
                    config: Some(malformed),
                    ..LoadOptions::default()
                },
                BUILD,
                env(&[]),
            )
            .unwrap_err();
            let rendered = err.to_string();
            assert!(rendered.contains(expected_key), "{rendered}");
            assert!(!rendered.contains("private-sentinel"), "{rendered}");
        }
    }

    #[test]
    fn oauth_scope_presence_and_client_id_validation_stays_private() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_scope = write(
            &dir,
            "invalid-scope.toml",
            "[integrations.billing.oauth]\ntoken_url = \"https://identity.example/token\"\nclient_id = \"billing-service\"\nscopes = [\"bad scope\"]\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(invalid_scope),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.scopes" && error.message == "must contain RFC 6749 scope tokens"),
            "{err}"
        );
        assert!(!err.to_string().contains("bad scope"), "{err}");

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.client_id" && error.message == "cannot be empty"),
            "{err}"
        );
    }

    #[test]
    fn oauth_blank_new_required_keys_are_refused() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.private_key" && error.message == "cannot be empty"),
            "{err}"
        );

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.key_id" && error.message == "cannot be empty"),
            "{err}"
        );

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.assertion_audience" && error.message == "cannot be empty"),
            "{err}"
        );
    }

    #[test]
    fn oauth_userinfo_validation_stays_private() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://@identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.token_url" && error.message == "must not include userinfo"),
            "{err}"
        );
    }
    // template:end outbound-auth:load-integrations-environment

    // template:begin grpc:load-integration-grpc-environment
    #[test]
    fn grpc_integration_environment_builds_a_lazy_client_tuple() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__GRPC__DESTINATION",
                    "https://billing.example:8443",
                ),
                ("APP__INTEGRATIONS__BILLING__GRPC__SECURITY", "tls"),
                (
                    "APP__INTEGRATIONS__BILLING__GRPC__CA_CERTIFICATE",
                    "public-ca",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__GRPC__CERTIFICATE",
                    "public-client-cert",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__GRPC__PRIVATE_KEY",
                    "private-client-key",
                ),
            ]),
        )
        .unwrap();
        let grpc = cfg
            .integrations
            .get("billing")
            .and_then(|integration| integration.grpc.as_ref())
            .expect("named gRPC tuple");
        assert_eq!(grpc.destination, "https://billing.example:8443");
        assert_eq!(grpc.security, crate::GrpcSecurity::Tls);
        assert!(!format!("{cfg:?}").contains("private-client-key"));
    }

    #[test]
    fn grpc_integration_refuses_unpaired_mtls_material() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__GRPC__DESTINATION",
                    "https://billing.example:8443",
                ),
                ("APP__INTEGRATIONS__BILLING__GRPC__SECURITY", "tls"),
                (
                    "APP__INTEGRATIONS__BILLING__GRPC__CERTIFICATE",
                    "public-client-cert",
                ),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.grpc.certificate/integrations.billing.grpc.private_key"),
            "{err}"
        );
    }
    // template:end grpc:load-integration-grpc-environment

    // template:begin inbound-webhooks:load-inbound-webhooks-environment
    #[test]
    fn inbound_webhooks_environment_builds_nested_endpoint_and_secret_maps() {
        use secrecy::ExposeSecret as _;

        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__ACTIVE_KEY",
                    "partner_v2",
                ),
                (
                    "APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V2",
                    "fixture-secret",
                ),
                ("APP__POSTGRES__ENABLED", "true"),
                ("APP__POSTGRES__DSN", "postgres://localhost/app"),
            ]),
        )
        .unwrap();
        assert_eq!(
            cfg.inbound_webhooks
                .endpoints
                .get("partner")
                .unwrap()
                .active_key,
            "partner_v2"
        );
        assert_eq!(
            cfg.inbound_webhooks
                .secrets
                .get("partner_v2")
                .unwrap()
                .expose_secret(),
            "fixture-secret"
        );
        assert!(!format!("{cfg:?}").contains("fixture-secret"));
    }

    #[test]
    fn inbound_webhooks_secrets_without_a_reference_are_not_echoed() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__INBOUND_WEBHOOKS__SECRETS", "fixture-secret-value")]),
        )
        .unwrap_err();
        let rendered = err.to_string();
        assert!(matches!(&err, Error::Deserialize(_)), "{rendered}");
        assert!(rendered.contains("inbound_webhooks.secrets"), "{rendered}");
        assert!(!rendered.contains("fixture-secret-value"), "{rendered}");
    }
    // template:end inbound-webhooks:load-inbound-webhooks-environment

    // template:begin oidc-introspection:load-introspection-environment
    fn introspection_environment(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut values = env(&[
            ("APP__AUTHN__MODE", "oidc-introspection"),
            ("APP__AUTHN__ISSUER", "https://issuer.example/tenant"),
            ("APP__AUTHN__AUDIENCE", "00123"),
            (
                "APP__AUTHN__INTROSPECTION_ENDPOINT",
                "https://issuer.example/introspect",
            ),
            ("APP__AUTHN__INTROSPECTION_CLIENT_ID", "false"),
            ("APP__AUTHN__INTROSPECTION_CLIENT_SECRET", "000042"),
        ]);
        values.extend(env(extra));
        values
    }

    #[test]
    fn introspection_environment_decodes_and_redacts_the_secret() {
        use secrecy::ExposeSecret;

        for (enabled, expected) in [("true", true), ("false", false)] {
            let cfg = load_from(
                &LoadOptions::default(),
                BUILD,
                introspection_environment(&[
                    ("APP__AUTHN__PROVIDER_CONCURRENCY", "7"),
                    ("APP__AUTHN__CACHE_ENABLED", enabled),
                    ("APP__AUTHN__CACHE_CAPACITY", "64"),
                    ("APP__AUTHN__CACHE_TTL", "1m 30s"),
                ]),
            )
            .unwrap();
            let AuthnConfig::OidcIntrospection {
                audience,
                introspection_client_id,
                introspection_client_secret,
                provider_concurrency,
                cache_enabled,
                cache_capacity,
                cache_ttl,
                ..
            } = &cfg.authn
            else {
                panic!("expected OIDC introspection configuration");
            };
            assert_eq!(*cache_enabled, expected);
            assert_eq!(*cache_capacity, 64);
            assert_eq!(*cache_ttl, Duration::from_secs(90));
            assert_eq!(provider_concurrency.get(), 7);
            assert_eq!(audience.as_slice(), ["00123"]);
            assert_eq!(introspection_client_id, "false");
            assert_eq!(
                introspection_client_secret
                    .as_ref()
                    .unwrap()
                    .expose_secret(),
                "000042"
            );
            assert!(!format!("{cfg:?}").contains("000042"));
        }
    }

    #[test]
    fn introspection_environment_rejects_invalid_scalar_overrides() {
        for (key, value) in [
            ("APP__AUTHN__CACHE_ENABLED", ""),
            ("APP__AUTHN__CACHE_ENABLED", "not-a-boolean"),
            ("APP__AUTHN__CACHE_CAPACITY", ""),
            ("APP__AUTHN__CACHE_CAPACITY", "-1"),
            ("APP__AUTHN__CACHE_CAPACITY", "1.5"),
            ("APP__AUTHN__PROVIDER_CONCURRENCY", ""),
            ("APP__AUTHN__PROVIDER_CONCURRENCY", "0"),
            ("APP__AUTHN__PROVIDER_CONCURRENCY", "4294967296"),
        ] {
            assert!(
                load_from(
                    &LoadOptions::default(),
                    BUILD,
                    introspection_environment(&[(key, value)]),
                )
                .is_err(),
                "{key}={value}"
            );
        }
    }
    // template:end oidc-introspection:load-introspection-environment

    // template:begin oidc-jwt:load-jwt-token-profile-environment
    #[test]
    fn jwt_environment_preserves_a_supplied_token_profile_and_jwks_uri() {
        let jwt_environment = |jwks_uri: &'static str| {
            env(&[
                ("APP__AUTHN__MODE", "oidc-jwt"),
                ("APP__AUTHN__ISSUER", "https://issuer.example/tenant"),
                ("APP__AUTHN__AUDIENCE", "service"),
                ("APP__AUTHN__TOKEN_PROFILE", "rfc9068"),
                ("APP__AUTHN__JWKS_URI", jwks_uri),
            ])
        };
        let jwt = load_from(
            &LoadOptions::default(),
            BUILD,
            jwt_environment("https://issuer.example/keys"),
        )
        .unwrap();
        let AuthnConfig::OidcJwt {
            token_profile,
            jwks_uri,
            ..
        } = jwt.authn
        else {
            panic!("expected OIDC JWT configuration");
        };
        assert_eq!(token_profile, TokenProfile::Rfc9068);
        assert_eq!(jwks_uri.as_deref(), Some("https://issuer.example/keys"));
        assert!(load_from(&LoadOptions::default(), BUILD, jwt_environment(" ")).is_err());
    }
    // template:end oidc-jwt:load-jwt-token-profile-environment

    // template:begin oidc-introspection:load-introspection-secret-file
    #[test]
    fn introspection_secret_in_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[authn]\nintrospection_client_secret = \"file-secret\"\n",
        );
        let err = load_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "authn.introspection_client_secret"),
            "{err}"
        );
    }
    // template:end oidc-introspection:load-introspection-secret-file

    // template:begin oidc-jwt:load-jwt-token-profile-file
    #[test]
    fn jwt_token_profile_in_a_file_is_public() {
        let dir = tempfile::tempdir().unwrap();
        let profile = write(
            &dir,
            "profile.toml",
            "[authn]\nmode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\ntoken_profile = \"rfc9068\"\n",
        );
        let cfg = load_from(
            &LoadOptions {
                config: Some(profile),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap();
        let AuthnConfig::OidcJwt { token_profile, .. } = cfg.authn else {
            panic!("expected OIDC JWT configuration");
        };
        assert_eq!(token_profile, TokenProfile::Rfc9068);
    }
    // template:end oidc-jwt:load-jwt-token-profile-file

    // template:begin postgres:load-migration-test
    #[test]
    fn migration_snapshot_ignores_the_sections_it_does_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "service.toml",
            "[http]\nrequest_timeout = \"not a duration\"\n[postgres]\nenabled = true\nmax_connections = 2\n",
        );
        let options = LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        };
        let variables = [
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
            (
                "APP__POSTGRES__PASSWORD_FILE",
                "/run/secrets/postgres-password",
            ),
            ("APP__POSTGRES__SESSION_BUDGETS", "server"),
            ("APP__POSTGRES__MIGRATION_DEADLINE", "2h"),
            ("APP__LOG__FORMAT", "text"),
            ("APP__GRPC__ENABLED", "true"),
        ];
        assert!(load_from(&options, BUILD, env(&variables)).is_err());

        let cfg = load_migration_from(&options, BUILD, env(&variables)).unwrap();
        assert!(cfg.postgres.enabled);
        assert_eq!(cfg.postgres.max_connections.get(), 2);
        assert!(cfg.postgres.has_dsn());
        assert_eq!(
            cfg.postgres.password_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/postgres-password"))
        );
        assert_eq!(
            cfg.postgres.session_budgets,
            crate::PostgresSessionBudgets::Server
        );
        assert_eq!(
            cfg.postgres.migration_deadline,
            std::time::Duration::from_hours(2)
        );
        assert_eq!(cfg.log.format, LogFormat::Text);
        assert_eq!(cfg.app.version, "1.2.3");
    }

    #[test]
    fn migration_snapshot_keeps_the_rules_of_its_own_sections() {
        let unknown = load_migration_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__POSTGRES__BOGUS", "1")]),
        )
        .unwrap_err();
        assert!(matches!(unknown, Error::Deserialize(_)), "{unknown}");

        let unknown_source = load_migration_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__POSTGRES__SESSION_BUDGETS", "pooler")]),
        )
        .unwrap_err();
        assert!(
            matches!(unknown_source, Error::Deserialize(_)),
            "{unknown_source}"
        );

        let unbounded = load_migration_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__POSTGRES__MIGRATION_DEADLINE", "25h")]),
        )
        .unwrap_err();
        assert!(
            matches!(&unbounded, Error::Validate(error) if error.key == "postgres.migration_deadline"),
            "{unbounded}"
        );

        let missing_dsn = load_migration_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__POSTGRES__ENABLED", "true")]),
        )
        .unwrap_err();
        assert!(
            matches!(&missing_dsn, Error::Validate(error) if error.key == "postgres.dsn"),
            "{missing_dsn}"
        );

        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[grpc]\nprivate_key = \"private-key-material\"\n",
        );
        let err = load_migration_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::SecretInFile { .. }), "{err}");
    }
    // template:end postgres:load-migration-test

    // template:begin jobs:load-jobs-operator-tests
    #[test]
    fn jobs_operator_ignores_unrelated_sections_and_worker_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(
            &dir,
            "operator.toml",
            "[postgres]\nenabled = true\nmax_connections = 1\n\
             [http]\nrequest_timeout = \"invalid\"\n\
             [jobs]\nmax_workers = 0\n",
        );
        let cfg = load_jobs_operator_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            env(&[
                ("APP__POSTGRES__DSN", "postgres://localhost/app"),
                ("APP__AUTHN__MODE", "invalid"),
                ("APP__MESSAGING__ENABLED", "invalid"),
                ("APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES", "invalid"),
                ("APP__OBJECT_STORAGE__ENABLED", "invalid"),
                ("APP__LOG__FORMAT", "invalid"),
                ("APP__OBSERVABILITY__OTEL", "invalid"),
            ]),
        )
        .unwrap();
        assert!(cfg.postgres.enabled);
        assert!(cfg.postgres.has_dsn());
        assert_eq!(cfg.postgres.max_connections.get(), 1);
    }

    #[test]
    fn jobs_operator_uses_the_shared_source_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = tempfile::tempdir().unwrap();
        let mut options = LoadOptions::default();
        let defaults = load_jobs_operator_from(&options, env(&[])).unwrap();
        assert_eq!(defaults.postgres.max_connections.get(), 4);

        options.config = Some(write(
            &dir,
            "base.toml",
            "[postgres]\nmax_connections = 5\n",
        ));
        let base = load_jobs_operator_from(&options, env(&[])).unwrap();
        assert_eq!(base.postgres.max_connections.get(), 5);
        for connections in [6, 7] {
            options.config_overlay.push(write(
                &dir,
                &format!("overlay-{connections}.toml"),
                &format!("[postgres]\nmax_connections = {connections}\n"),
            ));
            let overlay = load_jobs_operator_from(&options, env(&[])).unwrap();
            assert_eq!(overlay.postgres.max_connections.get(), connections);
        }
        write(&secrets, "APP__POSTGRES__MAX_CONNECTIONS", "8");
        write(&secrets, "APP__POSTGRES__DSN", "postgres://localhost/app");
        options.secrets_dir = Some(secrets.path().to_owned());
        let secret = load_jobs_operator_from(&options, env(&[])).unwrap();
        assert_eq!(secret.postgres.max_connections.get(), 8);
        assert!(secret.postgres.has_dsn());
        let environment = load_jobs_operator_from(
            &options,
            env(&[
                ("APP__POSTGRES__MAX_CONNECTIONS", "9"),
                ("APP__POSTGRES__DSN", ""),
            ]),
        )
        .unwrap();
        assert_eq!(environment.postgres.max_connections.get(), 9);
        assert!(!environment.postgres.has_dsn());
    }

    #[test]
    fn jobs_operator_keeps_postgres_validation_and_value_free_errors() {
        let options = LoadOptions::default();
        let unknown =
            load_jobs_operator_from(&options, env(&[("APP__POSTGRES__BOGUS", "1")])).unwrap_err();
        assert!(matches!(unknown, Error::Deserialize(_)), "{unknown}");
        for (name, value, key) in [
            (
                "APP__POSTGRES__MAX_CONNECTIONS",
                "501",
                "postgres.max_connections",
            ),
            ("APP__POSTGRES__ENABLED", "true", "postgres.dsn"),
        ] {
            let error = load_jobs_operator_from(&options, env(&[(name, value)])).unwrap_err();
            assert!(
                matches!(&error, Error::Validate(error) if error.key == key),
                "{error}"
            );
        }
        let secret = "fixture-private-value";
        let error = load_jobs_operator_from(&options, env(&[("APP__POSTGRES__ENABLED", secret)]))
            .unwrap_err();
        assert!(matches!(error, Error::Deserialize(_)), "{error}");
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }

    #[test]
    fn jobs_operator_prescans_unrelated_sections() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[unrelated]\nsecret = \"fixture-private-value\"\n",
        );
        let error = load_jobs_operator_from(
            &LoadOptions {
                config: Some(leaked),
                ..LoadOptions::default()
            },
            env(&[]),
        )
        .unwrap_err();
        assert!(matches!(error, Error::SecretInFile { .. }), "{error}");
        assert!(!error.to_string().contains("fixture-private-value"));

        let unaddressable = write(&dir, "unaddressable.toml", "[Unrelated]\nvalue = 1\n");
        let error = load_jobs_operator_from(
            &LoadOptions {
                config: Some(unaddressable),
                ..LoadOptions::default()
            },
            env(&[]),
        )
        .unwrap_err();
        assert!(matches!(error, Error::UnaddressableKey { .. }), "{error}");
        let malformed = load_jobs_operator_from(
            &LoadOptions::default(),
            env(&[("APP__UNRELATED__", "fixture-private-value")]),
        )
        .unwrap_err();
        assert!(
            matches!(malformed, Error::MalformedEnvName { .. }),
            "{malformed}"
        );
    }
    // template:end jobs:load-jobs-operator-tests

    #[test]
    fn rejected_environment_values_are_not_echoed() {
        // config-rs lowercases a refused boolean and parses a refused
        // number before quoting it, so no spelling may reach the message.
        for value in [
            "Hunter2 \"Quoted\"\nSecret",
            " hunter2 secret ",
            "+0055555555555",
            "x, expected hunter2",
        ] {
            for (name, key) in [
                ("APP__HTTP__MAX_IN_FLIGHT", "http.max_in_flight"),
                ("APP__HTTP__ADDR", "http.addr"),
                ("APP__HTTP__REQUEST_TIMEOUT", "http.request_timeout"),
                (
                    "APP__HTTP__ACCESS_LOG_HEALTH_PROBES",
                    "http.access_log_health_probes",
                ),
                ("APP__LOG__FORMAT", "log.format"),
                ("APP__HTTP", "http"),
            ] {
                let err =
                    load_from(&LoadOptions::default(), BUILD, env(&[(name, value)])).unwrap_err();
                let rendered = err.to_string();
                assert!(matches!(&err, Error::Deserialize(_)), "{name}: {rendered}");
                assert!(
                    rendered.contains(&format!("for key `{key}` in the environment")),
                    "{name}: {rendered}"
                );
                for text in [rendered, format!("{err:?}")] {
                    let text = text.to_lowercase();
                    for part in ["hunter2", "secret", "5555"] {
                        assert!(!text.contains(part), "{name}={value}: {text}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_decode_failure_that_names_only_keys_is_unchanged() {
        // The unknown field is reported even when a sibling's value happens
        // to be a word of the message.
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__LOG__LEVL", "level"), ("APP__LOG__LEVEL", "level")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field `levl`"), "{err}");

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__NOPE__X", "app")]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field `nope`"), "{err}");
    }

    #[test]
    fn rejected_environment_values_report_the_expected_form() {
        for (name, expected) in [
            ("APP__HTTP__MAX_IN_FLIGHT", "an integer"),
            ("APP__HTTP__REQUEST_TIMEOUT", "a duration"),
            ("APP__HTTP__ACCESS_LOG_HEALTH_PROBES", "a boolean"),
            ("APP__HTTP__ADDR", "an IP address and port"),
        ] {
            let err =
                load_from(&LoadOptions::default(), BUILD, env(&[(name, "maybe")])).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("invalid value, expected {expected}")),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn rejected_file_values_stay_visible() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(&dir, "bad.toml", "[log]\nformat = \"yaml\"\n");
        let err = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("yaml"), "{err}");
    }

    #[test]
    fn file_keys_the_environment_cannot_name_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        for (contents, expected) in [
            ("[HTTP]\naddr = \":1\"\n", "HTTP"),
            ("[http]\nMax_Body_Bytes = 1\n", "http.Max_Body_Bytes"),
            ("[observability.Otel.exporter]\n", "observability.Otel"),
            (
                "[observability.\"otel/exporter\"]\n",
                "observability.otel/exporter",
            ),
            ("[app]\n\"instance.id\" = \"a\"\n", "app.instance.id"),
            ("[app]\n\"instance__id\" = \"a\"\n", "app.instance__id"),
        ] {
            let file = write(&dir, "unaddressable.toml", contents);
            let err = load_from(
                &LoadOptions {
                    config: Some(file),
                    ..LoadOptions::default()
                },
                BUILD,
                env(&[]),
            )
            .unwrap_err();
            assert!(
                matches!(&err, Error::UnaddressableKey { key, .. } if key == expected),
                "{contents}: {err}"
            );
        }
    }

    #[test]
    fn precedence_is_defaults_then_files_in_order_then_env() {
        let dir = tempfile::tempdir().unwrap();
        let base = write(
            &dir,
            "base.toml",
            "[http]\naddr = \"127.0.0.1:1\"\nrequest_timeout = \"2s\"\n[log]\nlevel = \"debug\"\n",
        );
        let overlay = write(&dir, "overlay.toml", "[http]\naddr = \"127.0.0.1:2\"\n");
        let options = LoadOptions {
            config: Some(base),
            config_overlay: vec![overlay],
            ..LoadOptions::default()
        };
        let cfg = load_from(
            &options,
            BUILD,
            env(&[("APP__HTTP__ADDR", "127.0.0.1:3"), ("UNRELATED", "x")]),
        )
        .unwrap();
        assert_eq!(cfg.http.addr, "127.0.0.1:3".parse().unwrap(), "env wins");
        assert_eq!(
            cfg.http.request_timeout,
            Duration::from_secs(2),
            "base file survives"
        );
        assert_eq!(cfg.log.level, "debug");
    }

    #[test]
    fn env_parses_durations_sizes_numbers_and_enums() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__HTTP__REQUEST_TIMEOUT", "500ms"),
                ("APP__HTTP__MAX_BODY_BYTES", "2 MiB"),
                ("APP__HTTP__MAX_IN_FLIGHT", "12"),
                ("APP__HTTP__MAX_CONNECTION_AGE", "1h"),
                ("APP__HTTP__ACCESS_LOG_HEALTH_PROBES", "true"),
                ("APP__LOG__FORMAT", "text"),
                ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER", "always_on"),
                ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER_ARG", "0.5"),
            ]),
        )
        .unwrap();
        assert_eq!(cfg.http.request_timeout, Duration::from_millis(500));
        assert_eq!(cfg.http.max_body_bytes, bytesize::ByteSize::mib(2));
        assert_eq!(cfg.http.max_in_flight, 12);
        assert_eq!(cfg.http.connection_age(), Some(Duration::from_hours(1)));
        assert!(cfg.http.access_log_health_probes);
        assert_eq!(cfg.log.format, LogFormat::Text);
        assert_eq!(
            cfg.observability.otel.traces_sampler,
            crate::TracesSampler::AlwaysOn
        );
        assert!((cfg.observability.otel.traces_sampler_arg - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn unknown_keys_fail_from_files_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let file = write(&dir, "bad.toml", "[http]\naddress = \":1\"\n");
        let options = LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        };
        assert!(matches!(
            load_from(&options, BUILD, env(&[])),
            Err(Error::Deserialize(_))
        ));
        assert!(matches!(
            load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[("APP__HTTP__BOGUS", "1")])
            ),
            Err(Error::Deserialize(_))
        ));
        assert!(matches!(
            load_from(
                &LoadOptions::default(),
                BUILD,
                env(&[("APP__NOPE__X", "1")])
            ),
            Err(Error::Deserialize(_))
        ));
    }

    #[test]
    fn malformed_env_names_are_named() {
        for name in [
            "APP____ADDR",
            "APP__HTTP____ADDR",
            "APP__HTTP__ADDR__",
            "APP__",
            "APP__HTTP[0]",
            "APP__HTTP__ADDR[0]",
            "APP__HTTP.ADDR",
            "APP__HTTP__ADDR ",
        ] {
            let err = load_from(&LoadOptions::default(), BUILD, env(&[(name, "x")])).unwrap_err();
            assert!(
                matches!(&err, Error::MalformedEnvName { name: n } if n == name),
                "{name}: {err}"
            );
        }
    }

    #[test]
    fn empty_env_value_is_an_explicit_override() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__HTTP__ADDR", "")]),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Deserialize(_)), "{err}");
        assert!(err.to_string().contains("http.addr"), "{err}");
    }

    #[test]
    fn secrets_in_files_are_refused_and_empty_placeholders_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let leaked = write(
            &dir,
            "leaked.toml",
            "[observability.otel.exporter]\notlp_headers = \"authorization=x\"\n",
        );
        let options = LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        };
        let err = load_from(&options, BUILD, env(&[])).unwrap_err();
        assert!(
            matches!(&err, Error::SecretInFile { key, .. } if key == "observability.otel.exporter.otlp_headers"),
            "{err}"
        );

        let placeholder = write(
            &dir,
            "ok.toml",
            "[observability.otel.exporter]\notlp_headers = \"\"\n",
        );
        let options = LoadOptions {
            config: Some(placeholder),
            ..LoadOptions::default()
        };
        load_from(&options, BUILD, env(&[])).unwrap();
    }

    #[test]
    fn secret_from_env_is_accepted_and_redacted() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[(
                "APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS",
                "authorization=Bearer s3cr3t",
            )]),
        )
        .unwrap();
        assert!(cfg.observability.otel.exporter.has_headers());
        assert!(!format!("{cfg:?}").contains("s3cr3t"));
    }

    #[test]
    fn missing_file_is_reported() {
        let options = LoadOptions {
            config: Some(PathBuf::from("/definitely/missing.toml")),
            ..LoadOptions::default()
        };
        assert!(matches!(
            load_from(&options, BUILD, env(&[])),
            Err(Error::ReadFile { .. })
        ));
    }

    #[test]
    fn zero_counts_fail_to_deserialize() {
        let cases: &[(&str, &str)] = &[
            // template:begin postgres:load-zero-postgres-count
            ("APP__POSTGRES__MAX_CONNECTIONS", "postgres.max_connections"),
            // template:end postgres:load-zero-postgres-count
            // template:begin jobs:load-zero-jobs-count
            ("APP__JOBS__MAX_WORKERS", "jobs.max_workers"),
            // template:end jobs:load-zero-jobs-count
            // template:begin messaging:load-zero-messaging-count
            (
                "APP__MESSAGING__CONSUMER_CONCURRENCY",
                "messaging.consumer_concurrency",
            ),
            // template:end messaging:load-zero-messaging-count
        ];
        for &(name, key) in cases {
            let err = load_from(&LoadOptions::default(), BUILD, env(&[(name, "0")])).unwrap_err();
            assert!(matches!(err, Error::Deserialize(_)), "{name}: {err}");
            assert!(err.to_string().contains(key), "{name}: {err}");
        }
    }

    #[test]
    fn drain_timeout_and_probe_budget_load_from_env() {
        let canonical = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__HTTP__DRAIN_TIMEOUT", "20s"),
                ("APP__HTTP__REQUEST_TIMEOUT", "4s"),
                ("APP__HEALTH__PROBE_BUDGET", "3s"),
            ]),
        )
        .unwrap();
        assert_eq!(canonical.http.drain_timeout, Duration::from_secs(20));
        assert_eq!(canonical.health.probe_budget, Duration::from_secs(3));

        for (name, key) in [
            ("APP__HTTP__SHUTDOWN_TIMEOUT", "shutdown_timeout"),
            ("APP__HEALTH__READINESS_TIMEOUT", "readiness_timeout"),
        ] {
            let legacy =
                load_from(&LoadOptions::default(), BUILD, env(&[(name, "22s")])).unwrap_err();
            assert!(matches!(legacy, Error::Deserialize(_)), "{legacy}");
            assert!(legacy.to_string().contains(key), "{legacy}");
        }
    }

    fn with_secrets(dir: &tempfile::TempDir) -> LoadOptions {
        LoadOptions {
            secrets_dir: Some(dir.path().to_owned()),
            ..LoadOptions::default()
        }
    }

    #[test]
    fn secrets_directory_supplies_variables_and_the_environment_overrides_it() {
        use secrecy::ExposeSecret as _;

        let dir = tempfile::tempdir().unwrap();
        write(
            &dir,
            "APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS",
            " authorization=Bearer s3cr3t \r\n\n",
        );
        write(&dir, "APP__HTTP__ADDR", "127.0.0.1:7\n");
        write(&dir, "APP__LOG__LEVEL", "debug");
        // Entries outside the namespace are not variables.
        write(&dir, "README", "not a variable");
        write(&dir, ".APP__LOG__FORMAT", "yaml");
        std::fs::create_dir(dir.path().join("..data")).unwrap();

        let cfg = load_from(
            &with_secrets(&dir),
            BUILD,
            env(&[("APP__HTTP__ADDR", "127.0.0.1:9")]),
        )
        .unwrap();
        assert_eq!(
            cfg.observability
                .otel
                .exporter
                .otlp_headers
                .as_ref()
                .unwrap()
                .expose_secret(),
            " authorization=Bearer s3cr3t ",
            "only trailing line breaks are removed"
        );
        assert_eq!(cfg.http.addr, "127.0.0.1:9".parse().unwrap(), "env wins");
        assert_eq!(cfg.log.level, "debug");
        assert!(!format!("{cfg:?}").contains("s3cr3t"));
    }

    #[cfg(unix)]
    #[test]
    fn secrets_directory_follows_the_links_of_a_mounted_volume() {
        // A Kubernetes volume keeps the files under `..data` and links each
        // name to it, so an update swaps one link.
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("..data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("APP__LOG__LEVEL"), "warn\n").unwrap();
        std::os::unix::fs::symlink("..data/APP__LOG__LEVEL", dir.path().join("APP__LOG__LEVEL"))
            .unwrap();

        let cfg = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap();
        assert_eq!(cfg.log.level, "warn");
    }

    #[test]
    fn rejected_secret_file_values_are_not_echoed() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "APP__HTTP__MAX_IN_FLIGHT", "hunter2");
        let err = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap_err();
        let rendered = err.to_string();
        assert!(
            rendered.contains(
                "expected an integer for key `http.max_in_flight` in the environment or the secrets directory"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(!format!("{err:?}").contains("hunter2"), "{err:?}");
    }

    #[test]
    fn secrets_directory_failures_name_the_path_and_no_content() {
        let missing = LoadOptions {
            secrets_dir: Some(PathBuf::from("/definitely/missing-secrets")),
            ..LoadOptions::default()
        };
        assert!(matches!(
            load_from(&missing, BUILD, env(&[])),
            Err(Error::ReadSecretsDir { .. })
        ));

        for name in ["APP__HTTP.ADDR", "APP__HTTP__ADDR__", "APP__"] {
            let dir = tempfile::tempdir().unwrap();
            let file = write(&dir, name, "127.0.0.1:7");
            let err = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap_err();
            assert!(
                matches!(&err, Error::MalformedSecretName { path } if *path == file),
                "{name}: {err}"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("APP__HTTP__ADDR")).unwrap();
        assert!(matches!(
            load_from(&with_secrets(&dir), BUILD, env(&[])),
            Err(Error::ReadSecret { .. })
        ));

        // Not Unicode, and a NUL byte, which would let a value forge the
        // mark of a value-free message.
        for content in [&b"hunter2\xff"[..], &b"\0hunter2"[..]] {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("APP__LOG__LEVEL");
            std::fs::write(&file, content).unwrap();
            let err = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap_err();
            assert!(
                matches!(&err, Error::ReadSecret { path, .. } if *path == file),
                "{err}"
            );
            assert!(!err.to_string().contains("hunter2"), "{err}");
        }
    }

    #[test]
    fn two_spellings_of_one_variable_are_refused() {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__HTTP__ADDR", "127.0.0.1:1"),
                ("APP__http__addr", "127.0.0.1:2"),
            ]),
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::AmbiguousName { name, other }
                if name == "APP__http__addr" && other == "APP__HTTP__ADDR"),
            "{err}"
        );
    }

    #[test]
    fn the_environment_overrides_a_secret_file_spelled_in_another_case() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir, "APP__log__level", "debug");
        let cfg = load_from(
            &with_secrets(&dir),
            BUILD,
            env(&[("APP__LOG__LEVEL", "warn")]),
        )
        .unwrap();
        assert_eq!(cfg.log.level, "warn");
    }

    #[cfg(unix)]
    #[test]
    fn variables_that_are_not_unicode_are_refused() {
        use std::os::unix::ffi::OsStringExt as _;

        let not_unicode = |prefix: &str| {
            let mut bytes = prefix.as_bytes().to_vec();
            bytes.push(0xff);
            OsString::from_vec(bytes)
        };
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            [(OsString::from("APP__LOG__LEVEL"), not_unicode("hunter2"))],
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::NonUnicodeEnvValue { name } if name == "APP__LOG__LEVEL"),
            "{err}"
        );
        assert!(!err.to_string().contains("hunter2"), "{err}");

        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            [(not_unicode("APP__LOG__LEVEL"), OsString::from("info"))],
        )
        .unwrap_err();
        assert!(matches!(err, Error::MalformedEnvName { .. }), "{err}");

        // A name outside the namespace is not read at all.
        load_from(
            &LoadOptions::default(),
            BUILD,
            [(not_unicode("UNRELATED"), not_unicode("value"))],
        )
        .unwrap();
    }

    #[test]
    fn typed_version_and_commit_override_build_info() {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__APP__VERSION", "9.9.9"),
                ("APP__APP__COMMIT", "deadbeef"),
            ]),
        )
        .unwrap();
        assert_eq!(cfg.app.version, "9.9.9");
        assert_eq!(cfg.app.commit, "deadbeef");
    }
}
