//! Build the snapshot from defaults, files, and the `APP__` environment.

use std::path::{Path, PathBuf};

use crate::app::BuildInfo;
use crate::secret_policy::first_secret_like_key;
use crate::{Config, LoadOptions, ValidationError};

/// Environment namespace. `APP__HTTP__ADDR` sets `http.addr`.
pub const ENV_PREFIX: &str = "APP";
const ENV_SEPARATOR: &str = "__";

/// Configuration files larger than this are refused before parsing.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "environment variable {name} is malformed: empty segment between `{ENV_SEPARATOR}` separators"
    )]
    MalformedEnvName { name: String },
    #[error("config file {}: {source}", path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config file {} is {size} bytes, above the {MAX_FILE_BYTES} byte limit", path.display())]
    FileTooLarge { path: PathBuf, size: u64 },
    #[error("config file {}: {source}", path.display())]
    ParseFile {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("secret-like key `{key}` carries a value in config file {}; secrets come only from the environment", path.display())]
    SecretInFile { key: String, path: PathBuf },
    #[error("load configuration: {0}")]
    Merge(#[source] config::ConfigError),
    #[error("configuration is invalid: {0}")]
    Deserialize(#[source] config::ConfigError),
    #[error("configuration is invalid: {0}")]
    Validate(#[from] ValidationError),
}

/// Load, merge, and validate the snapshot.
///
/// # Errors
///
/// Fails on a malformed `APP__` variable name, an unreadable, oversized, or
/// unparsable file, a non-empty secret-like value in a file, an unknown key
/// anywhere, or a violated validation rule.
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
    K: Into<std::ffi::OsString>,
    V: Into<std::ffi::OsString>,
{
    let namespace = collect_namespace(environment)?;

    let mut builder = config::Config::builder();
    for path in options.files() {
        reject_file_secrets(path)?;
        builder = builder.add_source(
            config::File::from(path.clone())
                .format(config::FileFormat::Toml)
                .required(true),
        );
    }
    let env_source = config::Environment::with_prefix(ENV_PREFIX)
        .prefix_separator(ENV_SEPARATOR)
        .separator(ENV_SEPARATOR)
        // An empty value is still an explicit override; validation decides.
        .ignore_empty(false)
        .source(Some(namespace));
    let merged = builder
        .add_source(env_source)
        .build()
        .map_err(Error::Merge)?;

    let mut snapshot: Config = merged.try_deserialize().map_err(Error::Deserialize)?;
    snapshot.app.apply_build_info(build);
    snapshot.validate()?;
    Ok(snapshot)
}

/// Keep only `APP__*` variables, and refuse names config-rs would otherwise
/// report as an unknown field with an empty name.
fn collect_namespace<I, K, V>(environment: I) -> Result<config::Map<String, String>, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<std::ffi::OsString>,
    V: Into<std::ffi::OsString>,
{
    let full_prefix = format!("{ENV_PREFIX}{ENV_SEPARATOR}");
    let mut namespace = config::Map::new();
    for (key, value) in environment {
        let key = key.into();
        let Some(name) = key.to_str() else { continue };
        let Some(path) = name.strip_prefix(&full_prefix) else {
            continue;
        };
        if path.is_empty() || path.split(ENV_SEPARATOR).any(str::is_empty) {
            return Err(Error::MalformedEnvName {
                name: name.to_owned(),
            });
        }
        let value = value.into();
        let value = value.to_string_lossy().into_owned();
        namespace.insert(name.to_owned(), value);
    }
    Ok(namespace)
}

fn reject_file_secrets(path: &Path) -> Result<(), Error> {
    let size = std::fs::metadata(path)
        .map_err(|source| Error::ReadFile {
            path: path.to_owned(),
            source,
        })?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(Error::FileTooLarge {
            path: path.to_owned(),
            size,
        });
    }
    let text = std::fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.to_owned(),
        source,
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|source| Error::ParseFile {
        path: path.to_owned(),
        source,
    })?;
    if let Some(key) = first_secret_like_key(&table) {
        return Err(Error::SecretInFile {
            key,
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use super::*;
    use crate::LogFormat;

    const BUILD: BuildInfo = BuildInfo {
        version: "1.2.3",
        commit: "abc123",
    };

    fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
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
        assert_eq!(cfg.http.addr, ":8080");
        assert_eq!(cfg.app.version, "1.2.3");
        assert_eq!(cfg.app.commit, "abc123");
        assert_eq!(cfg.log.format, LogFormat::Json);
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
        };
        let cfg = load_from(
            &options,
            BUILD,
            env(&[("APP__HTTP__ADDR", "127.0.0.1:3"), ("UNRELATED", "x")]),
        )
        .unwrap();
        assert_eq!(cfg.http.addr, "127.0.0.1:3", "env wins");
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
        assert!(
            matches!(err, Error::Validate(ref v) if v.key == "http.addr"),
            "{err}"
        );
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
    fn missing_and_oversized_files_are_reported() {
        let options = LoadOptions {
            config: Some(PathBuf::from("/definitely/missing.toml")),
            ..LoadOptions::default()
        };
        assert!(matches!(
            load_from(&options, BUILD, env(&[])),
            Err(Error::ReadFile { .. })
        ));

        let dir = tempfile::tempdir().unwrap();
        let big = write(
            &dir,
            "big.toml",
            &format!(
                "# {}\n",
                "x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap())
            ),
        );
        let options = LoadOptions {
            config: Some(big),
            ..LoadOptions::default()
        };
        assert!(matches!(
            load_from(&options, BUILD, env(&[])),
            Err(Error::FileTooLarge { .. })
        ));
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
