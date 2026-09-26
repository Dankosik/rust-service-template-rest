//! Immutable configuration for durable outbound Standard Webhooks delivery.
//!
//! This section owns only the operator snapshot shape. URL admission and
//! decoded-secret validation remain with the provider that consumes the values.

use std::collections::BTreeMap;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::ValidationError;

/// Static outgoing webhook endpoints and their environment-only signing keys.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebhooksConfig {
    /// Stable endpoint IDs bound to destination and current signing keys.
    pub endpoints: BTreeMap<String, WebhookEndpointConfig>,
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
            validate_secret(&key, &endpoint.secret)?;
            if let Some(previous_secret) = &endpoint.previous_secret {
                let key = format!("webhooks.endpoints.{endpoint_id}.previous_secret");
                validate_secret(&key, previous_secret)?;
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

fn validate_secret(key: &str, secret: &SecretString) -> Result<(), ValidationError> {
    if secret.expose_secret().trim().is_empty() {
        return Err(ValidationError::new(key, "cannot be empty"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::{WebhookEndpointConfig, WebhooksConfig};

    #[test]
    fn defaults_are_inert() {
        let config = WebhooksConfig::default();
        assert!(config.endpoints.is_empty());
        config.validate().unwrap();
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
