use std::collections::BTreeMap;
use std::sync::Arc;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::ValidationError;
use crate::load::{ADDRESSABLE_KEY_FORM, is_env_addressable};
use crate::validate::{Violations, missing, validate_endpoint_id};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct InboundWebhooksInput {
    endpoints: BTreeMap<String, InboundWebhookEndpointInput>,
    #[serde(deserialize_with = "crate::de::secret_map")]
    secrets: BTreeMap<String, SecretString>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboundWebhookEndpointInput {
    #[serde(deserialize_with = "crate::de::string")]
    active_key: String,
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
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
        let keys: BTreeMap<_, _> = self
            .secrets
            .into_iter()
            .map(|(reference, secret)| {
                violations.check(validate_key_ref(
                    &format!("inbound_webhooks.secrets.{reference}"),
                    &reference,
                ));
                (
                    reference.clone(),
                    Arc::new(InboundWebhookKey { reference, secret }),
                )
            })
            .collect();
        let mut bindings = BTreeMap::new();
        for (id, endpoint) in self.endpoints {
            let active = resolve_key(&keys, &id, "active_key", &endpoint.active_key);
            let previous = match endpoint.previous_key {
                Some(reference) if reference == endpoint.active_key => Err(ValidationError::new(
                    format!("inbound_webhooks.endpoints.{id}.previous_key"),
                    "must differ from active_key when present",
                )),
                Some(reference) => resolve_key(&keys, &id, "previous_key", &reference).map(Some),
                None => Ok(None),
            };
            if let (Ok(active_key), Ok(previous_key)) =
                (violations.take(active), violations.take(previous))
            {
                bindings.insert(
                    id,
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
    endpoint: &str,
    field: &str,
    reference: &str,
) -> Result<Arc<InboundWebhookKey>, ValidationError> {
    let config_key = format!("inbound_webhooks.endpoints.{endpoint}.{field}");
    validate_key_ref(&config_key, reference)?;
    keys.get(reference)
        .filter(|key| !key.secret.expose_secret().trim().is_empty())
        .cloned()
        .ok_or_else(|| {
            missing(
                &format!("inbound_webhooks.secrets.{reference}"),
                &format!("{config_key} references `{reference}`"),
            )
        })
}

// template:begin worker:worker-inbound-webhooks
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct WorkerInboundWebhooksInput {
    #[expect(
        clippy::zero_sized_map_values,
        reason = "Serde validates endpoint table fields before the worker retains their names"
    )]
    endpoints: BTreeMap<String, WorkerEndpointInput>,
    #[serde(rename = "secrets")]
    _secrets: serde::de::IgnoredAny,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct WorkerEndpointInput {
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
            format!("key references must be {ADDRESSABLE_KEY_FORM}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
