use std::assert_matches;

use secrecy::ExposeSecret as _;

use super::*;
use crate::LoadOptions;
use crate::load::{ErrorKind, load_from, load_worker_from};
use crate::test_support::{BUILD, assert_secret_in_file, env, load_worker_env, write};
use crate::validate::{ValidationErrors, admit_with};

fn admit(
    input: MessagingInput,
    insecure_development: bool,
) -> Result<MessagingConfig, ValidationErrors> {
    admit_with(|v| input.admit(insecure_development, v))
}

fn tls_input() -> MessagingInput {
    MessagingInput {
        urls: vec!["tls://nats.example:4222".to_owned()],
        ..MessagingInput::default()
    }
}

fn fixture_credentials() -> SecretString {
    SecretString::from("fixture-credentials".to_owned())
}

#[test]
fn defaults_are_inactive_and_within_the_delivery_budget() {
    let config = admit(MessagingInput::default(), false).unwrap();
    assert_eq!(config.urls, Vec::<String>::new());
    assert_eq!(config.security, MessagingTransportSecurity::Tls);
    assert_eq!(config.max_payload_bytes, 256 * 1024);
    assert_eq!(config.consumer_concurrency, NonZeroUsize::MIN);
    assert!(config.credentials.is_none());
}

#[test]
fn default_delivery_fits_a_broker_with_the_default_payload_limit() {
    const BROKER_DEFAULT_MAX_PAYLOAD: u64 = 1024 * 1024;
    let config = admit(MessagingInput::default(), false).unwrap();
    assert!(
        u64::try_from(config.max_payload_bytes).unwrap() + DELIVERY_OVERHEAD_BYTES
            <= BROKER_DEFAULT_MAX_PAYLOAD
    );
}

#[test]
fn a_credentials_file_is_the_alternative_to_inline_credentials() {
    let input = MessagingInput {
        credentials_file: Some(PathBuf::from("/run/secrets/nats.creds")),
        source_stream: Some("events".to_owned()),
        ..tls_input()
    };
    let config = admit(input, false).unwrap();
    assert_matches!(
        &config.credentials,
        Some(MessagingCredentials::File(path)) if path == Path::new("/run/secrets/nats.creds")
    );
    let admitted = config.admit_producer().unwrap();
    assert_matches!(
        admitted.credentials,
        MessagingCredentials::File(path) if path == Path::new("/run/secrets/nats.creds")
    );

    let both = MessagingInput {
        credentials: Some(fixture_credentials()),
        credentials_file: Some(PathBuf::from("/run/secrets/nats.creds")),
        ..tls_input()
    };
    assert!(
        admit(both, false)
            .unwrap_err()
            .names("messaging.credentials_file")
    );
}

#[test]
fn producer_requires_admitted_connection_credentials_and_source() {
    let mut config = admit(tls_input(), false).unwrap();
    assert_eq!(
        config.admit_producer().unwrap_err().key,
        "messaging.credentials"
    );
    config.credentials = Some(MessagingCredentials::Inline(fixture_credentials()));
    assert_eq!(
        config.admit_producer().unwrap_err().key,
        "messaging.source_stream"
    );
    config.source_stream = Some("events".to_owned());
    let admitted = config.admit_producer().unwrap();
    assert_eq!(admitted.source_stream, "events");
    assert_eq!(admitted.urls, ["tls://nats.example:4222"]);
    assert_eq!(admitted.security, MessagingTransportSecurity::Tls);
    assert!(!format!("{config:?}").contains("fixture-credentials"));
    assert!(!format!("{admitted:?}").contains("fixture-credentials"));
    assert!(!format!("{admitted:?}").contains("nats.example"));
    assert_eq!(
        admit(MessagingInput::default(), false)
            .unwrap()
            .admit_producer()
            .unwrap_err()
            .key,
        "messaging.urls"
    );
}

#[test]
fn consumer_requires_its_distinct_operator_owned_names() {
    let mut config = admit(
        MessagingInput {
            source_stream: Some("events".to_owned()),
            ..tls_input()
        },
        false,
    )
    .unwrap();
    config.credentials = Some(MessagingCredentials::Inline(fixture_credentials()));
    assert_eq!(
        config.admit_consumer().unwrap_err().key,
        "messaging.consumer_durable"
    );
    config.consumer_durable = Some("worker".to_owned());
    assert_eq!(
        config.admit_consumer().unwrap_err().key,
        "messaging.consumer_filter_subject"
    );
    config.consumer_filter_subject = Some("events.created".to_owned());
    assert_eq!(
        config.admit_consumer().unwrap_err().key,
        "messaging.dlq_subject"
    );
    config.dlq_subject = Some("dead.created".to_owned());
    let admitted = config.admit_consumer().unwrap();
    assert_eq!(admitted.messaging.source_stream, "events");
    assert_eq!(admitted.durable, "worker");
    assert_eq!(admitted.filter_subject, "events.created");
    assert_eq!(admitted.dlq_subject, "dead.created");
    assert_eq!(admitted.concurrency, NonZeroUsize::MIN);
}

#[test]
fn rejects_url_userinfo_unknown_protocol_and_unapproved_plaintext() {
    for (url, key) in [
        ("tls://user:password@nats.example:4222", "messaging.urls[0]"),
        ("tls://@nats.example:4222", "messaging.urls[0]"),
        ("tls://nats.exa\tmple:4222", "messaging.urls[0]"),
        ("https://nats.example:4222", "messaging.urls[0]"),
        ("nats://nats.example:4222", "messaging.allow_plaintext"),
    ] {
        let input = MessagingInput {
            urls: vec![url.to_owned()],
            ..MessagingInput::default()
        };
        let errors = admit(input, true).unwrap_err();
        assert!(errors.names(key), "{url}");
    }
    let input = MessagingInput {
        urls: vec!["nats://nats.example:4222".to_owned()],
        ..MessagingInput::default()
    };
    assert!(
        admit(input, false)
            .unwrap_err()
            .names("messaging.allow_plaintext")
    );
}

#[test]
fn every_invalid_url_is_reported() {
    let input = MessagingInput {
        urls: vec![
            "https://a.example:4222".to_owned(),
            "tls://b.example:4222".to_owned(),
            "tls://@c.example:4222".to_owned(),
        ],
        ..MessagingInput::default()
    };
    let errors = admit(input, false).unwrap_err();
    assert!(errors.names("messaging.urls[0]"));
    assert!(!errors.names("messaging.urls[1]"));
    assert!(errors.names("messaging.urls[2]"));
}

#[test]
fn plaintext_and_unauthenticated_exceptions_require_insecure_development() {
    let local = [
        ("APP__APP__ENV", "development"),
        ("APP__MESSAGING__URLS", "nats://127.0.0.1:4222"),
        ("APP__MESSAGING__ALLOW_PLAINTEXT", "true"),
        ("APP__MESSAGING__ALLOW_UNAUTHENTICATED", "true"),
        ("APP__MESSAGING__SOURCE_STREAM", "events"),
    ];
    let options = LoadOptions::default();
    let error = load_worker_from(&options, BUILD, env(&local)).unwrap_err();
    assert!(
        error
            .validation_errors()
            .is_some_and(|errors| errors.names("messaging.allow_plaintext")
                && errors.names("messaging.allow_unauthenticated")),
        "{error}"
    );
    let config = load_worker_from(
        &options,
        BUILD,
        env(&[
            local.as_slice(),
            &[("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true")],
        ]
        .concat()),
    )
    .unwrap();
    assert_eq!(
        config.messaging().security,
        MessagingTransportSecurity::AllowPlaintext
    );
    assert_matches!(
        config.messaging().credentials,
        Some(MessagingCredentials::Unauthenticated)
    );
    config.messaging().admit_producer().unwrap();
}

#[test]
fn trusted_network_admits_plaintext_but_keeps_credentials_required() {
    let input = MessagingInput {
        urls: vec!["nats://nats.internal:4222".to_owned()],
        trusted_network: true,
        source_stream: Some("events".to_owned()),
        ..MessagingInput::default()
    };
    let mut config = admit(input, false).unwrap();
    assert_eq!(
        config.admit_producer().unwrap_err().key,
        "messaging.credentials"
    );
    config.credentials = Some(MessagingCredentials::Inline(fixture_credentials()));
    let admitted = config.admit_producer().unwrap();
    assert_eq!(
        admitted.security,
        MessagingTransportSecurity::AllowPlaintext
    );

    let unauthenticated = MessagingInput {
        urls: vec!["nats://nats.internal:4222".to_owned()],
        trusted_network: true,
        allow_unauthenticated: true,
        ..MessagingInput::default()
    };
    assert!(
        admit(unauthenticated, false)
            .unwrap_err()
            .names("messaging.allow_unauthenticated")
    );
}

#[test]
fn tls_first_requires_all_configured_seeds_to_use_tls() {
    for urls in [
        vec!["nats://broker.example:4222"],
        vec!["tls://secure.example:4222", "nats://broker.example:4222"],
    ] {
        let input = MessagingInput {
            urls: urls.into_iter().map(str::to_owned).collect(),
            tls_first: true,
            trusted_network: true,
            ..MessagingInput::default()
        };
        assert!(
            admit(input, false)
                .unwrap_err()
                .names("messaging.tls_first")
        );
    }
    admit(
        MessagingInput {
            tls_first: true,
            ..tls_input()
        },
        false,
    )
    .unwrap();
}

#[rstest::rstest]
#[case::over_budget(1024 * 1024, 64, false)]
#[case::at_budget(32 * 1024 * 1024 - 8192, 2, true)]
#[case::over_budget_by_one_byte_per_delivery(32 * 1024 * 1024 - 8191, 2, false)]
fn resident_delivery_memory_is_bounded(
    #[case] max_payload_bytes: u64,
    #[case] concurrency: u32,
    #[case] accepted: bool,
) {
    let input = MessagingInput {
        max_payload_bytes: ByteSize::b(max_payload_bytes),
        consumer_concurrency: NonZeroU32::new(concurrency).unwrap(),
        ..MessagingInput::default()
    };
    match admit(input, false) {
        Ok(_) => assert!(accepted),
        Err(errors) => {
            assert!(!accepted);
            let [error] = errors.iter().collect::<Vec<_>>()[..] else {
                panic!("expected exactly one violation: {errors}");
            };
            assert_eq!(error.key, "messaging.consumer_concurrency");
            let resident = u64::from(concurrency) * (max_payload_bytes + 8 * 1024);
            assert_eq!(
                error.message,
                format!(
                    "{concurrency} × (messaging.max_payload_bytes {max_payload_bytes} + 8 KiB headers) = {resident} bytes exceeds the 64 MiB resident delivery bound"
                )
            );
        }
    }
}

#[test]
fn messaging_environment_decodes_lists_and_redacts_credentials() {
    let cfg = load_worker_env(&[
        (
            "APP__MESSAGING__URLS",
            "tls://nats-a.example:4222,tls://nats-b.example:4222",
        ),
        ("APP__MESSAGING__CREDENTIALS", "fixture-credentials"),
        ("APP__MESSAGING__ROOT_CA_PATH", "/etc/nats/root-ca.pem"),
        ("APP__MESSAGING__TLS_FIRST", "true"),
        ("APP__MESSAGING__SOURCE_STREAM", "events"),
        ("APP__MESSAGING__MAX_PAYLOAD_BYTES", "2 MiB"),
        ("APP__MESSAGING__CONSUMER_DURABLE", "service-events"),
        ("APP__MESSAGING__CONSUMER_FILTER_SUBJECT", "events.>"),
        ("APP__MESSAGING__DLQ_SUBJECT", "events.dlq"),
        ("APP__MESSAGING__CONSUMER_CONCURRENCY", "2"),
    ]);
    assert_eq!(
        cfg.messaging().urls,
        vec![
            "tls://nats-a.example:4222".to_owned(),
            "tls://nats-b.example:4222".to_owned(),
        ]
    );
    assert_matches!(
        &cfg.messaging().credentials,
        Some(MessagingCredentials::Inline(secret)) if secret.expose_secret() == "fixture-credentials"
    );
    assert_eq!(
        cfg.messaging().root_ca_path.as_deref(),
        Some(Path::new("/etc/nats/root-ca.pem"))
    );
    assert_eq!(cfg.messaging().max_payload_bytes(), 2 * 1024 * 1024);
    assert_eq!(cfg.messaging().consumer_concurrency.get(), 2);
    assert_eq!(
        cfg.messaging().security,
        MessagingTransportSecurity::TlsFirst
    );
    let rendered = format!("{cfg:?}");
    assert!(!rendered.contains("fixture-credentials"));
    assert!(!rendered.contains("nats-a.example"));
}

#[test]
fn messaging_trusted_network_admits_plaintext_in_production() {
    let cfg = load_worker_env(&[
        ("APP__APP__ENV", "production"),
        ("APP__MESSAGING__URLS", "nats://nats.internal:4222"),
        ("APP__MESSAGING__TRUSTED_NETWORK", "true"),
        ("APP__MESSAGING__CREDENTIALS", "fixture-credentials"),
        ("APP__MESSAGING__SOURCE_STREAM", "events"),
    ]);
    assert_eq!(
        cfg.messaging().security,
        MessagingTransportSecurity::AllowPlaintext
    );
    cfg.messaging().admit_producer().unwrap();
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
    let cfg = load_worker_from(&options, BUILD, env(&[])).unwrap();
    assert_matches!(
        &cfg.messaging().credentials,
        Some(MessagingCredentials::File(path)) if path == Path::new("/run/secrets/from-file.creds")
    );

    let cfg = load_worker_from(
        &options,
        BUILD,
        env(&[(
            "APP__MESSAGING__CREDENTIALS_FILE",
            "/run/secrets/nats.creds",
        )]),
    )
    .unwrap();
    assert_matches!(
        &cfg.messaging().credentials,
        Some(MessagingCredentials::File(path)) if path == Path::new("/run/secrets/nats.creds")
    );

    let cfg = load_worker_from(
        &options,
        BUILD,
        env(&[("APP__MESSAGING__CREDENTIALS_FILE", " ")]),
    )
    .unwrap();
    assert_matches!(cfg.messaging().credentials, None);
}

#[test]
fn messaging_empty_root_ca_path_unsets_the_file_value() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "ca.toml",
        "[messaging]\nroot_ca_path = \"/etc/nats/root-ca.pem\"\n",
    );
    let cfg = load_worker_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[("APP__MESSAGING__ROOT_CA_PATH", "")]),
    )
    .unwrap();
    assert_eq!(cfg.messaging().root_ca_path, None);
}

#[test]
fn messaging_urls_accept_a_file_list_and_refuse_other_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let list = write(
        &dir,
        "list.toml",
        "[messaging]\nurls = [\"tls://nats-a.example:4222\", \"tls://nats-b.example:4222\"]\n",
    );
    let cfg = load_worker_from(
        &LoadOptions {
            config: Some(list.clone()),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    assert_eq!(cfg.messaging().urls.len(), 2);

    let overridden = load_worker_from(
        &LoadOptions {
            config: Some(list),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[("APP__MESSAGING__URLS", "tls://nats-c.example:4222")]),
    )
    .unwrap();
    assert_eq!(overridden.messaging().urls, ["tls://nats-c.example:4222"]);

    let number = write(&dir, "number.toml", "[messaging]\nurls = 4222\n");
    let err = load_worker_from(
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
            .contains("a list of strings or a comma-separated string"),
        "{err}"
    );

    for (blank, key) in [
        ("", "messaging.urls[0]"),
        ("tls://nats-a.example:4222,", "messaging.urls[1]"),
    ] {
        let options = LoadOptions::default();
        let environment = [("APP__MESSAGING__URLS", blank)];
        load_from(&options, BUILD, env(&environment)).unwrap();
        let err = load_worker_from(&options, BUILD, env(&environment)).unwrap_err();
        assert!(
            err.validation_errors()
                .is_some_and(|error| error.names(key)),
            "{err}"
        );
    }
}

#[test]
fn messaging_credentials_preserve_scalar_looking_strings() {
    for secret in ["00123", "TRUE"] {
        let cfg = load_worker_env(&[
            ("APP__MESSAGING__URLS", "tls://nats.example:4222"),
            ("APP__MESSAGING__CREDENTIALS", secret),
        ]);
        assert_eq!(cfg.messaging().urls, ["tls://nats.example:4222"]);
        assert_matches!(
            &cfg.messaging().credentials,
            Some(MessagingCredentials::Inline(inline)) if inline.expose_secret() == secret
        );
    }
}

#[test]
fn messaging_credentials_in_a_file_are_refused() {
    assert_secret_in_file(
        "[messaging]\ncredentials = \"fixture-credentials\"\n",
        "messaging.credentials",
    );
}

#[test]
fn only_the_worker_decodes_messaging_settings() {
    for (name, value, expected) in [
        ("APP__MESSAGING__UNKNOWN", "x", "unknown field `unknown`"),
        (
            "APP__MESSAGING__CONSUMER_CONCURRENCY",
            "0",
            "messaging.consumer_concurrency",
        ),
        (
            "APP__MESSAGING__CONSUMER_CONCURRENCY",
            "yes",
            "messaging.consumer_concurrency",
        ),
    ] {
        let options = LoadOptions::default();
        let environment = [(name, value)];
        load_from(&options, BUILD, env(&environment)).unwrap();
        let err = load_worker_from(&options, BUILD, env(&environment)).unwrap_err();
        assert_matches!(err.0, ErrorKind::Deserialize(_), "{name}: {err}");
        assert!(err.to_string().contains(expected), "{name}: {err}");
    }
}
