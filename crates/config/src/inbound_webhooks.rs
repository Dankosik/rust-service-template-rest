use std::collections::BTreeMap;
use std::sync::Arc;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::ValidationError;
use crate::load::{ADDRESSABLE_KEY_FORM, is_env_addressable};
use crate::validate::{Violations, missing, validate_endpoint_id};

#[derive(Deserialize, educe::Educe)]
#[educe(Default(bound(Secrets: Default)))]
#[serde(deny_unknown_fields, default)]
pub(crate) struct InboundWebhooksFields<Endpoint, Secrets> {
    endpoints: BTreeMap<String, Endpoint>,
    secrets: Secrets,
}

#[serde_with::serde_as]
#[derive(Default, Deserialize)]
#[serde(transparent)]
pub(crate) struct SecretMap(
    #[serde_as(as = "BTreeMap<_, crate::de::StrictSecret>")] BTreeMap<String, SecretString>,
);

pub(crate) type InboundWebhooksInput =
    InboundWebhooksFields<InboundWebhookEndpointInput, SecretMap>;

#[serde_with::serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InboundWebhookEndpointInput {
    #[serde_as(as = "crate::de::StrictString")]
    active_key: String,
    #[serde(default)]
    #[serde_as(as = "crate::de::BlankAsNone")]
    previous_key: Option<String>,
}

#[derive(Clone, Debug)]
pub struct InboundWebhookEndpointConfig {
    pub active_key: Arc<InboundWebhookKey>,
    pub previous_key: Option<Arc<InboundWebhookKey>>,
}

#[derive(Debug)]
pub struct InboundWebhookKey {
    pub reference: String,
    pub secret: SecretString,
}

impl InboundWebhooksInput {
    pub(crate) fn admit(
        self,
        postgres_enabled: bool,
        violations: &mut Violations,
    ) -> BTreeMap<String, InboundWebhookEndpointConfig> {
        validate_endpoints(&self.endpoints, postgres_enabled, violations);
        let mut keys = BTreeMap::new();
        for (reference, secret) in self.secrets.0 {
            violations.check(validate_key_ref(
                &format!("inbound_webhooks.secrets.{reference}"),
                &reference,
            ));
            if !secret.expose_secret().trim().is_empty() {
                keys.insert(
                    reference.clone(),
                    Arc::new(InboundWebhookKey { reference, secret }),
                );
            }
        }
        let mut bindings = BTreeMap::new();
        for (endpoint_id, endpoint) in self.endpoints {
            let active = resolve_key(&keys, &endpoint_id, "active_key", &endpoint.active_key);
            let previous = match endpoint.previous_key {
                Some(reference) if reference == endpoint.active_key => Err(ValidationError::new(
                    format!("inbound_webhooks.endpoints.{endpoint_id}.previous_key"),
                    "must differ from active_key when present",
                )),
                Some(reference) => {
                    resolve_key(&keys, &endpoint_id, "previous_key", &reference).map(Some)
                }
                None => Ok(None),
            };
            if let (Ok(active_key), Ok(previous_key)) =
                (violations.record(active), violations.record(previous))
            {
                bindings.insert(
                    endpoint_id,
                    InboundWebhookEndpointConfig {
                        active_key,
                        previous_key,
                    },
                );
            }
        }
        bindings
    }
}

fn resolve_key(
    keys: &BTreeMap<String, Arc<InboundWebhookKey>>,
    endpoint_id: &str,
    field: &str,
    reference: &str,
) -> Result<Arc<InboundWebhookKey>, ValidationError> {
    let config_key = format!("inbound_webhooks.endpoints.{endpoint_id}.{field}");
    validate_key_ref(&config_key, reference)?;
    keys.get(reference).cloned().ok_or_else(|| {
        missing(
            &format!("inbound_webhooks.secrets.{reference}"),
            &format!("{config_key} references `{reference}`"),
        )
    })
}

// template:begin worker:worker-inbound-webhooks
pub(crate) type WorkerInboundWebhooksInput =
    InboundWebhooksFields<WorkerEndpointInput, serde::de::IgnoredAny>;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct WorkerEndpointInput {
    #[serde(rename = "active_key")]
    _active_key: serde::de::IgnoredAny,
    #[serde(rename = "previous_key")]
    _previous_key: serde::de::IgnoredAny,
}

impl WorkerInboundWebhooksInput {
    pub(crate) fn admit(
        self,
        postgres_enabled: bool,
        violations: &mut Violations,
    ) -> std::collections::BTreeSet<String> {
        validate_endpoints(&self.endpoints, postgres_enabled, violations);
        self.endpoints.into_keys().collect()
    }
}
// template:end worker:worker-inbound-webhooks

fn validate_endpoints<T>(
    endpoints: &BTreeMap<String, T>,
    postgres_enabled: bool,
    violations: &mut Violations,
) {
    if !endpoints.is_empty() && !postgres_enabled {
        violations.reject(ValidationError::new(
            "postgres.enabled",
            "must be true when inbound webhook endpoints are configured",
        ));
    }
    for endpoint_id in endpoints.keys() {
        violations.check(validate_endpoint_id(
            "inbound_webhooks.endpoints",
            endpoint_id,
        ));
    }
}

fn validate_key_ref(config_key: &str, reference: &str) -> Result<(), ValidationError> {
    if !is_env_addressable(reference) {
        return Err(ValidationError::new(
            config_key,
            const_format::formatcp!("key references must be {ADDRESSABLE_KEY_FORM}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
