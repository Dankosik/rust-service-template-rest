use super::{InboundWebhookEndpointConfig, InboundWebhooksConfig};
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};

fn with_endpoint(active_key: &str, previous_key: Option<&str>) -> InboundWebhooksConfig {
    let mut config = InboundWebhooksConfig::default();
    config.endpoints.insert(
        "partner".to_owned(),
        InboundWebhookEndpointConfig {
            active_key: active_key.to_owned(),
            previous_key: previous_key.map(str::to_owned),
        },
    );
    config
}

#[test]
fn defaults_are_inert() {
    let config = InboundWebhooksConfig::default();
    assert!(config.endpoints.is_empty());
    assert!(config.secrets.is_empty());
    config.validate(false).unwrap();
}

#[test]
fn a_configured_endpoint_requires_postgres() {
    let config = with_endpoint("partner_v2", None);
    assert_eq!(config.validate(false).unwrap_err().key, "postgres.enabled");
    config.validate(true).unwrap();
}

#[test]
fn rejects_an_empty_key_reference() {
    let err = with_endpoint("", None).validate(true).unwrap_err();
    assert_eq!(err.key, "inbound_webhooks.endpoints.partner.active_key");
}

#[test]
fn rejects_a_reference_no_secret_variable_can_name() {
    for reference in ["Partner_V2", "partner__v2", "partner_"] {
        let err = with_endpoint(reference, None).validate(true).unwrap_err();
        assert_eq!(err.key, "inbound_webhooks.endpoints.partner.active_key");
    }
}

#[test]
fn rejects_reused_active_and_previous_references() {
    let err = with_endpoint("partner_v2", Some("partner_v2"))
        .validate(true)
        .unwrap_err();
    assert_eq!(err.key, "inbound_webhooks.endpoints.partner.previous_key");
    assert!(err.message.contains("must differ"), "{err}");
}

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
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
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

#[test]
fn an_empty_inbound_previous_key_variable_unsets_the_file_value() {
    let dir = tempfile::tempdir().unwrap();
    let config = write(
        &dir,
        "config.toml",
        "[inbound_webhooks.endpoints.partner]\nactive_key = \"partner_v2\"\nprevious_key = \"partner_v1\"\n",
    );
    let cfg = load_from(
        &LoadOptions {
            config: Some(config),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            (
                "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_KEY",
                "",
            ),
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        ]),
    )
    .unwrap();
    assert_eq!(
        cfg.inbound_webhooks
            .endpoints
            .get("partner")
            .unwrap()
            .previous_key,
        None
    );
}

#[test]
fn inbound_webhooks_secrets_without_a_reference_are_not_echoed() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__INBOUND_WEBHOOKS__SECRETS", "fixture-secret-value")]),
    )
    .unwrap_err();
    let rendered = err.to_string();
    assert!(matches!(&err, Error::Deserialize(_)), "{rendered}");
    assert!(rendered.contains("inbound_webhooks.secrets"), "{rendered}");
    assert!(!rendered.contains("fixture-secret-value"), "{rendered}");
}
