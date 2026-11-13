use super::*;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};
use crate::{HttpConfig, LoadOptions};
use std::assert_matches;

fn parse(toml: &str) -> Result<GrpcConfig, toml::de::Error> {
    toml::from_str(toml)
}

fn default_drain_budget() -> Duration {
    HttpConfig::default().effective_drain_budget()
}

#[test]
fn disabled_listener_is_inert_by_default_and_redacts_all_material() {
    let config = GrpcConfig::default();
    config.validate(default_drain_budget()).unwrap();
    let debug = format!("{config:?}");
    assert!(debug.contains("enabled"));
    assert!(!debug.contains("certificate"));
    assert!(!debug.contains("private_key"));
    assert!(!debug.contains("client_ca"));
}

#[test]
fn enabled_plaintext_requires_an_address_and_explicit_security() {
    let missing = parse("enabled = true").unwrap();
    assert_eq!(
        missing.validate(default_drain_budget()).unwrap_err().key,
        "grpc.addr"
    );

    let missing = parse("enabled = true\naddr = \"127.0.0.1:50051\"").unwrap();
    assert_eq!(
        missing.validate(default_drain_budget()).unwrap_err().key,
        "grpc.security"
    );

    let config = parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"").unwrap();
    config.validate(default_drain_budget()).unwrap();
}

#[test]
fn tls_requires_certificate_and_environment_only_private_key() {
    let config = parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"").unwrap();
    assert_eq!(
        config.validate(default_drain_budget()).unwrap_err().key,
        "grpc.certificate"
    );

    let config =
        parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"\ncertificate = \"cert\"")
            .unwrap();
    assert_eq!(
        config.validate(default_drain_budget()).unwrap_err().key,
        "grpc.private_key"
    );
}

fn enabled() -> GrpcConfig {
    parse("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"").unwrap()
}

#[test]
fn limits_default_to_the_http_bounds_and_zero_turns_one_off() {
    let config = enabled();
    assert_eq!(config.request_timeout, Duration::from_secs(8));
    assert_eq!(config.in_flight_cap().map(NonZeroU32::get), Some(256));
    assert_eq!(config.admission_wait, Duration::from_secs(1));
    assert_eq!(config.connection_cap().map(NonZeroU32::get), Some(4096));
    assert_eq!(config.connection_age_cap(), Some(Duration::from_mins(30)));

    let unbounded = GrpcConfig {
        max_in_flight: 0,
        max_connections: 0,
        max_connection_age: Duration::ZERO,
        ..enabled()
    };
    unbounded.validate(default_drain_budget()).unwrap();
    assert_eq!(unbounded.in_flight_cap(), None);
    assert_eq!(unbounded.connection_cap(), None);
    assert_eq!(unbounded.connection_age_cap(), None);
}

#[test]
fn the_admission_wait_may_not_outlast_the_call_budget() {
    let longest = GrpcConfig {
        admission_wait: Duration::from_secs(8),
        ..enabled()
    };
    longest.validate(default_drain_budget()).unwrap();
    let none = GrpcConfig {
        admission_wait: Duration::ZERO,
        ..enabled()
    };
    none.validate(default_drain_budget()).unwrap();
    let beyond = GrpcConfig {
        admission_wait: Duration::from_millis(8_001),
        ..enabled()
    };
    assert_eq!(
        beyond.validate(default_drain_budget()).unwrap_err().key,
        "grpc.admission_wait"
    );
}

#[test]
fn a_call_budget_must_fit_inside_the_shared_drain() {
    let config = GrpcConfig {
        request_timeout: Duration::from_secs(11),
        ..enabled()
    };
    let err = config.validate(default_drain_budget()).unwrap_err();
    assert_eq!(err.key, "grpc.request_timeout");

    let http = HttpConfig {
        drain_timeout: Duration::from_secs(40),
        ..HttpConfig::default()
    };
    config.validate(http.effective_drain_budget()).unwrap();
}

#[test]
fn a_connection_age_under_a_second_is_refused() {
    let config = GrpcConfig {
        max_connection_age: Duration::from_millis(10),
        ..enabled()
    };
    let err = config.validate(default_drain_budget()).unwrap_err();
    assert_eq!(err.key, "grpc.max_connection_age");
}

#[test]
fn plaintext_refuses_tls_material() {
    let config = parse(
        "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"\ncertificate = \"cert\"",
    )
    .unwrap();
    assert_eq!(
        config.validate(default_drain_budget()).unwrap_err().key,
        "grpc.certificate"
    );
}

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
    assert_eq!(cfg.grpc.required_addr().unwrap().port(), 0);
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
        ("APP__GRPC__ADMISSION_WAIT", "2s"),
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
    assert_eq!(cfg.grpc.admission_wait, Duration::from_secs(2));
    assert_eq!(cfg.grpc.connection_cap(), None);
    assert_eq!(cfg.grpc.connection_age_cap(), Some(Duration::from_mins(5)));

    let over_drain = [("APP__GRPC__REQUEST_TIMEOUT", "11s")];
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[listener.as_slice(), over_drain.as_slice()].concat()),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Validate(error) if error.key == "grpc.request_timeout", "{err}");
}

#[rstest::rstest]
#[case::in_flight("max_in_flight")]
#[case::connections("max_connections")]
fn grpc_limits_reject_negative_toml_floats(#[case] key: &str) {
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "bad.toml", &format!("[grpc]\n{key} = -1.0\n"));
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
    assert!(err.to_string().contains(&format!("grpc.{key}")), "{err}");
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
    assert_matches!(&err, Error::SecretInFile { key, .. } if key == "grpc.private_key", "{err}");
}
