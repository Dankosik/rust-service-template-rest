use std::collections::BTreeMap;
use std::num::NonZeroU32;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::validate::{Violations, non_blank, validate_endpoint_id};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WebhooksConfig {
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

#[derive(Clone, derive_more::Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEndpointConfig {
    #[debug(skip)]
    #[serde(deserialize_with = "crate::de::string")]
    pub url: String,
    #[serde(deserialize_with = "crate::de::secret")]
    pub secret: SecretString,
    /// Optional environment-only predecessor retained during a key rotation.
    /// Blank is absent.
    #[serde(default, deserialize_with = "crate::de::blank_secret_as_none")]
    pub previous_secret: Option<SecretString>,
}

impl WebhooksConfig {
    pub(crate) fn validate(&self, violations: &mut Violations) {
        for (endpoint_id, endpoint) in &self.endpoints {
            violations.check(validate_endpoint_id("webhooks.endpoints", endpoint_id));
            let key = format!("webhooks.endpoints.{endpoint_id}.secret");
            violations.check(non_blank(&key, endpoint.secret.expose_secret()));
        }
    }
}

#[cfg(test)]
mod tests;
