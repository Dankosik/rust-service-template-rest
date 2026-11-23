use std::assert_matches;
use std::path::Path;

use secrecy::ExposeSecret;

use super::*;
use crate::LoadOptions;
use crate::load::{ErrorKind, load_from};
use crate::test_support::{
    BUILD, assert_secret_in_file, assert_unknown_env_key_fails, env, load_env, write,
};
use crate::validate::{ValidationErrors, admit_with};

fn https_input() -> ClickHouseInput {
    ClickHouseInput {
        url: Some("https://ch.example:8443".to_owned()),
        database: Some("app".to_owned()),
        username: Some("app".to_owned()),
        password: Some(SecretString::from("hunter2".to_owned())),
        ..ClickHouseInput::default()
    }
}

fn admit(
    input: ClickHouseInput,
    local_development: bool,
) -> Result<Option<ClickHouseConfig>, ValidationErrors> {
    admit_with(|v| input.admit(local_development, v))
}

fn rejected_keys(input: ClickHouseInput, local_development: bool) -> Vec<String> {
    admit(input, local_development)
        .unwrap_err()
        .iter()
        .map(|error| error.key.clone())
        .collect()
}

#[test]
fn omitted_or_blank_url_is_inert_while_active_config_applies_defaults() {
    assert!(load_env(&[]).clickhouse().is_none());

    for blank_url in ["", " \t "] {
        let cfg = load_env(&[
            ("APP__CLICKHOUSE__URL", blank_url),
            ("APP__CLICKHOUSE__ROOT_CA_PATH", "/run/tls/root-ca.pem"),
            ("APP__CLICKHOUSE__DATABASE", "app"),
        ]);
        assert!(cfg.clickhouse().is_none());
    }

    let minimal = load_env(&[
        ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
        ("APP__CLICKHOUSE__DATABASE", "analytics"),
        ("APP__CLICKHOUSE__USERNAME", "svc"),
        ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
    ]);
    let minimal_ch = minimal.clickhouse().unwrap();
    assert_eq!(minimal_ch.root_ca_path, None);
    assert!(minimal_ch.client_certificate.is_none());

    let cfg = load_env(&[
        ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
        ("APP__CLICKHOUSE__DATABASE", "analytics"),
        ("APP__CLICKHOUSE__USERNAME", "svc"),
        ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
        ("APP__CLICKHOUSE__ROOT_CA_PATH", "/run/tls/ca.pem"),
        ("APP__CLICKHOUSE__CLIENT_CERT_PATH", "/run/tls/client.crt"),
        ("APP__CLICKHOUSE__CLIENT_KEY_PATH", "/run/tls/client.key"),
    ]);
    let ch = cfg.clickhouse().unwrap();
    assert_eq!(ch.url, "https://ch.example:8443");
    assert_eq!(ch.database, "analytics");
    assert_eq!(ch.username.as_deref(), Some("svc"));
    assert_eq!(
        ch.password.as_ref().map(ExposeSecret::expose_secret),
        Some("hunter2")
    );
    assert_eq!(ch.password_file, None);
    assert_eq!(
        ch.root_ca_path.as_deref(),
        Some(Path::new("/run/tls/ca.pem"))
    );
    let cert = ch.client_certificate.as_ref().unwrap();
    assert_eq!(cert.cert_path, Path::new("/run/tls/client.crt"));
    assert_eq!(cert.key_path, Path::new("/run/tls/client.key"));
    assert!(!ch.allow_plaintext);
    assert!(!ch.allow_unauthenticated);
    assert_eq!(ch.query_settings, ClickHouseQuerySettings::Client);
    assert_eq!(ch.query_timeout, Duration::from_secs(5));
    assert_eq!(ch.insert_timeout, Duration::from_secs(5));
    assert_eq!(ch.max_concurrency.get(), 32);
    assert_eq!(ch.max_result_rows.get(), 100_000);
    assert_eq!(ch.max_result_bytes.get(), 16 * 1024 * 1024);
    assert_eq!(ch.http_response_buffer_size.get(), 2 * 1024 * 1024);
    assert_eq!(
        ch.max_memory_usage.map(NonZeroU64::get),
        Some(512 * 1024 * 1024)
    );
    assert!(ch.propagate_trace_context);
    assert!(!format!("{cfg:?}").contains("hunter2"));
}

#[test]
fn independent_ranges_pairs_and_dev_flags_validate_even_when_inert() {
    for (variable, key) in [
        (
            "APP__CLICKHOUSE__ALLOW_PLAINTEXT",
            "clickhouse.allow_plaintext",
        ),
        (
            "APP__CLICKHOUSE__ALLOW_UNAUTHENTICATED",
            "clickhouse.allow_unauthenticated",
        ),
    ] {
        let error = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__APP__ENV", "local"), (variable, "true")]),
        )
        .unwrap_err();
        assert!(error.validation_errors().unwrap().names(key), "{error}");
    }

    let cert_only = ClickHouseInput {
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        ..ClickHouseInput::default()
    };
    assert_eq!(
        rejected_keys(cert_only, false),
        ["clickhouse.client_key_path"]
    );
    let key_only = ClickHouseInput {
        client_key_path: Some(PathBuf::from("/run/tls/client.key")),
        ..ClickHouseInput::default()
    };
    assert_eq!(
        rejected_keys(key_only, false),
        ["clickhouse.client_cert_path"]
    );

    let both_passwords = ClickHouseInput {
        password: Some(SecretString::from("hunter2".to_owned())),
        password_file: Some(PathBuf::from("/run/secrets/ch-password")),
        ..ClickHouseInput::default()
    };
    assert_eq!(
        rejected_keys(both_passwords, false),
        ["clickhouse.password_file"]
    );

    let many_violations = ClickHouseInput {
        allow_plaintext: true,
        allow_unauthenticated: true,
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        query_timeout: Duration::from_millis(9),
        insert_timeout: Duration::from_mins(6),
        max_concurrency: 0,
        max_result_rows: 10_000_001,
        max_result_bytes: ByteSize::kib(63),
        http_response_buffer_size: Some(ByteSize::kib(32)),
        max_memory_usage: ByteSize::mib(15),
        ..ClickHouseInput::default()
    };
    assert_eq!(
        rejected_keys(many_violations, false),
        [
            "clickhouse.allow_plaintext",
            "clickhouse.allow_unauthenticated",
            "clickhouse.query_timeout",
            "clickhouse.insert_timeout",
            "clickhouse.max_concurrency",
            "clickhouse.max_result_rows",
            "clickhouse.max_result_bytes",
            "clickhouse.http_response_buffer_size",
            "clickhouse.max_memory_usage",
            "clickhouse.client_key_path",
        ]
    );

    for (mutate, key) in [
        (
            (|input: &mut ClickHouseInput| input.query_timeout = Duration::from_millis(300_001))
                as fn(&mut _),
            "clickhouse.query_timeout",
        ),
        (
            |input| input.insert_timeout = Duration::from_millis(9),
            "clickhouse.insert_timeout",
        ),
        (
            |input| input.max_concurrency = 1025,
            "clickhouse.max_concurrency",
        ),
        (
            |input| input.max_result_rows = 0,
            "clickhouse.max_result_rows",
        ),
        (
            |input| input.max_result_bytes = ByteSize::mib(129),
            "clickhouse.max_result_bytes",
        ),
    ] {
        let mut input = ClickHouseInput::default();
        mutate(&mut input);
        assert_eq!(rejected_keys(input, false), [key]);
    }
}

#[test]
fn max_memory_usage_decodes_zero_to_none_and_checks_non_zero_bounds() {
    let env_zero = load_env(&[
        ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
        ("APP__CLICKHOUSE__DATABASE", "app"),
        ("APP__CLICKHOUSE__USERNAME", "app"),
        ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
        ("APP__CLICKHOUSE__MAX_MEMORY_USAGE", "0"),
    ]);
    assert_eq!(env_zero.clickhouse().unwrap().max_memory_usage, None);

    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("int_zero.toml", "max_memory_usage = 0"),
        ("str_zero.toml", "max_memory_usage = \"0\""),
    ] {
        let file = write(
            &dir,
            name,
            &format!(
                "[clickhouse]\nurl = \"https://ch.example:8443\"\ndatabase = \"app\"\nusername = \"app\"\n{body}\n"
            ),
        );
        let cfg = load_from(
            &LoadOptions {
                config: Some(file),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[("APP__CLICKHOUSE__PASSWORD", "hunter2")]),
        )
        .unwrap();
        assert_eq!(cfg.clickhouse().unwrap().max_memory_usage, None, "{name}");
    }

    for valid in [ByteSize::mib(16), ByteSize::gib(64)] {
        let input = ClickHouseInput {
            max_memory_usage: valid,
            ..https_input()
        };
        assert_eq!(
            admit(input, false)
                .unwrap()
                .unwrap()
                .max_memory_usage
                .map(NonZeroU64::get),
            Some(valid.as_u64())
        );
    }

    for invalid in [
        ByteSize::b(1),
        ByteSize::b(16 * 1024 * 1024 - 1),
        ByteSize::b(64 * 1024 * 1024 * 1024 + 1),
    ] {
        let input = ClickHouseInput {
            max_memory_usage: invalid,
            ..https_input()
        };
        assert_eq!(rejected_keys(input, false), ["clickhouse.max_memory_usage"]);
    }
}

#[test]
fn http_response_buffer_size_tracks_max_result_bytes() {
    let capped_default = load_env(&[
        ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
        ("APP__CLICKHOUSE__DATABASE", "app"),
        ("APP__CLICKHOUSE__USERNAME", "app"),
        ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
        ("APP__CLICKHOUSE__MAX_RESULT_BYTES", "1 MiB"),
    ]);
    let ch = capped_default.clickhouse().unwrap();
    assert_eq!(ch.max_result_bytes.get(), 1024 * 1024);
    assert_eq!(ch.http_response_buffer_size.get(), 1024 * 1024);

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
            ("APP__CLICKHOUSE__DATABASE", "app"),
            ("APP__CLICKHOUSE__USERNAME", "app"),
            ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
            ("APP__CLICKHOUSE__MAX_RESULT_BYTES", "1 MiB"),
            ("APP__CLICKHOUSE__HTTP_RESPONSE_BUFFER_SIZE", "2 MiB"),
        ]),
    )
    .unwrap_err();
    assert!(
        err.validation_errors()
            .unwrap()
            .names("clickhouse.http_response_buffer_size"),
        "{err}"
    );

    let min_edge = admit(
        ClickHouseInput {
            max_result_bytes: ByteSize::kib(64),
            http_response_buffer_size: Some(ByteSize::kib(64)),
            ..https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(min_edge.http_response_buffer_size.get(), 64 * 1024);

    let max_edge = admit(
        ClickHouseInput {
            max_result_bytes: ByteSize::mib(128),
            http_response_buffer_size: Some(ByteSize::mib(128)),
            ..https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(max_edge.http_response_buffer_size.get(), 128 * 1024 * 1024);
}

#[rstest::rstest]
#[case::root_without_slash("https://ch.example:8443", false, "https://ch.example:8443")]
#[case::root_with_slash("https://ch.example:8443/", false, "https://ch.example:8443")]
#[case::prefix_without_slash("https://ch.example:8443/ch", false, "https://ch.example:8443/ch")]
#[case::prefix_with_slash("https://ch.example:8443/ch/", false, "https://ch.example:8443/ch")]
#[case::nested_prefix("https://ch.example:8443/a/b/", false, "https://ch.example:8443/a/b")]
#[case::ipv6_prefix("https://[::1]:8443/ch/", false, "https://[::1]:8443/ch")]
#[case::http_dev("http://127.0.0.1:8123/proxy/", true, "http://127.0.0.1:8123/proxy")]
fn valid_urls_normalize_trailing_slash(
    #[case] raw: &str,
    #[case] allow_plaintext: bool,
    #[case] expected: &str,
) {
    let input = ClickHouseInput {
        url: Some(raw.to_owned()),
        allow_plaintext,
        ..https_input()
    };
    let admitted = admit(input, allow_plaintext).unwrap().unwrap();
    assert_eq!(admitted.url, expected);
}

#[rstest::rstest]
#[case::not_a_url("not-a-url")]
#[case::unsupported_scheme("clickhouse://ch.example:8443")]
#[case::missing_host("https://")]
#[case::triple_slash("https:///ch.example:8443")]
#[case::triple_slash_ip("https:///127.0.0.1:8443")]
#[case::single_slash_after_scheme("https:/127.0.0.1:8443")]
#[case::userinfo_password("https://user:secret@ch.example:8443")]
#[case::userinfo_only("https://user@ch.example:8443")]
#[case::userinfo_empty("https://@ch.example:8443")]
#[case::userinfo_triple_slash("HTTPS:///@ch.example:8443")]
#[case::query("https://ch.example:8443?database=app")]
#[case::empty_query("https://ch.example:8443?")]
#[case::fragment("https://ch.example:8443#frag")]
#[case::empty_fragment("https://ch.example:8443#")]
#[case::double_slash_root("https://ch.example:8443//")]
#[case::double_slash_path("https://ch.example:8443/a//b")]
#[case::single_dot("https://ch.example:8443/.")]
#[case::double_dot("https://ch.example:8443/..")]
#[case::dot_segment("https://ch.example:8443/a/./b")]
#[case::parent_segment("https://ch.example:8443/a/../b")]
#[case::encoded_dot("https://ch.example:8443/a/%2e/b")]
#[case::encoded_parent("https://ch.example:8443/a/%2E%2E/b")]
#[case::mixed_dot_prefix("https://ch.example:8443/a/.%2e/b")]
#[case::mixed_dot_suffix("https://ch.example:8443/a/%2e./b")]
#[case::backslash("https://ch.example:8443/a\\b")]
#[case::backslash_traversal("https://ch.example:8443/a\\..\\b")]
#[case::newline("https://ch.example:8443\n")]
#[case::tab("https://ch.example:8443\t")]
#[case::space("https://ch.example:8443 ")]
#[case::plaintext_refused("http://127.0.0.1:8123")]
fn invalid_urls_are_rejected(#[case] raw: &str) {
    let input = ClickHouseInput {
        url: Some(raw.to_owned()),
        ..https_input()
    };
    assert!(admit(input, false).unwrap_err().names("clickhouse.url"));
}

#[test]
fn http_url_rejects_tls_materials() {
    let input = ClickHouseInput {
        url: Some("http://127.0.0.1:8123".to_owned()),
        allow_plaintext: true,
        root_ca_path: Some(PathBuf::from("/run/tls/ca.pem")),
        client_cert_path: Some(PathBuf::from("/run/tls/client.crt")),
        client_key_path: Some(PathBuf::from("/run/tls/client.key")),
        ..https_input()
    };
    assert_eq!(
        rejected_keys(input, true),
        [
            "clickhouse.root_ca_path",
            "clickhouse.client_cert_path",
            "clickhouse.client_key_path",
        ]
    );
}

#[test]
fn header_safe_checks_reject_ascii_control_bytes_without_leaking_secrets() {
    for bad in [
        "val\nue",
        "val\rue",
        "val\0ue",
        "val\tue",
        "val\x1fue",
        "val\x7fue",
    ] {
        let db = ClickHouseInput {
            database: Some(bad.to_owned()),
            ..https_input()
        };
        assert_eq!(rejected_keys(db, false), ["clickhouse.database"]);

        let user = ClickHouseInput {
            username: Some(bad.to_owned()),
            ..https_input()
        };
        assert_eq!(rejected_keys(user, false), ["clickhouse.username"]);

        let pw_err = admit(
            ClickHouseInput {
                password: Some(SecretString::from(format!("hunter2{bad}"))),
                ..https_input()
            },
            false,
        )
        .unwrap_err();
        assert!(pw_err.names("clickhouse.password"));
        let rendered = format!("{pw_err} {pw_err:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");

        let inert_pw = admit(
            ClickHouseInput {
                password: Some(SecretString::from(format!("hunter2{bad}"))),
                ..ClickHouseInput::default()
            },
            false,
        )
        .unwrap_err();
        assert!(inert_pw.names("clickhouse.password"));
        assert!(!format!("{inert_pw} {inert_pw:?}").contains("hunter2"));
    }

    let spaced = load_env(&[
        ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
        ("APP__CLICKHOUSE__DATABASE", "app"),
        ("APP__CLICKHOUSE__USERNAME", "app"),
        ("APP__CLICKHOUSE__PASSWORD", " pw "),
    ]);
    assert_eq!(
        spaced
            .clickhouse()
            .unwrap()
            .password
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some(" pw ")
    );
}

#[test]
fn authentication_matrix_enforces_all_five_rows() {
    let with_file = ClickHouseInput {
        password: None,
        password_file: Some(PathBuf::from("/run/secrets/clickhouse-password")),
        ..https_input()
    };
    assert!(admit(with_file, false).unwrap().is_some());

    let no_user = ClickHouseInput {
        username: None,
        ..https_input()
    };
    assert_eq!(rejected_keys(no_user, false), ["clickhouse.username"]);

    let no_password = ClickHouseInput {
        password: None,
        ..https_input()
    };
    assert_eq!(rejected_keys(no_password, false), ["clickhouse.password"]);

    let whitespace_password = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
            ("APP__CLICKHOUSE__DATABASE", "app"),
            ("APP__CLICKHOUSE__USERNAME", "app"),
            ("APP__CLICKHOUSE__PASSWORD", "   "),
        ]),
    )
    .unwrap_err();
    assert!(
        whitespace_password
            .validation_errors()
            .unwrap()
            .names("clickhouse.password"),
        "{whitespace_password}"
    );

    let both_sources = ClickHouseInput {
        password_file: Some(PathBuf::from("/run/secrets/clickhouse-password")),
        ..https_input()
    };
    assert_eq!(
        rejected_keys(both_sources, false),
        ["clickhouse.password_file"]
    );

    for username in [None, Some("dev_user".to_owned())] {
        let unauth = ClickHouseInput {
            username: username.clone(),
            password: None,
            allow_unauthenticated: true,
            ..https_input()
        };
        let cfg = admit(unauth, true).unwrap().unwrap();
        assert!(cfg.allow_unauthenticated);
        assert_eq!(cfg.username, username);
        assert!(cfg.password.is_none());
    }

    let unauth_with_password = ClickHouseInput {
        allow_unauthenticated: true,
        ..https_input()
    };
    assert_eq!(
        rejected_keys(unauth_with_password, true),
        ["clickhouse.password"]
    );

    let unauth_with_file = ClickHouseInput {
        password: None,
        password_file: Some(PathBuf::from("/run/secrets/clickhouse-password")),
        allow_unauthenticated: true,
        ..https_input()
    };
    assert_eq!(
        rejected_keys(unauth_with_file, true),
        ["clickhouse.password_file"]
    );
}

#[test]
fn password_in_toml_file_is_refused_and_empty_placeholder_allowed() {
    assert_secret_in_file(
        "[clickhouse]\npassword = \"hunter2\"\n",
        "clickhouse.password",
    );

    let dir = tempfile::tempdir().unwrap();
    let placeholder = write(
        &dir,
        "ch.toml",
        "[clickhouse]\nurl = \"https://ch.example:8443\"\ndatabase = \"app\"\nusername = \"app\"\npassword = \"\"\n",
    );
    let secrets_dir = tempfile::tempdir().unwrap();
    write(
        &secrets_dir,
        "APP__CLICKHOUSE__PASSWORD",
        "from-secrets-dir\n",
    );
    let cfg = load_from(
        &LoadOptions {
            config: Some(placeholder),
            secrets_dir: Some(secrets_dir.path().to_owned()),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    assert_eq!(
        cfg.clickhouse()
            .unwrap()
            .password
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("from-secrets-dir")
    );

    let cfg_env_override = load_from(
        &LoadOptions {
            secrets_dir: Some(secrets_dir.path().to_owned()),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
            ("APP__CLICKHOUSE__DATABASE", "app"),
            ("APP__CLICKHOUSE__USERNAME", "app"),
            ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
        ]),
    )
    .unwrap();
    assert_eq!(
        cfg_env_override
            .clickhouse()
            .unwrap()
            .password
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("hunter2")
    );
}

#[test]
fn query_settings_and_unknown_or_out_of_scope_keys_fail_loading() {
    let server = load_env(&[
        ("APP__CLICKHOUSE__URL", "https://ch.example:8443"),
        ("APP__CLICKHOUSE__DATABASE", "app"),
        ("APP__CLICKHOUSE__USERNAME", "app"),
        ("APP__CLICKHOUSE__PASSWORD", "hunter2"),
        ("APP__CLICKHOUSE__QUERY_SETTINGS", "server"),
        ("APP__CLICKHOUSE__PROPAGATE_TRACE_CONTEXT", "false"),
    ]);
    let ch = server.clickhouse().unwrap();
    assert_eq!(ch.query_settings, ClickHouseQuerySettings::Server);
    assert!(!ch.propagate_trace_context);

    assert_matches!(
        load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__CLICKHOUSE__QUERY_SETTINGS", "readonly")]),
        ),
        Err(crate::load::Error(ErrorKind::Deserialize(_)))
    );

    assert_unknown_env_key_fails("APP__CLICKHOUSE__UNKNOWN");
    for rejected in [
        "APP__CLICKHOUSE__POOL_IDLE_TIMEOUT",
        "APP__CLICKHOUSE__VALIDATE_ROW_BINARY",
        "APP__CLICKHOUSE__COMPRESSION",
        "APP__CLICKHOUSE__TRUSTED_NETWORK",
        "APP__CLICKHOUSE__ACCESS_TOKEN",
    ] {
        assert_unknown_env_key_fails(rejected);
    }
}

#[test]
fn local_toml_keeps_clickhouse_inert_until_url_is_set_and_retains_section_across_binaries() {
    let local_toml = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../env/config/local.toml");
    let options = LoadOptions {
        config: Some(local_toml),
        ..LoadOptions::default()
    };

    let inert = load_from(&options, BUILD, env(&[])).unwrap();
    assert!(inert.clickhouse().is_none());

    let active = load_from(
        &options,
        BUILD,
        env(&[
            ("APP__CLICKHOUSE__URL", "http://127.0.0.1:8123"),
            ("APP__CLICKHOUSE__PASSWORD", "app"),
        ]),
    )
    .unwrap();
    let ch = active.clickhouse().unwrap();
    assert_eq!(ch.url, "http://127.0.0.1:8123");
    assert_eq!(ch.database, "app");
    assert_eq!(ch.username.as_deref(), Some("app"));
    assert!(ch.allow_plaintext);

    // template:begin postgres:clickhouse-migration-retention-test
    let migration = crate::load::load_migration_from(
        &options,
        BUILD,
        env(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
            ("APP__CLICKHOUSE__URL", "http://127.0.0.1:8123"),
        ]),
    )
    .unwrap();
    assert!(migration.app().is_local_development());
    // template:end postgres:clickhouse-migration-retention-test
}
