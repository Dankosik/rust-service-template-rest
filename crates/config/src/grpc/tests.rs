use super::*;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, assert_secret_in_file, env, load_env, write};
use crate::validate::admit_with;
use crate::{HttpConfig, LoadOptions, ValidationErrors};
use std::assert_matches;

fn input(toml: &str) -> GrpcInput {
    toml::from_str(toml).unwrap()
}

fn default_drain_budget() -> Duration {
    HttpConfig::default().effective_drain_budget()
}

fn admit(input: GrpcInput) -> Result<Option<GrpcConfig>, ValidationErrors> {
    admit_with(|violations| input.admit(Some(default_drain_budget()), violations))
}

fn refused_keys(input: GrpcInput) -> Vec<String> {
    admit(input)
        .unwrap_err()
        .iter()
        .map(|error| error.key.clone())
        .collect()
}

fn enabled() -> GrpcInput {
    input("enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"plaintext\"")
}

#[test]
fn a_disabled_listener_is_absent_whatever_its_other_keys_say() {
    assert!(admit(GrpcInput::default()).unwrap().is_none());
    let ignored = GrpcInput {
        request_timeout: Duration::ZERO,
        certificate: Some("cert".to_owned()),
        ..GrpcInput::default()
    };
    assert!(admit(ignored).unwrap().is_none());
}

#[test]
fn an_enabled_listener_reports_every_missing_input_at_once() {
    assert_eq!(
        refused_keys(input("enabled = true")),
        ["grpc.addr", "grpc.security"]
    );
    assert_eq!(
        refused_keys(input("enabled = true\naddr = \"127.0.0.1:50051\"")),
        ["grpc.security"]
    );

    let config = admit(enabled()).unwrap().unwrap();
    assert_eq!(config.addr.port(), 0);
    assert_matches!(config.security, GrpcListenerSecurity::Plaintext);
}

#[test]
fn tls_requires_certificate_and_environment_only_private_key() {
    assert_eq!(
        refused_keys(input(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\""
        )),
        ["grpc.certificate", "grpc.private_key"]
    );

    let complete = GrpcInput {
        private_key: Some(SecretString::from("key")),
        ..input(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"\ncertificate = \"cert\"",
        )
    };
    let config = admit(complete).unwrap().unwrap();
    let GrpcListenerSecurity::Tls(material) = config.security else {
        panic!("tls was selected");
    };
    assert_eq!(material.certificate, "cert");
    assert_eq!(material.client_ca, None);
}

#[test]
fn limits_default_to_the_http_bounds_and_zero_turns_one_off() {
    let config = admit(enabled()).unwrap().unwrap();
    assert_eq!(config.request_timeout, Duration::from_secs(8));
    assert_eq!(config.in_flight_cap().map(NonZeroU32::get), Some(256));
    assert_eq!(config.admission_wait, Duration::from_secs(1));
    assert_eq!(config.connection_cap().map(NonZeroU32::get), Some(4096));
    assert_eq!(config.connection_age_cap(), Some(Duration::from_mins(30)));

    let unbounded = admit(GrpcInput {
        max_in_flight: 0,
        max_connections: 0,
        max_connection_age: Duration::ZERO,
        ..enabled()
    })
    .unwrap()
    .unwrap();
    assert_eq!(unbounded.in_flight_cap(), None);
    assert_eq!(unbounded.connection_cap(), None);
    assert_eq!(unbounded.connection_age_cap(), None);
}

#[test]
fn the_admission_wait_never_outlasts_the_call_budget() {
    let short_budget = admit(GrpcInput {
        request_timeout: Duration::from_millis(500),
        ..enabled()
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        short_budget.admission_wait_cap(),
        Duration::from_millis(500)
    );
    let immediate = admit(GrpcInput {
        admission_wait: Duration::ZERO,
        ..enabled()
    })
    .unwrap()
    .unwrap();
    assert_eq!(immediate.admission_wait_cap(), Duration::ZERO);
    assert_eq!(
        refused_keys(GrpcInput {
            admission_wait: Duration::from_mins(11),
            ..enabled()
        }),
        ["grpc.admission_wait"]
    );
}

#[test]
fn a_call_budget_must_fit_inside_the_shared_drain() {
    let over = || GrpcInput {
        request_timeout: Duration::from_secs(11),
        ..enabled()
    };
    assert_eq!(refused_keys(over()), ["grpc.request_timeout"]);

    let http = HttpConfig {
        drain_timeout: Duration::from_secs(40),
        ..HttpConfig::default()
    };
    admit_with(|violations| over().admit(Some(http.effective_drain_budget()), violations)).unwrap();
    admit_with(|violations| over().admit(None, violations)).unwrap();
}

#[test]
fn an_out_of_range_call_budget_is_the_only_violation_it_causes() {
    assert_eq!(
        refused_keys(GrpcInput {
            request_timeout: Duration::ZERO,
            ..enabled()
        }),
        ["grpc.request_timeout"]
    );
}

#[test]
fn a_connection_age_under_a_second_is_refused() {
    assert_eq!(
        refused_keys(GrpcInput {
            max_connection_age: Duration::from_millis(10),
            ..enabled()
        }),
        ["grpc.max_connection_age"]
    );
}

#[test]
fn plaintext_refuses_every_piece_of_tls_material() {
    let config = GrpcInput {
        certificate: Some("cert".to_owned()),
        private_key: Some(SecretString::from("key")),
        client_ca: Some("ca".to_owned()),
        ..enabled()
    };
    assert_eq!(
        refused_keys(config),
        ["grpc.certificate", "grpc.private_key", "grpc.client_ca"]
    );
}

#[test]
fn admitted_debug_names_the_mode_but_not_the_material() {
    let config = admit(GrpcInput {
        private_key: Some(SecretString::from("private-key-material")),
        ..input(
            "enabled = true\naddr = \"127.0.0.1:0\"\nsecurity = \"tls\"\ncertificate = \"public-cert\"\nclient_ca = \"client-ca\"",
        )
    })
    .unwrap()
    .unwrap();
    let debug = format!("{config:?}");
    assert!(debug.contains("tls"), "{debug}");
    for material in ["private-key-material", "public-cert", "client-ca"] {
        assert!(!debug.contains(material), "{debug}");
    }
}

#[test]
fn grpc_environment_enables_an_explicit_plaintext_listener() {
    let cfg = load_env(&[
        ("APP__GRPC__ENABLED", "true"),
        ("APP__GRPC__ADDR", "127.0.0.1:0"),
        ("APP__GRPC__SECURITY", "plaintext"),
    ]);
    let grpc = cfg.grpc.unwrap();
    assert_eq!(grpc.addr.port(), 0);
    assert_matches!(grpc.security, GrpcListenerSecurity::Plaintext);
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
    let grpc = cfg.grpc.unwrap();
    assert_eq!(grpc.request_timeout, Duration::from_secs(3));
    assert_eq!(grpc.in_flight_cap().map(NonZeroU32::get), Some(32));
    assert_eq!(grpc.admission_wait, Duration::from_secs(2));
    assert_eq!(grpc.connection_cap(), None);
    assert_eq!(grpc.connection_age_cap(), Some(Duration::from_mins(5)));

    let over_drain = [("APP__GRPC__REQUEST_TIMEOUT", "11s")];
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[listener.as_slice(), over_drain.as_slice()].concat()),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Validate(errors) if errors.names("grpc.request_timeout"), "{err}");
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

    assert_secret_in_file(
        "[grpc]\nprivate_key = \"private-key-material\"\n",
        "grpc.private_key",
    );
}
