use std::assert_matches;
use std::time::Duration;

use super::*;
use crate::LogFormat;
use crate::test_support::{BUILD, env, load_env, write};

#[rstest::rstest]
#[case::word("partner", true)]
#[case::underscore("partner_v2", true)]
#[case::hyphen("partner-v2", true)]
#[case::leading_underscore("_partner", true)]
#[case::path_characters("partner/a", false)]
#[case::dot("partner.a", false)]
#[case::non_ascii("pärtner", false)]
#[case::empty("", false)]
#[case::uppercase("Partner", false)]
#[case::segment_separator("partner__v2", false)]
#[case::trailing_underscore("partner_", false)]
fn env_addressable_names(#[case] name: &str, #[case] addressable: bool) {
    assert_eq!(is_env_addressable(name), addressable);
}

#[test]
fn defaults_alone_produce_a_valid_snapshot() {
    let cfg = load_env(&[]);
    assert_eq!(cfg.app.env, "unspecified");
    assert!(!cfg.app.is_local_development());
    assert_eq!(cfg.http.listener().addr, "0.0.0.0:8080".parse().unwrap());
    assert_eq!(cfg.app.version, "1.2.3");
    assert_eq!(cfg.app.commit, "abc123");
    assert_eq!(cfg.log.format, LogFormat::Json);
    assert_eq!(cfg.app.instance_id, None);
    assert_eq!(cfg.runtime.worker_threads, None);
}

#[test]
fn runtime_workers_load_from_the_environment_and_zero_names_its_key() {
    let cfg = load_env(&[("APP__RUNTIME__WORKER_THREADS", "4")]);
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
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    assert!(err.to_string().contains("runtime.worker_threads"), "{err}");

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__RUNTIME__WORKER_THREADS", "513")]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Validate(errors) if errors.names("runtime.worker_threads"), "{err}");
}

#[test]
fn log_filter_keeps_spaces_in_a_span_field_value() {
    let filter = "[postgres_statement{db.query.summary=complete jobs}]=debug";
    let config = load_env(&[("APP__LOG__LEVEL", filter)]);
    assert_eq!(config.log.level, filter);
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
    assert!(cfg.app.is_local_development());
}

#[test]
fn insecure_development_refuses_unrecognized_environment_names() {
    for name in [
        "production",
        "dev",
        "Development",
        "private-environment-value",
    ] {
        let error = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__APP__ENV", name),
                ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ]),
        )
        .unwrap_err();
        assert_matches!(&error, Error::Validate(errors) if errors.names("app.allow_insecure_development"));
        assert!(!error.to_string().contains("private-environment-value"));
    }
}

#[test]
fn empty_instance_id_is_vacant_none() {
    let empty = load_env(&[("APP__APP__INSTANCE_ID", "")]);
    assert_eq!(empty.app.instance_id, None);
    let set = load_env(&[("APP__APP__INSTANCE_ID", "  pod-a  ")]);
    assert_eq!(set.app.instance_id.as_deref(), Some("pod-a"));
}

#[test]
fn empty_dsn_and_otlp_headers_are_vacant_none() {
    let cfg = load_env(&[
        // template:begin postgres:load-empty-dsn-env
        ("APP__POSTGRES__DSN", ""),
        ("APP__POSTGRES__PASSWORD_FILE", "  "),
        // template:end postgres:load-empty-dsn-env
        ("APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS", "  "),
        ("APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT", "  "),
    ]);
    // template:begin postgres:load-empty-dsn-assertion
    assert!(cfg.postgres.is_none());
    // template:end postgres:load-empty-dsn-assertion
    assert!(cfg.observability.otel.exporter.otlp_headers.is_none());
    assert_eq!(cfg.observability.otel.exporter.otlp_endpoint, None);
}

#[test]
fn rejected_environment_values_are_not_echoed() {
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
            let err = load_from(&LoadOptions::default(), BUILD, env(&[(name, value)])).unwrap_err();
            let rendered = err.to_string();
            assert_matches!(&err, Error::Deserialize(_), "{name}: {rendered}");
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
        let err = load_from(&LoadOptions::default(), BUILD, env(&[(name, "maybe")])).unwrap_err();
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
        assert_matches!(&err, Error::UnaddressableKey { key, .. } if key == expected, "{contents}: {err}");
    }
}

#[test]
fn malformed_toml_reports_its_position_without_echoing_input() {
    let dir = tempfile::tempdir().unwrap();
    let options = LoadOptions {
        config: Some(write(
            &dir,
            "invalid.toml",
            "[app]\nenv = \"fixture-value\" trailing\n",
        )),
        ..LoadOptions::default()
    };
    let error = load_from(&options, BUILD, env(&[])).unwrap_err();
    let message = error.to_string();

    assert_matches!(&error, Error::ParseFile { .. });
    assert!(message.contains("invalid.toml: invalid TOML at byte"));
    assert!(!message.contains("fixture-value"));
    assert!(!format!("{error:?}").contains("fixture-value"));
}

#[test]
fn independent_input_failures_are_reported_together() {
    let dir = tempfile::tempdir().unwrap();
    let options = LoadOptions {
        config: Some(write(&dir, "base.toml", "key = \"base-private\" trailing")),
        config_overlay: vec![
            dir.path().join("missing-overlay.toml"),
            write(
                &dir,
                "broken-overlay.toml",
                "key = \"overlay-private\" trailing",
            ),
        ],
        secrets_dir: Some(dir.path().join("missing-secrets")),
    };
    let err = load_from(&options, BUILD, env(&[])).unwrap_err();
    assert_matches!(&err, Error::InputErrors(errors) if errors.len() == 4, "{err}");
    let message = err.to_string();
    for rejected in [
        "missing-secrets",
        "base.toml: invalid TOML at byte",
        "missing-overlay.toml",
        "broken-overlay.toml: invalid TOML at byte",
    ] {
        assert!(message.contains(rejected), "{message}");
    }
    for value in ["base-private", "overlay-private"] {
        assert!(!message.contains(value), "{message}");
        assert!(!format!("{err:?}").contains(value));
    }
}

#[test]
fn utf8_bom_loads_once_and_rejects_a_second_bom() {
    let dir = tempfile::tempdir().unwrap();
    let single = LoadOptions {
        config: Some(write(
            &dir,
            "bom.toml",
            "\u{feff}[log]\nlevel = \"debug\"\n",
        )),
        ..LoadOptions::default()
    };
    let cfg = load_from(&single, BUILD, env(&[])).unwrap();
    assert_eq!(cfg.log.level, "debug");

    let double_path = write(
        &dir,
        "double-bom.toml",
        "\u{feff}\u{feff}[log]\nlevel = \"debug\"\n",
    );
    let double = LoadOptions {
        config: Some(double_path.clone()),
        ..LoadOptions::default()
    };
    let error = load_from(&double, BUILD, env(&[])).unwrap_err();
    assert_matches!(&error, Error::ParseFile { path, .. } if path == &double_path, "{error}");
    assert!(
        error
            .to_string()
            .trim_end()
            .ends_with("double-bom.toml: invalid TOML at byte 6"),
        "{error}"
    );
}

#[test]
fn a_replaced_file_does_not_change_the_admitted_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(&dir, "base.toml", "[log]\nlevel = \"debug\"\n");
    let source = scan_file(&path).unwrap();
    write(&dir, "base.toml", "[log]\nlevel = \"info\"\n");

    let merged = config::Config::builder()
        .add_source(source)
        .build()
        .unwrap();
    assert_eq!(merged.get_string("log.level").unwrap(), "debug");
}

#[rstest::rstest]
#[case::http_requests("http", "max_in_flight", "-1.0")]
#[case::http_connections("http", "max_connections", "false")]
#[case::readiness("health", "failure_threshold", "0.7")]
#[case::runtime_workers("runtime", "worker_threads", "true")]
fn configuration_counts_reject_non_integer_toml(
    #[case] section: &str,
    #[case] key: &str,
    #[case] input: &str,
) {
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "bad.toml", &format!("[{section}]\n{key} = {input}\n"));
    let err = load_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    assert!(
        err.to_string().contains(&format!("{section}.{key}")),
        "{err}"
    );
}

#[test]
fn only_numeric_zero_disables_the_http_in_flight_cap() {
    let options = LoadOptions::default();
    let err = load_from(
        &options,
        BUILD,
        env(&[("APP__HTTP__MAX_IN_FLIGHT", "false")]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    assert!(err.to_string().contains("http.max_in_flight"), "{err}");

    let cfg = load_from(&options, BUILD, env(&[("APP__HTTP__MAX_IN_FLIGHT", "0")])).unwrap();
    assert_eq!(cfg.http.listener().max_in_flight, None);
}

#[test]
fn http_can_limit_connections_below_concurrent_requests() {
    let cfg = load_env(&[
        ("APP__HTTP__MAX_CONNECTIONS", "4"),
        ("APP__HTTP__MAX_IN_FLIGHT", "32"),
    ]);
    assert_eq!(
        cfg.http
            .listener()
            .max_connections
            .map(std::num::NonZeroU32::get),
        Some(4)
    );
    assert_eq!(
        cfg.http
            .listener()
            .max_in_flight
            .map(std::num::NonZeroU32::get),
        Some(32)
    );
}

// template:begin grpc:config-test-finite-listener-connection-caps
#[rstest::rstest]
#[case::all_listeners("4", ":9090", "true", "7", 15)]
#[case::disabled_diagnostics("4", "", "true", "7", 11)]
#[case::disabled_grpc("4", ":9090", "false", "7", 8)]
#[case::uncapped_http("0", ":9090", "true", "7", 7)]
#[case::uncapped_grpc("4", ":9090", "true", "0", 8)]
fn service_counts_finite_caps_of_enabled_listeners(
    #[case] http_max_connections: &str,
    #[case] diagnostics_addr: &str,
    #[case] grpc_enabled: &str,
    #[case] grpc_max_connections: &str,
    #[case] expected: u64,
) {
    let cfg = load_env(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__HTTP__MAX_CONNECTIONS", http_max_connections),
        ("APP__OBSERVABILITY__METRICS__ADDR", diagnostics_addr),
        ("APP__GRPC__ENABLED", grpc_enabled),
        ("APP__GRPC__ADDR", "127.0.0.1:0"),
        ("APP__GRPC__SECURITY", "plaintext"),
        ("APP__GRPC__MAX_CONNECTIONS", grpc_max_connections),
    ]);
    assert_eq!(cfg.finite_listener_connection_caps(), expected);
}
// template:end grpc:config-test-finite-listener-connection-caps

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
    assert_eq!(
        cfg.http.listener().addr,
        "127.0.0.1:3".parse().unwrap(),
        "env wins"
    );
    assert_eq!(
        cfg.http.listener().request_timeout,
        Duration::from_secs(2),
        "base file survives"
    );
    assert_eq!(cfg.log.level, "debug");
}

#[test]
fn an_overlay_cannot_erase_a_table_before_unknown_keys_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let base = write(&dir, "base.toml", "[http]\nrequest_timout = \"10s\"");
    let replace = write(&dir, "replace.toml", "http = \"temporary\"");
    let restore = write(&dir, "restore.toml", "[http]\naddr = \":8080\"");
    let options = LoadOptions {
        config: Some(base),
        config_overlay: vec![replace.clone(), restore],
        ..LoadOptions::default()
    };

    let err = load_from(&options, BUILD, env(&[]))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cannot replace configuration table `http`"),
        "{err}"
    );
    assert!(err.contains(&*replace.to_string_lossy()), "{err}");
}

#[test]
fn an_overlay_cannot_hide_keys_inside_arrays() {
    let dir = tempfile::tempdir().unwrap();
    let overlay = write(&dir, "overlay.toml", "[messaging]\nurls = []");
    for (contents, expected) in [
        (
            "[messaging]\nurls = [{ urll = 'example' }]",
            "messaging.urls[0].urll",
        ),
        (
            "[messaging]\nurls = [[], [{ urll = 'example' }]]",
            "messaging.urls[1][0].urll",
        ),
    ] {
        let base = write(&dir, "base.toml", contents);
        let options = LoadOptions {
            config: Some(base.clone()),
            config_overlay: vec![overlay.clone()],
            ..LoadOptions::default()
        };

        let err = load_from(&options, BUILD, env(&[])).unwrap_err();
        assert_matches!(&err, Error::UnaddressableKey { key, path } if key == expected && path == &base, "{err}");
    }
}

#[test]
fn env_parses_durations_sizes_numbers_and_enums() {
    let cfg = load_env(&[
        ("APP__HTTP__REQUEST_TIMEOUT", "500ms"),
        ("APP__HTTP__MAX_BODY_BYTES", "2 MiB"),
        ("APP__HTTP__MAX_IN_FLIGHT", "12"),
        ("APP__HTTP__MAX_CONNECTION_AGE", "1h"),
        ("APP__HTTP__ACCESS_LOG_HEALTH_PROBES", "true"),
        ("APP__LOG__FORMAT", "text"),
        ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER", "always_on"),
        ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER_ARG", "0.5"),
    ]);
    let http = cfg.http.listener();
    assert_eq!(http.request_timeout, Duration::from_millis(500));
    assert_eq!(http.max_body_bytes, bytesize::ByteSize::mib(2));
    assert_eq!(http.max_in_flight.map(std::num::NonZeroU32::get), Some(12));
    assert_eq!(http.max_connection_age, Some(Duration::from_hours(1)));
    assert!(http.access_log_health_probes);
    assert_eq!(cfg.log.format, LogFormat::Text);
    assert_eq!(
        cfg.observability.otel.traces_sampler,
        crate::TracesSampler::AlwaysOn
    );
    assert!((cfg.observability.otel.traces_sampler_arg - 0.5).abs() < f64::EPSILON);
}

#[test]
fn sampler_ratio_refuses_booleans_and_words() {
    for value in ["true", "yes", "on", "false", "no", "off"] {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER_ARG", value)]),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("expected a number"),
            "{value}: {err}"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    for (name, body, accepted) in [
        ("bool.toml", "traces_sampler_arg = true", None),
        ("word.toml", "traces_sampler_arg = \"off\"", None),
        ("integer.toml", "traces_sampler_arg = 1", Some(1.0)),
        ("float.toml", "traces_sampler_arg = 0.25", Some(0.25)),
    ] {
        let file = write(&dir, name, &format!("[observability.otel]\n{body}\n"));
        let loaded = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        );
        match accepted {
            Some(expected) => {
                let arg = loaded.unwrap().observability.otel.traces_sampler_arg;
                assert!((arg - expected).abs() < f64::EPSILON, "{name}: {arg}");
            }
            None => assert!(
                loaded
                    .unwrap_err()
                    .to_string()
                    .contains("expected a number"),
                "{name}"
            ),
        }
    }
}

#[test]
fn unknown_keys_fail_from_files_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "bad.toml", "[http]\naddress = \":1\"\n");
    let options = LoadOptions {
        config: Some(file),
        ..LoadOptions::default()
    };
    assert_matches!(
        load_from(&options, BUILD, env(&[])),
        Err(Error::Deserialize(_))
    );
    assert_matches!(
        load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__HTTP__BOGUS", "1")])
        ),
        Err(Error::Deserialize(_))
    );
    assert_matches!(
        load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__NOPE__X", "1")])
        ),
        Err(Error::Deserialize(_))
    );
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
        assert_matches!(&err, Error::MalformedEnvName { name: n } if n == name, "{name}: {err}");
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
    assert_matches!(err, Error::Deserialize(_), "{err}");
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
    assert_matches!(&err, Error::SecretInFile { key, .. } if key == "observability.otel.exporter.otlp_headers", "{err}");

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
    let cfg = load_env(&[(
        "APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS",
        "authorization=Bearer s3cr3t",
    )]);
    assert!(cfg.observability.otel.exporter.otlp_headers.is_some());
    assert!(!format!("{cfg:?}").contains("s3cr3t"));
}

#[test]
fn missing_file_is_reported() {
    let options = LoadOptions {
        config: Some(PathBuf::from("/definitely/missing.toml")),
        ..LoadOptions::default()
    };
    assert_matches!(
        load_from(&options, BUILD, env(&[])),
        Err(Error::ReadFile { .. })
    );
}

#[test]
fn nonzero_counts_reject_zero_and_boolean_text() {
    let cases: &[(&str, &str)] = &[
        ("APP__HEALTH__FAILURE_THRESHOLD", "health.failure_threshold"),
        // template:begin postgres:load-zero-postgres-count
        ("APP__POSTGRES__MAX_CONNECTIONS", "postgres.max_connections"),
        // template:end postgres:load-zero-postgres-count
    ];
    for &(name, key) in cases {
        for input in ["0", "yes"] {
            let err = load_from(&LoadOptions::default(), BUILD, env(&[(name, input)])).unwrap_err();
            assert_matches!(err, Error::Deserialize(_), "{name}: {err}");
            assert!(err.to_string().contains(key), "{name}: {err}");
        }
    }
    // template:begin jobs:load-zero-jobs-count
    for input in ["0", "yes"] {
        let err = load_worker_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__JOBS__MAX_WORKERS", input)]),
        )
        .unwrap_err();
        assert_matches!(err, Error::Deserialize(_), "{err}");
        assert!(err.to_string().contains("jobs.max_workers"), "{err}");
    }
    // template:end jobs:load-zero-jobs-count
}

// template:begin worker:load-worker-defaults
#[test]
fn worker_defaults_load_with_build_info() {
    let cfg = load_worker_from(&LoadOptions::default(), BUILD, env(&[])).unwrap();
    assert_eq!(cfg.app.version, "1.2.3");
    assert_eq!(cfg.app.commit, "abc123");
}
// template:end worker:load-worker-defaults

#[test]
fn drain_timeout_and_probe_budget_load_from_env() {
    let canonical = load_env(&[
        ("APP__HTTP__DRAIN_TIMEOUT", "20s"),
        ("APP__HTTP__REQUEST_TIMEOUT", "4s"),
        ("APP__HEALTH__PROBE_BUDGET", "3s"),
    ]);
    assert_eq!(canonical.http.drain_timeout, Duration::from_secs(20));
    assert_eq!(canonical.health.probe_budget, Duration::from_secs(3));

    for (name, key) in [
        ("APP__HTTP__SHUTDOWN_TIMEOUT", "shutdown_timeout"),
        ("APP__HEALTH__READINESS_TIMEOUT", "readiness_timeout"),
    ] {
        let legacy = load_from(&LoadOptions::default(), BUILD, env(&[(name, "22s")])).unwrap_err();
        assert_matches!(legacy, Error::Deserialize(_), "{legacy}");
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
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
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
    assert_eq!(
        cfg.http.listener().addr,
        "127.0.0.1:9".parse().unwrap(),
        "env wins"
    );
    assert_eq!(cfg.log.level, "debug");
    assert!(!format!("{cfg:?}").contains("s3cr3t"));
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn secrets_directory_follows_the_links_of_a_mounted_volume() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("..data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("APP__LOG__LEVEL"), "warn\n").unwrap();
    std::os::unix::fs::symlink("..data/APP__LOG__LEVEL", dir.path().join("APP__LOG__LEVEL"))
        .unwrap();

    let cfg = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap();
    assert_eq!(cfg.log.level, "warn");
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the fixture publishes a new generation before updating the public links"
)]
fn projected_secrets_use_the_active_generation_even_before_its_public_links_are_updated() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let old = "..2026_11_18_01_00_00.10000001";
    let next = "..2026_11_18_01_00_01.10000002";
    for generation in [old, next] {
        std::fs::create_dir(dir.path().join(generation)).unwrap();
    }
    for (generation, key, value) in [
        (old, "APP__LOG__LEVEL", "warn"),
        (old, "APP__HTTP__ADDR", "127.0.0.1:7"),
        (next, "APP__LOG__LEVEL", "debug"),
        (next, "APP__LOG__FORMAT", "text"),
    ] {
        std::fs::write(dir.path().join(generation).join(key), value).unwrap();
    }
    let active = dir.path().join("..data");
    symlink(old, &active).unwrap();
    for key in ["APP__LOG__LEVEL", "APP__HTTP__ADDR"] {
        symlink(Path::new("..data").join(key), dir.path().join(key)).unwrap();
    }
    let pending = dir.path().join("..data_tmp");
    symlink(next, &pending).unwrap();
    std::fs::rename(&pending, &active).unwrap();

    let cfg = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap();
    assert_eq!(cfg.log.level, "debug");
    assert_eq!(cfg.log.format, LogFormat::Text);
    assert_eq!(cfg.http.listener().addr, "0.0.0.0:8080".parse().unwrap());
}

#[cfg(unix)]
#[test]
fn secrets_directory_refuses_a_symlink_to_a_fifo_without_waiting_for_a_writer() {
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let fifo = dir.path().join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let file = dir.path().join("APP__LOG__LEVEL");
    std::os::unix::fs::symlink(&fifo, &file).unwrap();
    let options = with_secrets(&dir);
    let (sender, receiver) = mpsc::channel();
    let loader = std::thread::spawn(move || {
        sender.send(load_from(&options, BUILD, env(&[]))).unwrap();
    });
    let err = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("secret loading must return without opening a blocking FIFO")
        .unwrap_err();
    loader.join().unwrap();
    assert_matches!(&err, Error::ReadSecret { path, error }
        if *path == file && error.kind() == std::io::ErrorKind::InvalidInput, "{err}");
}

#[test]
fn secrets_directory_accepts_one_mib_and_refuses_larger_files() {
    let dir = tempfile::tempdir().unwrap();
    let mut value = "warn".to_owned();
    value.extend(std::iter::repeat_n('\n', 1024 * 1024 - value.len()));
    let options = with_secrets(&dir);
    write(&dir, "APP__LOG__LEVEL", &value);
    let cfg = load_from(&options, BUILD, env(&[])).unwrap();
    assert_eq!(cfg.log.level, "warn");

    value.push('\n');
    let file = write(&dir, "APP__LOG__LEVEL", &value);
    let err = load_from(&options, BUILD, env(&[])).unwrap_err();
    assert_matches!(&err, Error::ReadSecret { path, error }
        if *path == file && error.kind() == std::io::ErrorKind::InvalidData, "{err}");
    assert!(err.to_string().contains("1 MiB secret file limit"), "{err}");
}

#[test]
fn rejected_secret_file_values_are_not_echoed() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "APP__HTTP__MAX_IN_FLIGHT", "hunter2");
    let err = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap_err();
    let rendered = err.to_string();
    assert!(
        rendered
            .contains("expected an integer for key `http.max_in_flight` in the secrets directory"),
        "{rendered}"
    );
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(!format!("{err:?}").contains("hunter2"), "{err:?}");

    let err = load_from(
        &with_secrets(&dir),
        BUILD,
        env(&[("APP__HTTP__MAX_IN_FLIGHT", "invalid-override")]),
    )
    .unwrap_err();
    assert!(err.to_string().ends_with("in the environment"), "{err}");
    assert!(!err.to_string().contains("invalid-override"), "{err}");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn secrets_directory_failures_name_the_path_and_no_content() {
    let missing = LoadOptions {
        secrets_dir: Some(PathBuf::from("/definitely/missing-secrets")),
        ..LoadOptions::default()
    };
    assert_matches!(
        load_from(&missing, BUILD, env(&[])),
        Err(Error::ReadSecretsDir { .. })
    );

    for name in ["APP__HTTP.ADDR", "APP__HTTP__ADDR__", "APP__"] {
        let dir = tempfile::tempdir().unwrap();
        let file = write(&dir, name, "127.0.0.1:7");
        let err = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap_err();
        assert_matches!(&err, Error::MalformedSecretName { path } if *path == file, "{name}: {err}");
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("APP__HTTP__ADDR")).unwrap();
    assert_matches!(
        load_from(&with_secrets(&dir), BUILD, env(&[])),
        Err(Error::ReadSecret { .. })
    );

    for content in [&b"hunter2\xff"[..], &b"\0hunter2"[..]] {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("APP__LOG__LEVEL");
        std::fs::write(&file, content).unwrap();
        let err = load_from(&with_secrets(&dir), BUILD, env(&[])).unwrap_err();
        assert_matches!(&err, Error::ReadSecret { path, .. } if *path == file, "{err}");
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
    assert_matches!(&err, Error::AmbiguousName { name, other }
            if name == "APP__http__addr" && other == "APP__HTTP__ADDR", "{err}");
}

#[rstest::rstest]
#[case::environment_only(false)]
#[case::across_carriers(true)]
fn overlapping_variable_paths_are_refused(#[case] parent_from_secrets_dir: bool) {
    let dir = tempfile::tempdir().unwrap();
    let mut options = LoadOptions {
        config: Some(write(&dir, "base.toml", "[http]\nrequest_timeout = \"9s\"")),
        ..LoadOptions::default()
    };
    let parent = ("APP__http", "sample-value");
    let mut environment = vec![("APP__HTTP__ADDR", "127.0.0.1:8080")];
    if parent_from_secrets_dir {
        write(&dir, parent.0, parent.1);
        options.secrets_dir = Some(dir.path().to_owned());
    } else {
        environment.push(parent);
    }

    let err = load_from(&options, BUILD, env(&environment)).unwrap_err();
    let rendered = err.to_string();
    assert!(
        rendered.contains("APP__http and APP__HTTP__ADDR set overlapping keys"),
        "{err}"
    );
    assert!(!rendered.contains(parent.1), "{err}");
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
    assert_matches!(&err, Error::NonUnicodeEnvValue { name } if name == "APP__LOG__LEVEL", "{err}");
    assert!(!err.to_string().contains("hunter2"), "{err}");

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        [(not_unicode("APP__LOG__LEVEL"), OsString::from("info"))],
    )
    .unwrap_err();
    assert_matches!(err, Error::MalformedEnvName { .. }, "{err}");

    load_from(
        &LoadOptions::default(),
        BUILD,
        [(not_unicode("UNRELATED"), not_unicode("value"))],
    )
    .unwrap();
}

#[test]
fn typed_version_and_commit_override_build_info() {
    let cfg = load_env(&[
        ("APP__APP__VERSION", "9.9.9"),
        ("APP__APP__COMMIT", "deadbeef"),
    ]);
    assert_eq!(cfg.app.version, "9.9.9");
    assert_eq!(cfg.app.commit, "deadbeef");
}

#[test]
fn toml_non_string_on_string_field_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "bad.toml", "[app]\ninstance_id = false\n");
    let err = load_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    let rendered = err.to_string();
    assert!(rendered.contains("app.instance_id"), "{rendered}");
    assert!(rendered.contains("expected a string"), "{rendered}");
}

#[test]
fn toml_non_boolean_on_boolean_field_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "bad.toml", "[http]\naccess_log_health_probes = 42\n");
    let err = load_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    let rendered = err.to_string();
    assert!(
        rendered.contains("http.access_log_health_probes"),
        "{rendered}"
    );
    assert!(rendered.contains("expected a boolean"), "{rendered}");
}

#[test]
fn environment_enum_rejection_names_variants_without_echoing_value() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__LOG__FORMAT", "yaml")]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    let rendered = err.to_string();
    assert!(
        rendered.contains("expected `json` or `text` for key `log.format` in the environment"),
        "{rendered}"
    );
    assert!(!rendered.contains("yaml"), "{rendered}");
    assert!(!format!("{err:?}").contains("yaml"), "{err:?}");
}

#[test]
fn unquoted_toml_datetime_is_rejected_unless_overridden() {
    let dir = tempfile::tempdir().unwrap();
    let base = write(&dir, "base.toml", "[app]\nenv = 2026-11-13\n");
    let base_only = LoadOptions {
        config: Some(base.clone()),
        ..LoadOptions::default()
    };
    let err = load_from(&base_only, BUILD, env(&[])).unwrap_err();
    assert_matches!(&err, Error::Deserialize(_), "{err}");
    let rendered = err.to_string();
    assert!(rendered.contains("app.env"), "{rendered}");
    assert!(
        rendered.contains("must not be an unquoted TOML datetime"),
        "{rendered}"
    );

    let overlay = write(&dir, "overlay.toml", "[app]\nenv = \"2026-11-13\"\n");
    let with_overlay = LoadOptions {
        config: Some(base),
        config_overlay: vec![overlay],
        ..LoadOptions::default()
    };
    let cfg = load_from(&with_overlay, BUILD, env(&[])).unwrap();
    assert_eq!(cfg.app.env, "2026-11-13");

    let cfg = load_from(&base_only, BUILD, env(&[("APP__APP__ENV", "production")])).unwrap();
    assert_eq!(cfg.app.env, "production");

    let scalar_section = LoadOptions {
        config: Some(write(&dir, "scalar.toml", "app = 2026-11-13\n")),
        ..LoadOptions::default()
    };
    let cfg = load_from(
        &scalar_section,
        BUILD,
        env(&[("APP__APP__ENV", "production")]),
    )
    .unwrap();
    assert_eq!(cfg.app.env, "production");
}

#[test]
fn shutdown_tail_aggregates_grace_budget_with_other_violations() {
    let err = load_with_tail_from(
        &LoadOptions::default(),
        BUILD,
        Some(crate::ShutdownTail::new(
            Duration::from_millis(18_500),
            "teardown tail",
        )),
        env(&[("APP__HTTP__GRACE_PERIOD", "40s"), ("APP__APP__ENV", " ")]),
    )
    .unwrap_err();
    let Error::Validate(errors) = &err else {
        panic!("expected aggregated validation errors: {err}");
    };
    assert!(errors.names("http.grace_period"), "{errors}");
    assert!(errors.names("app.env"), "{errors}");
}
