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
        // `Environment::source` keeps keys that still carry the `APP__` prefix;
        // inserting the stripped path would make every override disappear.
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
        assert_eq!(cfg.http.addr, ":8080");
        assert_eq!(cfg.app.version, "1.2.3");
        assert_eq!(cfg.app.commit, "abc123");
        assert_eq!(cfg.log.format, LogFormat::Json);
        assert_eq!(cfg.app.instance_id, None);
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
                // template:end postgres:load-empty-dsn-env
                ("APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS", "  "),
                ("APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT", "  "),
            ]),
        )
        .unwrap();
        // template:begin postgres:load-empty-dsn-assertion
        assert!(!cfg.postgres.has_dsn());
        // template:end postgres:load-empty-dsn-assertion
        assert!(!cfg.observability.otel.exporter.has_headers());
        assert_eq!(cfg.observability.otel.exporter.otlp_endpoint, None);
    }

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
        assert_eq!(cfg.jobs.max_workers, 8);
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
            ]),
        )
        .unwrap();
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
        }
    }
    // template:end webhooks:load-webhooks-environment

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
            ("APP__AUTHN__CACHE_CAPACITY", "0"),
            ("APP__AUTHN__CACHE_CAPACITY", "1025"),
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
    fn jwt_environment_preserves_a_supplied_token_profile() {
        let jwt = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__AUTHN__MODE", "oidc-jwt"),
                ("APP__AUTHN__ISSUER", "https://issuer.example/tenant"),
                ("APP__AUTHN__AUDIENCE", "service"),
                ("APP__AUTHN__TOKEN_PROFILE", "rfc9068"),
            ]),
        )
        .unwrap();
        let AuthnConfig::OidcJwt { token_profile, .. } = jwt.authn else {
            panic!("expected OIDC JWT configuration");
        };
        assert_eq!(token_profile, TokenProfile::Rfc9068);
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
    fn drain_timeout_and_probe_budget_accept_legacy_keys() {
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

        let legacy = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__HTTP__SHUTDOWN_TIMEOUT", "22s"),
                ("APP__HTTP__REQUEST_TIMEOUT", "4s"),
                ("APP__HEALTH__READINESS_TIMEOUT", "5s"),
            ]),
        )
        .unwrap();
        assert_eq!(legacy.http.drain_timeout, Duration::from_secs(22));
        assert_eq!(legacy.health.probe_budget, Duration::from_secs(5));
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
