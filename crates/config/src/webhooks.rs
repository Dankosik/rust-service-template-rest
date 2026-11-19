use std::collections::BTreeMap;
use std::num::NonZeroU32;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::validate::{Violations, non_blank, validate_endpoint_id};

#[derive(Deserialize, educe::Educe)]
#[educe(Default(bound(MaxConcurrent: Default)))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct WebhooksFields<Endpoint, MaxConcurrent> {
    endpoints: BTreeMap<String, Endpoint>,
    max_concurrent_deliveries: MaxConcurrent,
}

#[serde_with::serde_as]
#[derive(Deserialize)]
#[serde(transparent)]
struct MaxConcurrentDeliveries(#[serde_as(as = "crate::de::StrictCount")] NonZeroU32);

pub(crate) type ServiceWebhooksInput = WebhooksFields<serde::de::IgnoredAny, serde::de::IgnoredAny>;

type WebhooksInput = WebhooksFields<WebhookEndpointConfig, Option<MaxConcurrentDeliveries>>;

impl ServiceWebhooksInput {
    pub(crate) fn admit(self, violations: &mut Violations) -> Vec<String> {
        for id in self.endpoints.keys() {
            violations.check(validate_endpoint_id("webhooks.endpoints", id));
        }
        self.endpoints.into_keys().collect()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "WebhooksInput")]
pub struct WebhooksConfig {
    pub endpoints: BTreeMap<String, WebhookEndpointConfig>,
    /// The most deliveries one worker process runs at once
    /// (`APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES`). Unset by default:
    /// deliveries may then take every `jobs.max_workers` slot, and a receiver
    /// that answers slowly delays the worker's other job kinds for up to the
    /// 30-second attempt timeout per delivery. A value below
    /// `jobs.max_workers` keeps the difference for those kinds.
    pub max_concurrent_deliveries: Option<NonZeroU32>,
}

impl From<WebhooksInput> for WebhooksConfig {
    fn from(input: WebhooksInput) -> Self {
        Self {
            endpoints: input.endpoints,
            max_concurrent_deliveries: input.max_concurrent_deliveries.map(|value| value.0),
        }
    }
}

#[serde_with::serde_as]
#[derive(Clone, derive_more::Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEndpointConfig {
    #[debug(skip)]
    #[serde_as(as = "crate::de::StrictString")]
    pub url: String,
    #[serde_as(as = "crate::de::StrictSecret")]
    pub secret: SecretString,
    /// Optional environment-only predecessor retained during a key rotation.
    /// Blank is absent.
    #[serde(default)]
    #[serde_as(as = "crate::de::BlankSecretAsNone")]
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
