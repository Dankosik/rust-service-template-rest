use std::assert_matches;
use std::num::NonZeroU32;

use secrecy::SecretString;

use super::{WebhookEndpointInput, WorkerWebhooksInput};
use crate::LoadOptions;
use crate::load::{ErrorKind, InputError, load_from, load_worker_from};
use crate::test_support::{BUILD, assert_secret_in_file, env, load_worker_env, write};
use crate::validate::admit_with;

#[test]
fn defaults_are_inert() {
    let config = admit_with(|v| Ok(WorkerWebhooksInput::default().admit(v))).unwrap();
    assert!(config.endpoints.is_empty());
    assert_eq!(config.max_concurrent_deliveries, None);
}

#[test]
fn a_zero_delivery_bound_fails_to_deserialize() {
    let err = toml::from_str::<WorkerWebhooksInput>("max_concurrent_deliveries = 0")
        .err()
        .expect("zero max_concurrent_deliveries must fail to deserialize");
    assert!(
        err.to_string().contains("max_concurrent_deliveries"),
        "{err}"
    );
    let input = toml::from_str::<WorkerWebhooksInput>("max_concurrent_deliveries = 4").unwrap();
    let config = admit_with(|v| Ok(input.admit(v))).unwrap();
    assert_eq!(
        config.max_concurrent_deliveries.map(NonZeroU32::get),
        Some(4)
    );
}

#[test]
fn rejects_an_empty_endpoint_id() {
    let mut input = WorkerWebhooksInput::default();
    input.endpoints.insert(
        String::new(),
        WebhookEndpointInput {
            url: "https://partner.example/events".to_owned(),
            secret: SecretString::from("whsec_current".to_owned()),
            previous_secret: None,
        },
    );

    let mut invalid = WorkerWebhooksInput::default();
    invalid.endpoints.insert(
        String::new(),
        WebhookEndpointInput {
            url: "https://partner.example/events".to_owned(),
            secret: SecretString::from("whsec_current".to_owned()),
            previous_secret: None,
        },
    );
    let errors = admit_with(|v| Ok(invalid.admit(v))).unwrap_err();
    assert!(errors.names("webhooks.endpoints"));

    let endpoint = input.endpoints.remove("").unwrap();
    input.endpoints.insert("partner/a?#".to_owned(), endpoint);
    admit_with(|v| Ok(input.admit(v))).unwrap();
}

#[test]
fn rejects_empty_signing_secrets() {
    let mut input = WorkerWebhooksInput::default();
    input.endpoints.insert(
        "partner".to_owned(),
        WebhookEndpointInput {
            url: "https://partner.example/events".to_owned(),
            secret: SecretString::from(String::new()),
            previous_secret: None,
        },
    );

    let errors = admit_with(|v| Ok(input.admit(v))).unwrap_err();
    assert!(errors.names("webhooks.endpoints.partner.secret"));
}

#[test]
fn webhooks_environment_builds_endpoint_local_signing_keys() {
    use secrecy::ExposeSecret as _;

    let destination = "https://partner.example/fixture-private-path?token=fixture-private-query";
    let cfg = load_worker_env(&[
        ("APP__WEBHOOKS__ENDPOINTS__PARTNER__URL", destination),
        (
            "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
            "fixture-current-secret",
        ),
        (
            "APP__WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_SECRET",
            "fixture-previous-secret",
        ),
        ("APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES", "4"),
    ]);
    assert_eq!(
        cfg.webhooks()
            .max_concurrent_deliveries
            .map(std::num::NonZeroU32::get),
        Some(4)
    );
    let endpoint = cfg.webhooks().endpoints.get("partner").unwrap();
    assert_eq!(endpoint.url, destination);
    assert_eq!(endpoint.secret.expose_secret(), "fixture-current-secret");
    assert_eq!(
        endpoint.previous_secret.as_ref().unwrap().expose_secret(),
        "fixture-previous-secret"
    );
    let debug = format!("{cfg:?}");
    for sensitive in [
        "fixture-private-path",
        "fixture-private-query",
        "fixture-current-secret",
        "fixture-previous-secret",
    ] {
        assert!(!debug.contains(sensitive));
    }
}

#[test]
fn webhooks_secret_in_a_file_is_refused() {
    assert_secret_in_file(
        "[webhooks.endpoints.partner]\nsecret = \"whsec_private\"\n",
        "webhooks.endpoints.partner.secret",
    );
}

#[test]
fn webhooks_secret_without_its_key_segment_is_not_echoed() {
    let err = load_worker_from(
        &LoadOptions::default(),
        BUILD,
        env(&[(
            "APP__WEBHOOKS__ENDPOINTS__PARTNER",
            "fixture-current-secret",
        )]),
    )
    .unwrap_err();
    let rendered = err.to_string();
    assert_matches!(&err.0, ErrorKind::Deserialize(_), "{rendered}");
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
    let err = load_worker_from(
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
    assert_matches!(
        &err.0,
        ErrorKind::Input(InputError::UnaddressableKey { key, .. })
            if key == "webhooks.endpoints.Partner",
        "{err}"
    );
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
    let cfg = load_worker_from(
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
    assert_eq!(cfg.webhooks().endpoints.len(), 1);
    let endpoint = cfg.webhooks().endpoints.get("partner-v2").unwrap();
    assert_eq!(endpoint.secret.expose_secret(), "fixture-current-secret");
}

#[test]
fn webhooks_blank_environment_secret_is_refused() {
    let err = load_worker_from(
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
    assert!(
        err.validation_errors()
            .unwrap()
            .names("webhooks.endpoints.partner.secret"),
        "{err}"
    );
    assert_eq!(
        err.to_string(),
        "configuration is invalid: webhooks.endpoints.partner.secret: cannot be empty"
    );
}

#[test]
fn webhooks_blank_previous_secret_ends_the_rotation() {
    let cfg = load_worker_env(&[
        (
            "APP__WEBHOOKS__ENDPOINTS__PARTNER__URL",
            "https://partner.example/events",
        ),
        (
            "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
            "fixture-current-secret",
        ),
        ("APP__WEBHOOKS__ENDPOINTS__PARTNER__PREVIOUS_SECRET", "  "),
    ]);
    assert!(
        cfg.webhooks().endpoints["partner"]
            .previous_secret
            .is_none()
    );
}

#[test]
fn webhooks_file_placeholders_document_environment_only_keys() {
    let dir = tempfile::tempdir().unwrap();
    let documented = write(
        &dir,
        "documented.toml",
        "[webhooks.endpoints.partner]\nurl = \"https://partner.example/events\"\nsecret = \"\"\nprevious_secret = \"\"\n",
    );
    let cfg = load_worker_from(
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
    assert!(
        cfg.webhooks().endpoints["partner"]
            .previous_secret
            .is_none()
    );
}

#[test]
fn service_uses_the_workers_endpoint_ids_without_delivery_urls_or_signing_keys() {
    let dir = tempfile::tempdir().unwrap();
    let shared = write(
        &dir,
        "shared.toml",
        "[webhooks]\nmax_concurrent_deliveries = 4\n[webhooks.endpoints.partner]\nurl = \"https://partner.example/events\"\n[webhooks.endpoints.orders]\n",
    );
    let options = LoadOptions {
        config: Some(shared),
        ..LoadOptions::default()
    };
    let service = load_from(&options, BUILD, env(&[])).unwrap();
    assert_eq!(service.outbound_webhook_endpoints(), ["orders", "partner"]);
    assert!(!format!("{service:?}").contains("https://partner.example/events"));

    let worker = load_worker_from(
        &options,
        BUILD,
        env(&[
            (
                "APP__WEBHOOKS__ENDPOINTS__PARTNER__SECRET",
                "partner-secret",
            ),
            ("APP__WEBHOOKS__ENDPOINTS__ORDERS__SECRET", "orders-secret"),
            (
                "APP__WEBHOOKS__ENDPOINTS__ORDERS__URL",
                "https://orders.example/events",
            ),
        ]),
    )
    .unwrap();
    assert!(
        service
            .outbound_webhook_endpoints()
            .iter()
            .eq(worker.webhooks().endpoints.keys())
    );
}

#[test]
fn service_rejects_a_misspelled_webhook_section_key() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[(
            "APP__WEBHOOKS__ENDPOINT__PARTNER__URL",
            "https://partner.example/fixture-private-path",
        )]),
    )
    .unwrap_err();
    assert_matches!(&err.0, ErrorKind::Deserialize(_), "{err}");
    let message = err.to_string();
    assert!(message.contains("unknown field `endpoint`"), "{err}");
    assert!(!message.contains("fixture-private-path"), "{err}");
}
