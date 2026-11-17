use std::collections::BTreeMap;

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;

use crate::ValidationError;
use crate::load::{ADDRESSABLE_KEY_FORM, is_env_addressable};
use crate::validate::{Violations, missing, validate_endpoint_id};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct InboundWebhooksConfig {
    pub endpoints: BTreeMap<String, InboundWebhookEndpointConfig>,
    #[serde(deserialize_with = "crate::de::secret_map")]
    pub secrets: BTreeMap<String, SecretString>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundWebhookEndpointConfig {
    #[serde(deserialize_with = "crate::de::string")]
    pub active_key: String,
    #[serde(default, deserialize_with = "crate::de::blank_as_none")]
    pub previous_key: Option<String>,
}

impl InboundWebhooksConfig {
    pub(crate) fn validate_service(&self, postgres_enabled: bool, violations: &mut Violations) {
        validate_endpoints(&self.endpoints, postgres_enabled, violations);
        for (endpoint_id, endpoint) in &self.endpoints {
            self.check_secret_present(endpoint_id, "active_key", &endpoint.active_key, violations);
            if let Some(previous_key) = &endpoint.previous_key
                && previous_key != &endpoint.active_key
            {
                self.check_secret_present(endpoint_id, "previous_key", previous_key, violations);
            }
        }
        for reference in self.secrets.keys() {
            violations.check(validate_key_ref(
                &format!("inbound_webhooks.secrets.{reference}"),
                reference,
            ));
        }
    }

    fn check_secret_present(
        &self,
        endpoint_id: &str,
        field: &str,
        key: &str,
        violations: &mut Violations,
    ) {
        if !is_env_addressable(key) {
            return;
        }
        if self
            .secrets
            .get(key)
            .is_none_or(|secret| secret.expose_secret().trim().is_empty())
        {
            violations.check(Err(missing(
                &format!("inbound_webhooks.secrets.{key}"),
                &format!("inbound_webhooks.endpoints.{endpoint_id}.{field} references `{key}`"),
            )));
        }
    }
}

// template:begin worker:worker-inbound-webhooks
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct WorkerInboundWebhooksInput {
    endpoints: BTreeMap<String, InboundWebhookEndpointConfig>,
    secrets: serde::de::IgnoredAny,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(from = "WorkerInboundWebhooksInput")]
pub struct WorkerInboundWebhooksConfig {
    pub endpoints: BTreeMap<String, InboundWebhookEndpointConfig>,
}

impl From<WorkerInboundWebhooksInput> for WorkerInboundWebhooksConfig {
    fn from(input: WorkerInboundWebhooksInput) -> Self {
        Self {
            endpoints: input.endpoints,
        }
    }
}

impl WorkerInboundWebhooksConfig {
    pub(crate) fn validate(&self, postgres_enabled: bool, violations: &mut Violations) {
        validate_endpoints(&self.endpoints, postgres_enabled, violations);
    }
}
// template:end worker:worker-inbound-webhooks

fn validate_endpoints(
    endpoints: &BTreeMap<String, InboundWebhookEndpointConfig>,
    postgres_enabled: bool,
    violations: &mut Violations,
) {
    if !endpoints.is_empty() && !postgres_enabled {
        violations.check(Err(ValidationError::new(
            "postgres.enabled",
            "must be true when inbound webhook endpoints are configured",
        )));
    }
    for (endpoint_id, endpoint) in endpoints {
        violations.check(validate_endpoint_id(
            "inbound_webhooks.endpoints",
            endpoint_id,
        ));
        let config_key = format!("inbound_webhooks.endpoints.{endpoint_id}.active_key");
        violations.check(validate_key_ref(&config_key, &endpoint.active_key));
        if let Some(previous_key) = &endpoint.previous_key {
            let config_key = format!("inbound_webhooks.endpoints.{endpoint_id}.previous_key");
            violations.check(validate_key_ref(&config_key, previous_key));
            if previous_key == &endpoint.active_key {
                violations.check(Err(ValidationError::new(
                    config_key,
                    "must differ from active_key when present",
                )));
            }
        }
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
