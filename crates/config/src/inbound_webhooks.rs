//! Immutable configuration for Standard Webhooks receipt admission.
//!
//! Verification-key decoding and consumer binding remain provider and root
//! responsibilities. This typed snapshot remains independently removable from
//! outbound webhook delivery.

use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::Deserialize;

use crate::ValidationError;

/// Static inbound endpoints and their environment-only verification secrets.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InboundWebhooksConfig {
    /// Stable endpoint IDs bound to immutable verification-key references.
    pub endpoints: BTreeMap<String, InboundWebhookEndpointConfig>,
    /// Environment-only Standard Webhooks secrets, keyed by immutable reference.
    pub secrets: BTreeMap<String, SecretString>,
}

/// Verification-key references for one configured inbound endpoint.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundWebhookEndpointConfig {
    /// Immutable reference to the current verification key.
    pub active_key: String,
    /// Optional immutable predecessor reference retained during rotation.
    pub previous_key: Option<String>,
}

impl InboundWebhooksConfig {
    pub(crate) fn validate(&self) -> Result<(), ValidationError> {
        for (endpoint_id, endpoint) in &self.endpoints {
            validate_endpoint_id("inbound_webhooks.endpoints", endpoint_id)?;
            let key = format!("inbound_webhooks.endpoints.{endpoint_id}.active_key");
            validate_key_ref(&key, &endpoint.active_key)?;
            if let Some(previous_key) = &endpoint.previous_key {
                let key = format!("inbound_webhooks.endpoints.{endpoint_id}.previous_key");
                validate_key_ref(&key, previous_key)?;
                if previous_key == &endpoint.active_key {
                    return Err(ValidationError::new(
                        &key,
                        "must differ from active_key when present",
                    ));
                }
            }
        }
        for reference in self.secrets.keys() {
            validate_key_ref("inbound_webhooks.secrets", reference)?;
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

fn validate_key_ref(key: &str, reference: &str) -> Result<(), ValidationError> {
    if reference.is_empty() || reference.contains('\0') {
        return Err(ValidationError::new(
            key,
            "key references must be nonempty and NUL-free",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{InboundWebhookEndpointConfig, InboundWebhooksConfig};

    #[test]
    fn defaults_are_inert() {
        let config = InboundWebhooksConfig::default();
        assert!(config.endpoints.is_empty());
        assert!(config.secrets.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn rejects_an_empty_key_reference() {
        let mut config = InboundWebhooksConfig::default();
        config.endpoints.insert(
            "partner/a?#".to_owned(),
            InboundWebhookEndpointConfig {
                active_key: String::new(),
                previous_key: None,
            },
        );

        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "inbound_webhooks.endpoints.partner/a?#.active_key");
        config.endpoints.get_mut("partner/a?#").unwrap().active_key = "partner_v2".to_owned();
        config.validate().unwrap();
    }

    #[test]
    fn rejects_reused_active_and_previous_references() {
        let mut config = InboundWebhooksConfig::default();
        config.endpoints.insert(
            "partner".to_owned(),
            InboundWebhookEndpointConfig {
                active_key: "partner_v2".to_owned(),
                previous_key: Some("partner_v2".to_owned()),
            },
        );

        let err = config.validate().unwrap_err();
        assert_eq!(err.key, "inbound_webhooks.endpoints.partner.previous_key");
    }
}
