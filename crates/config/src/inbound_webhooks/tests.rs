use std::assert_matches;

use secrecy::SecretString;

use super::{InboundWebhookEndpointInput, InboundWebhooksInput};
use crate::LoadOptions;
use crate::load::{Error, is_env_addressable, load_from, load_worker_from};
use crate::test_support::{BUILD, env, load_env, write};
use crate::validate::{ValidationErrors, validate_with};

fn validated(config: InboundWebhooksInput, postgres_enabled: bool) -> Result<(), ValidationErrors> {
    validate_with(|v| {
        let _ = config.admit(postgres_enabled, v);
    })
}

fn with_endpoint(active_key: &str, previous_key: Option<&str>) -> InboundWebhooksInput {
    let mut config = InboundWebhooksInput::default();
    config.endpoints.insert(
        "partner".to_owned(),
        InboundWebhookEndpointInput {
            active_key: active_key.to_owned(),
            previous_key: previous_key.map(str::to_owned),
        },
    );
    if is_env_addressable(active_key) {
        config.secrets.insert(
            active_key.to_owned(),
            SecretString::from("fixture-secret".to_owned()),
        );
    }
    if let Some(previous) = previous_key
        && is_env_addressable(previous)
    {
        config.secrets.insert(
            previous.to_owned(),
            SecretString::from("fixture-previous-secret".to_owned()),
        );
    }
    config
}

#[test]
fn defaults_are_inert() {
    assert!(load_env(&[]).inbound_webhooks.is_empty());
}

#[test]
fn a_configured_endpoint_requires_postgres() {
    assert!(
        validated(with_endpoint("partner_v2", None), false)
            .unwrap_err()
            .names("postgres.enabled")
    );
    validated(with_endpoint("partner_v2", None), true).unwrap();
}

#[test]
fn rejects_an_empty_key_reference() {
    let errors = validated(with_endpoint("", None), true).unwrap_err();
    assert!(errors.names("inbound_webhooks.endpoints.partner.active_key"));
}

#[test]
fn rejects_a_reference_no_secret_variable_can_name() {
    for reference in ["Partner_V2", "partner__v2", "partner_"] {
        let errors = validated(with_endpoint(reference, None), true).unwrap_err();
        assert!(errors.names("inbound_webhooks.endpoints.partner.active_key"));
    }
}

#[test]
fn rejects_reused_active_and_previous_references() {
    let errors = validated(with_endpoint("partner_v2", Some("partner_v2")), true).unwrap_err();
    assert!(errors.names("inbound_webhooks.endpoints.partner.previous_key"));
    assert!(errors.to_string().contains("must differ"), "{errors}");
}

#[test]
fn service_requires_non_blank_secrets_for_referenced_inbound_keys() {
    let dir = tempfile::tempdir().unwrap();
    let config = write(
        &dir,
        "config.toml",
        "[inbound_webhooks.endpoints.partner]\nactive_key = \"partner_v2\"\nprevious_key = \"partner_v1\"\n",
    );
    let options = LoadOptions {
        config: Some(config),
        ..LoadOptions::default()
    };
    let err = load_from(
        &options,
        BUILD,
        env(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
            ("APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V1", "   "),
        ]),
    )
    .unwrap_err();
    let rendered = err.to_string();
    assert_matches!(&err, Error::Validate(errors) if errors.names("inbound_webhooks.secrets.partner_v2") && errors.names("inbound_webhooks.secrets.partner_v1"), "{rendered}");
    assert!(
        rendered.contains("; set APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V2"),
        "{rendered}"
    );
    assert!(
        rendered.contains("; set APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V1"),
        "{rendered}"
    );
}

#[test]
fn worker_requires_only_endpoint_ids_and_ignores_service_signing_fields() {
    let dir = tempfile::tempdir().unwrap();
    let config = write(
        &dir,
        "config.toml",
        "[inbound_webhooks.endpoints.partner]\nprevious_key = [42]\n[inbound_webhooks.endpoints.orders]\n",
    );
    let cfg = load_worker_from(
        &LoadOptions {
            config: Some(config),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        ]),
    )
    .unwrap();
    assert_eq!(
        cfg.inbound_webhook_endpoints
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["orders", "partner"]
    );
    let err = load_worker_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__TYPO", "hidden")]),
    )
    .unwrap_err();
    assert_matches!(err, Error::Deserialize(_));
}

#[test]
fn service_binds_active_and_previous_secrets_to_each_endpoint() {
    use secrecy::ExposeSecret as _;

    let cfg = load_env(&[
        (
            "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__ACTIVE_KEY",
            "partner_v2",
        ),
        (
            "APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V2",
            "fixture-secret",
        ),
        (
            "APP__INBOUND_WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_KEY",
            "partner_v1",
        ),
        (
            "APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V1",
            "previous-fixture",
        ),
        ("APP__POSTGRES__ENABLED", "true"),
        ("APP__POSTGRES__DSN", "postgres://localhost/app"),
    ]);
    let endpoint = &cfg.inbound_webhooks["partner"];
    assert_eq!(endpoint.active_key.reference, "partner_v2");
    assert_eq!(endpoint.active_key.secret.expose_secret(), "fixture-secret");
    let previous = endpoint.previous_key.as_ref().unwrap();
    assert_eq!(previous.reference, "partner_v1");
    assert_eq!(previous.secret.expose_secret(), "previous-fixture");
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("fixture-secret"));
    assert!(!debug.contains("previous-fixture"));
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
            (
                "APP__INBOUND_WEBHOOKS__SECRETS__PARTNER_V2",
                "fixture-secret",
            ),
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        ]),
    )
    .unwrap();
    assert!(cfg.inbound_webhooks["partner"].previous_key.is_none());
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
    assert_matches!(&err, Error::Deserialize(_), "{rendered}");
    assert!(rendered.contains("inbound_webhooks.secrets"), "{rendered}");
    assert!(!rendered.contains("fixture-secret-value"), "{rendered}");
}
