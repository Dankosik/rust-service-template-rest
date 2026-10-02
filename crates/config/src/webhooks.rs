//! Immutable configuration for durable outbound Standard Webhooks delivery.
//!
//! This section owns only the operator snapshot shape. URL admission and
//! decoded-secret validation remain with the provider that consumes the values.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::ValidationError;
use crate::validate::non_empty;

/// Static outgoing webhook endpoints and their environment-only signing keys.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebhooksConfig {
    /// Stable endpoint IDs bound to destination and current signing keys.
    pub endpoints: BTreeMap<String, WebhookEndpointConfig>,
    /// The most deliveries one worker process runs at once
    /// (`APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES`). Unset by default:
    /// deliveries may then take every `jobs.max_workers` slot, and a receiver
    /// that answers slowly delays the worker's other job kinds for up to the
    /// 30-second attempt timeout per delivery. A value below
    /// `jobs.max_workers` keeps the difference for those kinds.
    pub max_concurrent_deliveries: Option<NonZeroU32>,
}

/// One configured outbound destination and its current signing keys.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEndpointConfig {
    /// The static HTTPS destination. Provider construction admits its URL form.
    pub url: String,
    /// Environment-only key used for every new delivery.
    pub secret: SecretString,
    /// Optional environment-only predecessor retained during a key rotation.
    pub previous_secret: Option<SecretString>,
}

impl WebhooksConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        for (endpoint_id, endpoint) in &self.endpoints {
            validate_endpoint_id("webhooks.endpoints", endpoint_id)?;
            let key = format!("webhooks.endpoints.{endpoint_id}.secret");
            non_empty(&key, endpoint.secret.expose_secret())?;
            if let Some(previous_secret) = &endpoint.previous_secret {
                let key = format!("webhooks.endpoints.{endpoint_id}.previous_secret");
                non_empty(&key, previous_secret.expose_secret())?;
            }
        }
        Ok(())
    }
}

fn validate_endpoint_id(section: &str, endpoint_id: &str) -> Result<(), ValidationError> {
    if endpoint_id.is_empty() || endpoint_id.contains('\0') {
        return Err(ValidationError::new(
            section,
            "endpoint IDs must be nonempty and NUL-free",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use secrecy::SecretString;

    use super::{WebhookEndpointConfig, WebhooksConfig};

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

        let endpoint = config.endpoints.get_mut("partner").unwrap();
        endpoint.secret = SecretString::from("whsec_current".to_owned());
        endpoint.previous_secret = Some(SecretString::from("  ".to_owned()));
        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "webhooks.endpoints.partner.previous_secret");
    }
}
