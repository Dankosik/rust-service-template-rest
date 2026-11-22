use std::assert_matches;

use secrecy::ExposeSecret;

use super::*;
use crate::LoadOptions;
use crate::load::{ErrorKind, InputError, load_from, load_temporal_worker_from};
use crate::test_support::{
    BUILD, assert_secret_in_file, assert_unknown_env_key_fails, env, load_env, write,
};
use crate::validate::{ValidationErrors, admit_with};

fn admit(
    input: TemporalInput,
    local_development: bool,
) -> Result<Option<TemporalClientConfig>, ValidationErrors> {
    admit_with(|v| input.admit(local_development, "orders-api", "1.2.3", v))
}

fn enabled_https_input() -> TemporalInput {
    TemporalInput {
        enabled: true,
        endpoint: Some("https://ns.acct.tmprl.cloud:7233".to_owned()),
        api_key: Some(SecretString::from("token-value-123".to_owned())),
        ..TemporalInput::default()
    }
}

#[test]
fn disabled_by_default_and_inert_without_endpoint_or_auth() {
    let cfg = load_env(&[]);
    assert!(cfg.temporal().is_none());

    let inert = TemporalInput {
        enabled: false,
        endpoint: Some("not a url".to_owned()),
        allow_plaintext: true,
        ..TemporalInput::default()
    };
    assert!(admit(inert, false).unwrap().is_none());
}

#[test]
fn enabled_https_defaults_and_computed_identity_follow_service_and_build() {
    let cfg = load_env(&[
        ("APP__OBSERVABILITY__OTEL__SERVICE_NAME", "billing"),
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", "https://temporal.example:7233"),
        ("APP__TEMPORAL__API_KEY", "secret-token"),
    ]);
    let temporal = cfg.temporal().expect("temporal enabled");
    assert_eq!(
        temporal.endpoint().as_str(),
        "https://temporal.example:7233/"
    );
    assert_eq!(temporal.namespace(), "default");
    assert_eq!(temporal.identity(), "billing@1.2.3");
    assert_eq!(
        temporal.api_key().map(ExposeSecret::expose_secret),
        Some("secret-token")
    );
    assert_eq!(temporal.api_key_file(), None);
    assert_eq!(temporal.client_cert_path(), None);
    assert_eq!(temporal.client_key_path(), None);
    assert_eq!(temporal.root_ca_path(), None);
    assert_eq!(temporal.mtls_rotation(), TemporalTlsRotation::Static);
    assert_eq!(
        temporal.credential_refresh_interval(),
        Duration::from_secs(30)
    );
    assert!(!temporal.allow_plaintext());
    assert!(!temporal.allow_unauthenticated());
    assert!(!temporal.trusted_network());
    assert_eq!(temporal.connect_timeout(), Duration::from_secs(5));
    assert_eq!(temporal.rpc_timeout(), Duration::from_secs(10));
    assert_eq!(temporal.keep_alive_interval(), Duration::from_secs(30));
    assert_eq!(temporal.keep_alive_timeout(), Duration::from_secs(15));
    assert!(temporal.dns_load_balancing());
    assert_eq!(temporal.http_proxy_url(), None);
    assert_eq!(temporal.max_payload_bytes(), 512 * 1024);
    assert!(!temporal.readiness_critical());
}

#[test]
fn debug_output_redacts_secrets_paths_endpoint_and_proxy() {
    let cfg = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("https://private-temporal.example:7233".to_owned()),
            api_key: Some(SecretString::from("super-secret-bearer".to_owned())),
            client_cert_path: Some(PathBuf::from("/run/secrets/client-cert.pem")),
            client_key_path: Some(PathBuf::from("/run/secrets/client-key.pem")),
            root_ca_path: Some(PathBuf::from("/run/secrets/root-ca.pem")),
            dns_load_balancing: false,
            http_proxy_url: Some("http://private-proxy.internal:8080".to_owned()),
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap()
    .unwrap();
    let rendered = format!("{cfg:?}");
    for sensitive in [
        "super-secret-bearer",
        "private-temporal.example",
        "private-proxy.internal",
        "client-cert.pem",
        "client-key.pem",
        "root-ca.pem",
    ] {
        assert!(!rendered.contains(sensitive), "{sensitive}: {rendered}");
    }
    for marker in [
        "api_key_file: false",
        "client_cert_path: true",
        "client_key_path: true",
        "root_ca_path: true",
    ] {
        assert!(rendered.contains(marker), "{marker}: {rendered}");
    }

    let cfg_with_key_file = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("https://private-temporal.example:7233".to_owned()),
            api_key_file: Some(PathBuf::from("/run/secrets/temporal-api-key.txt")),
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap()
    .unwrap();
    let rendered_key_file = format!("{cfg_with_key_file:?}");
    for sensitive in ["temporal-api-key.txt", "/run/secrets/"] {
        assert!(
            !rendered_key_file.contains(sensitive),
            "{sensitive}: {rendered_key_file}"
        );
    }
    assert!(
        rendered_key_file.contains("api_key_file: true"),
        "{rendered_key_file}"
    );
}

#[test]
fn api_key_in_file_is_refused_in_service_and_worker_while_empty_placeholder_is_allowed() {
    assert_secret_in_file("[temporal]\napi_key = \"top-secret\"\n", "temporal.api_key");

    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[temporal]\napi_key = \"top-secret\"\n",
    );
    let err = load_temporal_worker_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(
        &err.0,
        ErrorKind::Input(InputError::SecretInFile { key, .. }) if key == "temporal.api_key",
        "{err}"
    );

    let placeholder = write(&dir, "placeholder.toml", "[temporal]\napi_key = \"\"\n");
    let cfg = load_from(
        &LoadOptions {
            config: Some(placeholder),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    assert!(cfg.temporal().is_none());
}

#[test]
fn api_key_from_env_preserves_scalar_strings_and_rejects_non_header_ascii() {
    for secret in ["00123", "TRUE"] {
        let cfg = load_env(&[
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", "https://temporal.example:7233"),
            ("APP__TEMPORAL__API_KEY", secret),
        ]);
        assert_eq!(
            cfg.temporal()
                .unwrap()
                .api_key()
                .map(ExposeSecret::expose_secret),
            Some(secret)
        );
    }

    for invalid in [
        "bad\ntoken-secret",
        "bad\rtoken-secret",
        "bad\ttoken-secret",
        "bad\x7ftoken-secret",
        "bad-é-token-secret",
    ] {
        let errors = admit(
            TemporalInput {
                api_key: Some(SecretString::from(invalid.to_owned())),
                ..enabled_https_input()
            },
            false,
        )
        .unwrap_err();
        assert!(errors.names("temporal.api_key"), "{invalid:?}: {errors}");
        assert!(!errors.to_string().contains("token-secret"), "{errors}");
        assert!(
            !format!("{errors:?}").contains("token-secret"),
            "{errors:?}"
        );
    }
}

#[test]
fn endpoint_validation_rejects_missing_malformed_userinfo_query_fragment_and_controls() {
    let missing = admit(
        TemporalInput {
            enabled: true,
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap_err();
    assert!(missing.names("temporal.endpoint"), "{missing}");

    for bad in [
        "grpc://temporal.example:7233",
        "https://",
        "https://user:secret-pass@temporal.example:7233",
        "https://@temporal.example:7233",
        "https://temporal.example:7233?foo=bar",
        "https://temporal.example:7233?",
        "https://temporal.example:7233#frag",
        "https://temporal.example:7233#",
        " https://temporal.example:7233",
        "https://temporal.example:7233\n",
        "https://temporal\texample:7233",
        "https://temporal.example\\path",
    ] {
        let errors = admit(
            TemporalInput {
                endpoint: Some(bad.to_owned()),
                ..enabled_https_input()
            },
            false,
        )
        .unwrap_err();
        assert!(errors.names("temporal.endpoint"), "{bad:?}: {errors}");
        assert!(!errors.to_string().contains("secret-pass"), "{errors}");
    }
}

#[test]
fn plaintext_rules_enforce_https_unless_local_dev_or_trusted_network() {
    let prod_http = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("http://temporal.internal:7233".to_owned()),
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap_err();
    assert!(prod_http.names("temporal.endpoint"), "{prod_http}");

    let prod_allow_plaintext = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("http://temporal.internal:7233".to_owned()),
            allow_plaintext: true,
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap_err();
    assert!(
        prod_allow_plaintext.names("temporal.allow_plaintext"),
        "{prod_allow_plaintext}"
    );
    assert!(
        prod_allow_plaintext.names("temporal.endpoint"),
        "{prod_allow_plaintext}"
    );

    let prod_allow_plaintext_on_https = admit(
        TemporalInput {
            allow_plaintext: true,
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        prod_allow_plaintext_on_https.names("temporal.allow_plaintext"),
        "{prod_allow_plaintext_on_https}"
    );

    let local_http = load_env(&[
        ("APP__APP__ENV", "local"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", "http://127.0.0.1:7233"),
        ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
    ]);
    assert!(local_http.temporal().unwrap().allow_plaintext());

    let trusted_http = load_env(&[
        ("APP__APP__ENV", "production"),
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", "http://temporal.internal:7233"),
        ("APP__TEMPORAL__TRUSTED_NETWORK", "true"),
    ]);
    assert!(trusted_http.temporal().unwrap().trusted_network());
}

#[test]
fn unauthenticated_https_requires_local_dev_or_trusted_network() {
    let https_no_auth = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("https://temporal.example:7233".to_owned()),
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap_err();
    assert!(https_no_auth.names("temporal.api_key"), "{https_no_auth}");

    let prod_unauth = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("https://temporal.example:7233".to_owned()),
            allow_unauthenticated: true,
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap_err();
    assert!(
        prod_unauth.names("temporal.allow_unauthenticated"),
        "{prod_unauth}"
    );
    assert!(prod_unauth.names("temporal.api_key"), "{prod_unauth}");

    let local_https_unauth = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("https://temporal.local:7233".to_owned()),
            allow_unauthenticated: true,
            ..TemporalInput::default()
        },
        true,
    )
    .unwrap()
    .unwrap();
    assert!(local_https_unauth.allow_unauthenticated());

    let trusted_https_unauth = admit(
        TemporalInput {
            enabled: true,
            endpoint: Some("https://temporal.internal:7233".to_owned()),
            trusted_network: true,
            ..TemporalInput::default()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert!(trusted_https_unauth.trusted_network());
}

#[test]
fn api_key_exclusivity_and_mtls_pairing_are_enforced() {
    let both_api_keys = admit(
        TemporalInput {
            api_key_file: Some(PathBuf::from("/run/secrets/temporal.key")),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        both_api_keys.names("temporal.api_key_file"),
        "{both_api_keys}"
    );

    let key_file_only = admit(
        TemporalInput {
            api_key: None,
            api_key_file: Some(PathBuf::from("/run/secrets/temporal.key")),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        key_file_only.api_key_file(),
        Some(Path::new("/run/secrets/temporal.key"))
    );

    let cert_without_key = admit(
        TemporalInput {
            api_key: None,
            client_cert_path: Some(PathBuf::from("/etc/temporal/client.crt")),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        cert_without_key.names("temporal.client_key_path"),
        "{cert_without_key}"
    );

    let key_without_cert = admit(
        TemporalInput {
            api_key: None,
            client_key_path: Some(PathBuf::from("/etc/temporal/client.key")),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        key_without_cert.names("temporal.client_cert_path"),
        "{key_without_cert}"
    );

    let mtls_static = admit(
        TemporalInput {
            api_key: None,
            client_cert_path: Some(PathBuf::from("/etc/temporal/client.crt")),
            client_key_path: Some(PathBuf::from("/etc/temporal/client.key")),
            root_ca_path: Some(PathBuf::from("/etc/temporal/ca.crt")),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        mtls_static.client_cert_path(),
        Some(Path::new("/etc/temporal/client.crt"))
    );
    assert_eq!(
        mtls_static.client_key_path(),
        Some(Path::new("/etc/temporal/client.key"))
    );
    assert_eq!(
        mtls_static.root_ca_path(),
        Some(Path::new("/etc/temporal/ca.crt"))
    );
    assert_eq!(mtls_static.mtls_rotation(), TemporalTlsRotation::Static);
}

#[test]
fn dynamic_mtls_rotation_requires_mtls_and_forbids_dns_lb_or_proxy() {
    let dynamic_without_mtls = admit(
        TemporalInput {
            mtls_rotation: TemporalTlsRotation::Dynamic,
            dns_load_balancing: false,
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        dynamic_without_mtls.names("temporal.mtls_rotation"),
        "{dynamic_without_mtls}"
    );

    let dynamic_with_dns_lb = admit(
        TemporalInput {
            api_key: None,
            client_cert_path: Some(PathBuf::from("/etc/temporal/client.crt")),
            client_key_path: Some(PathBuf::from("/etc/temporal/client.key")),
            mtls_rotation: TemporalTlsRotation::Dynamic,
            dns_load_balancing: true,
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        dynamic_with_dns_lb.names("temporal.mtls_rotation"),
        "{dynamic_with_dns_lb}"
    );

    let dynamic_with_proxy = admit(
        TemporalInput {
            api_key: None,
            client_cert_path: Some(PathBuf::from("/etc/temporal/client.crt")),
            client_key_path: Some(PathBuf::from("/etc/temporal/client.key")),
            mtls_rotation: TemporalTlsRotation::Dynamic,
            dns_load_balancing: false,
            http_proxy_url: Some("http://proxy.internal:8080".to_owned()),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        dynamic_with_proxy.names("temporal.mtls_rotation"),
        "{dynamic_with_proxy}"
    );

    let dynamic_valid = admit(
        TemporalInput {
            api_key: None,
            client_cert_path: Some(PathBuf::from("/etc/temporal/client.crt")),
            client_key_path: Some(PathBuf::from("/etc/temporal/client.key")),
            mtls_rotation: TemporalTlsRotation::Dynamic,
            dns_load_balancing: false,
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(dynamic_valid.mtls_rotation(), TemporalTlsRotation::Dynamic);
}

#[test]
fn mtls_rotation_deserializes_snake_case_values_and_rejects_invalid() {
    let static_cfg = load_env(&[
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", "https://temporal.example:7233"),
        (
            "APP__TEMPORAL__CLIENT_CERT_PATH",
            "/etc/temporal/client.crt",
        ),
        ("APP__TEMPORAL__CLIENT_KEY_PATH", "/etc/temporal/client.key"),
        ("APP__TEMPORAL__MTLS_ROTATION", "static"),
    ]);
    assert_eq!(
        static_cfg.temporal().unwrap().mtls_rotation(),
        TemporalTlsRotation::Static
    );

    let dynamic_cfg = load_env(&[
        ("APP__TEMPORAL__ENABLED", "true"),
        ("APP__TEMPORAL__ENDPOINT", "https://temporal.example:7233"),
        (
            "APP__TEMPORAL__CLIENT_CERT_PATH",
            "/etc/temporal/client.crt",
        ),
        ("APP__TEMPORAL__CLIENT_KEY_PATH", "/etc/temporal/client.key"),
        ("APP__TEMPORAL__DNS_LOAD_BALANCING", "false"),
        ("APP__TEMPORAL__MTLS_ROTATION", "dynamic"),
    ]);
    assert_eq!(
        dynamic_cfg.temporal().unwrap().mtls_rotation(),
        TemporalTlsRotation::Dynamic
    );

    let dir = tempfile::tempdir().unwrap();
    let toml_file = write(
        &dir,
        "mtls-dynamic.toml",
        concat!(
            "[temporal]\n",
            "enabled = true\n",
            "endpoint = \"https://temporal.example:7233\"\n",
            "client_cert_path = \"/etc/temporal/client.crt\"\n",
            "client_key_path = \"/etc/temporal/client.key\"\n",
            "dns_load_balancing = false\n",
            "mtls_rotation = \"dynamic\"\n",
        ),
    );
    let toml_cfg = load_from(
        &LoadOptions {
            config: Some(toml_file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    assert_eq!(
        toml_cfg.temporal().unwrap().mtls_rotation(),
        TemporalTlsRotation::Dynamic
    );

    for invalid in ["Dynamic", "Static", "rolling"] {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__TEMPORAL__MTLS_ROTATION", invalid)]),
        )
        .unwrap_err();
        assert_matches!(err.0, ErrorKind::Deserialize(_), "{invalid:?}: {err}");
    }
}

#[test]
fn proxy_url_mutual_exclusion_and_blank_path_clearing() {
    let conflict = admit(
        TemporalInput {
            dns_load_balancing: true,
            http_proxy_url: Some("http://proxy.internal:8080".to_owned()),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(conflict.names("temporal.http_proxy_url"), "{conflict}");

    for bad_proxy in [
        "https://proxy.internal:8443",
        "http://proxy.internal:8080#frag",
        "http://proxy.internal:8080?q=1",
        "http://user:secret-proxy-pass@proxy.internal:8080",
        "http://user@proxy.internal:8080",
        "http://@proxy.internal:8080",
        " http://proxy.internal:8080",
        "socks5://proxy.internal:1080",
    ] {
        let errors = admit(
            TemporalInput {
                dns_load_balancing: false,
                http_proxy_url: Some(bad_proxy.to_owned()),
                ..enabled_https_input()
            },
            false,
        )
        .unwrap_err();
        assert!(
            errors.names("temporal.http_proxy_url"),
            "{bad_proxy:?}: {errors}"
        );
        assert!(
            !errors.to_string().contains("secret-proxy-pass"),
            "{bad_proxy:?}: {errors}"
        );
    }

    let valid_proxy = admit(
        TemporalInput {
            dns_load_balancing: false,
            http_proxy_url: Some("http://proxy.internal:8080".to_owned()),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        valid_proxy.http_proxy_url().map(Url::as_str),
        Some("http://proxy.internal:8080/")
    );

    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "temporal.toml",
        concat!(
            "[temporal]\n",
            "enabled = true\n",
            "endpoint = \"https://temporal.example:7233\"\n",
            "client_cert_path = \"/etc/temporal/client.crt\"\n",
            "client_key_path = \"/etc/temporal/client.key\"\n",
            "root_ca_path = \"/etc/temporal/ca.crt\"\n",
            "api_key_file = \"/run/secrets/key\"\n",
        ),
    );
    let cfg = load_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            ("APP__TEMPORAL__API_KEY", "inline-token"),
            ("APP__TEMPORAL__API_KEY_FILE", "   "),
            ("APP__TEMPORAL__CLIENT_CERT_PATH", ""),
            ("APP__TEMPORAL__CLIENT_KEY_PATH", " "),
            ("APP__TEMPORAL__ROOT_CA_PATH", "\t"),
        ]),
    )
    .unwrap();
    let temporal = cfg.temporal().unwrap();
    assert_eq!(temporal.api_key_file(), None);
    assert_eq!(temporal.client_cert_path(), None);
    assert_eq!(temporal.client_key_path(), None);
    assert_eq!(temporal.root_ca_path(), None);
}

#[test]
fn namespace_and_identity_enforce_length_non_blank_and_character_rules() {
    let max_ok = "a".repeat(255);
    let too_long = "a".repeat(256);

    let trimmed = admit(
        TemporalInput {
            namespace: " prod-ns ".to_owned(),
            identity: Some(max_ok.clone()),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(trimmed.namespace(), "prod-ns");
    assert_eq!(trimmed.identity(), max_ok);

    for bad_ns in [
        "",
        "   ",
        "ns\n",
        "ns\t",
        "ns\x7f",
        "café",
        too_long.as_str(),
    ] {
        let errors = admit(
            TemporalInput {
                namespace: bad_ns.to_owned(),
                ..enabled_https_input()
            },
            false,
        )
        .unwrap_err();
        assert!(errors.names("temporal.namespace"), "{bad_ns:?}: {errors}");
    }

    for bad_id in ["", "   ", "id\n", "id\0", too_long.as_str()] {
        let errors = admit(
            TemporalInput {
                identity: Some(bad_id.to_owned()),
                ..enabled_https_input()
            },
            false,
        )
        .unwrap_err();
        assert!(errors.names("temporal.identity"), "{bad_id:?}: {errors}");
    }
}

#[test]
fn duration_and_payload_bounds_and_keep_alive_ordering_are_enforced() {
    let min_ok = admit(
        TemporalInput {
            credential_refresh_interval: Duration::from_secs(5),
            connect_timeout: Duration::from_millis(100),
            rpc_timeout: Duration::from_millis(100),
            keep_alive_interval: Duration::from_secs(10),
            keep_alive_timeout: Duration::from_secs(1),
            max_payload_bytes: ByteSize::kib(1),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(min_ok.max_payload_bytes(), 1024);

    let max_ok = admit(
        TemporalInput {
            credential_refresh_interval: Duration::from_mins(15),
            connect_timeout: Duration::from_secs(60),
            rpc_timeout: Duration::from_mins(5),
            keep_alive_interval: Duration::from_mins(5),
            keep_alive_timeout: Duration::from_secs(60),
            max_payload_bytes: ByteSize::mib(2),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(max_ok.max_payload_bytes(), 2 * 1024 * 1024);

    let too_small = admit(
        TemporalInput {
            credential_refresh_interval: Duration::from_secs(4),
            connect_timeout: Duration::from_millis(99),
            rpc_timeout: Duration::from_millis(99),
            keep_alive_interval: Duration::from_secs(9),
            keep_alive_timeout: Duration::from_millis(999),
            max_payload_bytes: ByteSize::b(1023),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    for key in [
        "temporal.credential_refresh_interval",
        "temporal.connect_timeout",
        "temporal.rpc_timeout",
        "temporal.keep_alive_interval",
        "temporal.keep_alive_timeout",
        "temporal.max_payload_bytes",
    ] {
        assert!(too_small.names(key), "missing {key}: {too_small}");
    }

    let too_large = admit(
        TemporalInput {
            credential_refresh_interval: Duration::from_secs(901),
            connect_timeout: Duration::from_secs(61),
            rpc_timeout: Duration::from_secs(301),
            keep_alive_interval: Duration::from_secs(301),
            keep_alive_timeout: Duration::from_secs(61),
            max_payload_bytes: ByteSize::b(2 * 1024 * 1024 + 1),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    for key in [
        "temporal.credential_refresh_interval",
        "temporal.connect_timeout",
        "temporal.rpc_timeout",
        "temporal.keep_alive_interval",
        "temporal.keep_alive_timeout",
        "temporal.max_payload_bytes",
    ] {
        assert!(too_large.names(key), "missing {key}: {too_large}");
    }

    let equal_ka = admit(
        TemporalInput {
            keep_alive_interval: Duration::from_secs(30),
            keep_alive_timeout: Duration::from_secs(30),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(equal_ka.names("temporal.keep_alive_timeout"), "{equal_ka}");

    let greater_ka = admit(
        TemporalInput {
            keep_alive_interval: Duration::from_secs(30),
            keep_alive_timeout: Duration::from_secs(31),
            ..enabled_https_input()
        },
        false,
    )
    .unwrap_err();
    assert!(
        greater_ka.names("temporal.keep_alive_timeout"),
        "{greater_ka}"
    );
}

#[test]
fn unknown_temporal_key_fails_closed() {
    assert_unknown_env_key_fails("APP__TEMPORAL__BOGUS");
}
