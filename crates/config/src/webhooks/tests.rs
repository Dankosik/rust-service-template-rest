use std::assert_matches;
use std::num::NonZeroU32;

use secrecy::SecretString;

use super::{WebhookEndpointConfig, WebhooksConfig};
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};

#[test]
fn defaults_are_inert() {
    let config = WebhooksConfig::default();
    assert!(config.endpoints.is_empty());
    assert_eq!(config.max_concurrent_deliveries, None);
    config.validate().unwrap();
}

#[test]
fn a_zero_delivery_bound_fails_to_deserialize() {
    let err = toml::from_str::<WebhooksConfig>("max_concurrent_deliveries = 0").unwrap_err();
    assert!(
        err.to_string().contains("max_concurrent_deliveries"),
        "{err}"
    );
    let config = toml::from_str::<WebhooksConfig>("max_concurrent_deliveries = 4").unwrap();
    assert_eq!(
        config.max_concurrent_deliveries.map(NonZeroU32::get),
        Some(4)
    );
}

#[test]
fn rejects_an_empty_endpoint_id() {
    let mut config = WebhooksConfig::default();
    config.endpoints.insert(
        String::new(),
        WebhookEndpointConfig {
            url: "https://partner.example/events".to_owned(),
            secret: SecretString::from("whsec_current".to_owned()),
            previous_secret: None,
        },
    );

    let err = config.validate().unwrap_err();
    assert_eq!(err.key, "webhooks.endpoints");

    let endpoint = config.endpoints.remove("").unwrap();
    config.endpoints.insert("partner/a?#".to_owned(), endpoint);
    config.validate().unwrap();
}

#[test]
fn rejects_empty_signing_secrets() {
    let mut config = WebhooksConfig::default();
    config.endpoints.insert(
        "partner".to_owned(),
        WebhookEndpointConfig {
            url: "https://partner.example/events".to_owned(),
            secret: SecretString::from(String::new()),
            previous_secret: None,
        },
    );

    let err = config.validate().unwrap_err();
    assert_eq!(err.key, "webhooks.endpoints.partner.secret");
}

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
    assert_matches!(&err, Error::SecretInFile { key, .. } if key == "webhooks.endpoints.partner.secret", "{err}");
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
    assert_matches!(&err, Error::Deserialize(_), "{rendered}");
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
    assert_matches!(&err, Error::UnaddressableKey { key, .. } if key == "webhooks.endpoints.Partner", "{err}");
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
fn webhooks_blank_environment_secret_is_refused() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__URL",
                "https://partner.example/events",
            ),
            ("APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET", ""),
        ]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Validate(validation) if validation.key == "webhooks.endpoints.partner.secret", "{err}");
    assert_eq!(
        err.to_string(),
        "configuration is invalid: webhooks.endpoints.partner.secret: cannot be empty"
    );
}

#[test]
fn webhooks_blank_previous_secret_ends_the_rotation() {
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
            ("APP__WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_SECRET", "  "),
        ]),
    )
    .unwrap();
    assert!(cfg.webhooks.endpoints["partner"].previous_secret.is_none());
}

#[test]
fn webhooks_file_placeholders_document_environment_only_keys() {
    let dir = tempfile::tempdir().unwrap();
    let documented = write(
        &dir,
        "documented.toml",
        "[webhooks.endpoints.partner]\nurl = \"https://partner.example/events\"\nsecret = \"\"\nprevious_secret = \"\"\n",
    );
    let cfg = load_from(
        &LoadOptions {
            config: Some(documented),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[(
            "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
            "fixture-current-secret",
        )]),
    )
    .unwrap();
    assert!(cfg.webhooks.endpoints["partner"].previous_secret.is_none());
}
