use std::collections::BTreeMap;
use std::num::NonZeroU32;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::de::{BlankAsNoneVerbatim, Keep, Mode, ModeMarker, Skip, StrictCount, When};
use crate::validate::{Violations, non_blank, validate_endpoint_id};

#[serde_with::serde_as]
#[derive(Deserialize, educe::Educe)]
#[educe(Default(bound = false))]
#[serde(
    bound(deserialize = "Endpoint: Deserialize<'de>"),
    deny_unknown_fields,
    default
)]
pub(crate) struct WebhooksFields<Endpoint, MaxConcurrent: Mode = Keep> {
    endpoints: BTreeMap<String, Endpoint>,
    #[serde_as(as = "When<MaxConcurrent, Option<StrictCount>>")]
    max_concurrent_deliveries: Option<NonZeroU32>,
    #[serde(skip)]
    _mode: ModeMarker<MaxConcurrent>,
}

pub(crate) type WebhooksInput = WebhooksFields<serde::de::IgnoredAny, Skip>;

pub(crate) type WorkerWebhooksInput = WebhooksFields<WebhookEndpointInput>;

#[serde_with::serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WebhookEndpointInput {
    url: String,
    secret: SecretString,
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    previous_secret: Option<SecretString>,
}

impl WebhooksInput {
    pub(crate) fn admit(self, v: &mut Violations) -> Vec<String> {
        for id in self.endpoints.keys() {
            v.check(validate_endpoint_id("webhooks.endpoints", id));
        }
        self.endpoints.into_keys().collect()
    }
}

impl WorkerWebhooksInput {
    pub(crate) fn admit(self, v: &mut Violations) -> WebhooksConfig {
        let endpoints = self
            .endpoints
            .into_iter()
            .map(|(endpoint_id, endpoint)| {
                v.check(validate_endpoint_id("webhooks.endpoints", &endpoint_id));
                let key = format!("webhooks.endpoints.{endpoint_id}.secret");
                v.check(non_blank(&key, endpoint.secret.expose_secret()));
                (
                    endpoint_id,
                    WebhookEndpointConfig {
                        url: endpoint.url,
                        secret: endpoint.secret,
                        previous_secret: endpoint.previous_secret,
                    },
                )
            })
            .collect();
        WebhooksConfig {
            endpoints,
            max_concurrent_deliveries: self.max_concurrent_deliveries,
        }
    }
}

#[derive(Clone, Debug)]
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

#[derive(Clone, derive_more::Debug)]
pub struct WebhookEndpointConfig {
    #[debug(skip)]
    pub url: String,
    pub secret: SecretString,
    /// Optional environment-only predecessor retained during a key rotation.
    /// Blank is absent.
    pub previous_secret: Option<SecretString>,
}

#[cfg(test)]
mod tests;
