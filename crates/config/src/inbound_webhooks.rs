//! Immutable configuration for Standard Webhooks receipt admission.
//!
//! Verification-key decoding and consumer binding remain provider and root
//! responsibilities. This typed snapshot remains independently removable from
//! outbound webhook delivery.

use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::Deserialize;

use crate::ValidationError;
use crate::load::{ADDRESSABLE_KEY_FORM, is_env_addressable};
use crate::validate::validate_endpoint_id;

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
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
    pub previous_key: Option<String>,
}

impl InboundWebhooksConfig {
    /// Receipts are durable, so a configured endpoint needs PostgreSQL.
    /// Secret presence is checked by the receiving root: the worker shares
    /// this section but never holds verification secrets.
    pub(crate) fn validate(&self, postgres_enabled: bool) -> Result<(), ValidationError> {
        if !self.endpoints.is_empty() && !postgres_enabled {
            return Err(ValidationError::new(
                "postgres.enabled",
                "must be true when inbound webhook endpoints are configured",
            ));
        }
        for (endpoint_id, endpoint) in &self.endpoints {
            validate_endpoint_id("inbound_webhooks.endpoints", endpoint_id)?;
            let config_key = format!("inbound_webhooks.endpoints.{endpoint_id}.active_key");
            validate_key_ref(&config_key, &endpoint.active_key)?;
            if let Some(previous_key) = &endpoint.previous_key {
                let config_key = format!("inbound_webhooks.endpoints.{endpoint_id}.previous_key");
                validate_key_ref(&config_key, previous_key)?;
                if previous_key == &endpoint.active_key {
                    return Err(ValidationError::new(
                        config_key,
                        "must differ from active_key when present",
                    ));
                }
            }
        }
        for reference in self.secrets.keys() {
            validate_key_ref(&format!("inbound_webhooks.secrets.{reference}"), reference)?;
        }
        Ok(())
    }
}

/// A reference names an `APP__INBOUND_WEBHOOKS__SECRETS__<REF>` variable,
/// which the loader lowercases; any other spelling could never resolve.
fn validate_key_ref(config_key: &str, reference: &str) -> Result<(), ValidationError> {
    if !is_env_addressable(reference) {
        return Err(ValidationError::new(
            config_key,
            format!("key references must be {ADDRESSABLE_KEY_FORM}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
