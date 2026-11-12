use std::assert_matches;
use std::num::NonZeroU32;

use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};

#[test]
fn defaults_are_inactive_and_within_the_delivery_budget() {
    let config = MessagingConfig::default();
    assert!(!config.is_active());
    assert!(!config.tls_first);
    assert_eq!(config.max_payload_bytes, ByteSize::kib(256));
    assert_eq!(config.consumer_concurrency, NonZeroU32::MIN);
    config.validate("production").unwrap();
}

#[test]
fn default_delivery_fits_a_broker_with_the_default_payload_limit() {
    // nats-server's default `max_payload`, which bounds payload and
    // headers together.
    const BROKER_DEFAULT_MAX_PAYLOAD: u64 = 1024 * 1024;
    let config = MessagingConfig::default();
    assert!(
        config.max_payload_bytes.as_u64() + DELIVERY_OVERHEAD_BYTES <= BROKER_DEFAULT_MAX_PAYLOAD
    );
}

#[test]
fn a_credentials_file_is_the_alternative_to_inline_credentials() {
    let mut config = MessagingConfig {
        urls: vec!["tls://nats.example:4222".to_owned()],
        credentials_file: Some(PathBuf::from("/run/secrets/nats.creds")),
        source_stream: Some("events".to_owned()),
        ..MessagingConfig::default()
    };
    config.validate_producer("production").unwrap();

    config.credentials = Some(SecretString::from("fixture-credentials".to_owned()));
    assert_eq!(
        config.validate_producer("production").unwrap_err().key,
        "messaging.credentials_file"
    );
}

#[test]
fn producer_requires_admitted_connection_credentials_and_source() {
    let mut config = MessagingConfig {
        urls: vec!["tls://nats.example:4222".to_owned()],
        ..MessagingConfig::default()
    };
    assert_eq!(
        config.validate_producer("production").unwrap_err().key,
        "messaging.credentials"
    );
    config.credentials = Some(SecretString::from("fixture-credentials".to_owned()));
    assert_eq!(
        config.validate_producer("production").unwrap_err().key,
        "messaging.source_stream"
    );
    config.source_stream = Some("events".to_owned());
    config.validate_producer("production").unwrap();
    assert!(!format!("{config:?}").contains("fixture-credentials"));
}

#[test]
fn consumer_requires_its_distinct_operator_owned_names() {
    let config = MessagingConfig {
        urls: vec!["tls://nats.example:4222".to_owned()],
        credentials: Some(SecretString::from("fixture-credentials".to_owned())),
        source_stream: Some("events".to_owned()),
        ..MessagingConfig::default()
    };
    assert_eq!(
        config.validate_consumer("production").unwrap_err().key,
        "messaging.consumer_durable"
    );
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
        let config = MessagingConfig {
            urls: vec![url.to_owned()],
            ..MessagingConfig::default()
        };
        assert_eq!(config.validate("local").unwrap_err().key, key, "{url}");
    }
    let config = MessagingConfig {
        urls: vec!["nats://nats.example:4222".to_owned()],
        ..MessagingConfig::default()
    };
    assert_eq!(
        config.validate("production").unwrap_err().key,
        "messaging.allow_plaintext"
    );
}

#[test]
fn plaintext_and_unauthenticated_are_only_available_in_local_development() {
    let config = MessagingConfig {
        urls: vec!["nats://127.0.0.1:4222".to_owned()],
        allow_plaintext: true,
        allow_unauthenticated: true,
        source_stream: Some("events".to_owned()),
        ..MessagingConfig::default()
    };
    config.validate_producer("local").unwrap();
    assert_eq!(
        config.validate("production").unwrap_err().key,
        "messaging.allow_plaintext"
    );
}

#[test]
fn trusted_network_admits_plaintext_but_keeps_credentials_required() {
    let mut config = MessagingConfig {
        urls: vec!["nats://nats.internal:4222".to_owned()],
        trusted_network: true,
        source_stream: Some("events".to_owned()),
        ..MessagingConfig::default()
    };
    assert_eq!(
        config.validate_producer("production").unwrap_err().key,
        "messaging.credentials"
    );
    config.credentials = Some(SecretString::from("fixture-credentials".to_owned()));
    config.validate_producer("production").unwrap();
    assert!(config.plaintext_admitted());
    config.allow_unauthenticated = true;
    assert_eq!(
        config.validate_producer("production").unwrap_err().key,
        "messaging.allow_unauthenticated"
    );
}

#[test]
fn tls_first_requires_all_configured_seeds_to_use_tls() {
    for urls in [
        vec!["nats://broker.example:4222"],
        vec!["tls://secure.example:4222", "nats://broker.example:4222"],
    ] {
        let config = MessagingConfig {
            urls: urls.into_iter().map(str::to_owned).collect(),
            tls_first: true,
            trusted_network: true,
            ..MessagingConfig::default()
        };
        assert_eq!(
            config.validate("production").unwrap_err().key,
            "messaging.tls_first"
        );
    }
    MessagingConfig {
        urls: vec!["tls://secure.example:4222".to_owned()],
        tls_first: true,
        ..MessagingConfig::default()
    }
    .validate("production")
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
    let config = MessagingConfig {
        max_payload_bytes: ByteSize::b(max_payload_bytes),
        consumer_concurrency: NonZeroU32::new(concurrency).unwrap(),
        ..MessagingConfig::default()
    };
    match config.validate("production") {
        Ok(()) => assert!(accepted),
        Err(error) => {
            assert!(!accepted);
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
            ("APP__MESSAGING__TLS_FIRST", "true"),
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
    assert!(cfg.messaging.tls_first);
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

    for (blank, key) in [
        ("", "messaging.urls[0]"),
        ("tls://nats-a.example:4222,", "messaging.urls[1]"),
    ] {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__MESSAGING__URLS", blank)]),
        )
        .unwrap_err();
        assert_matches!(&err, Error::Validate(error) if error.key == key, "{err}");
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
    assert_matches!(&err, Error::SecretInFile { key, .. } if key == "messaging.credentials", "{err}");
}

#[test]
fn messaging_unknown_environment_key_fails_loading() {
    assert_matches!(
        load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[("APP__MESSAGING__UNKNOWN", "x")]),
        ),
        Err(Error::Deserialize(_))
    );
}
