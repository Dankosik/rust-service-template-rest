//! Immutable configuration for durable outbound Standard Webhooks delivery.
//!
//! This section owns only the operator snapshot shape. URL admission and
//! decoded-secret validation remain with the provider that consumes the values.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::ValidationError;
use crate::validate::{non_blank, validate_endpoint_id};

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
    #[serde(deserialize_with = "crate::de::optional_unsigned_count")]
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
            non_blank(&key, endpoint.secret.expose_secret())?;
            if let Some(previous_secret) = &endpoint.previous_secret {
                let key = format!("webhooks.endpoints.{endpoint_id}.previous_secret");
                non_blank(&key, previous_secret.expose_secret())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
