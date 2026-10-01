//! Immutable configuration for Standard Webhooks receipt admission.
//!
//! Verification-key decoding and consumer binding remain provider and root
//! responsibilities. This typed snapshot remains independently removable from
//! outbound webhook delivery.

use std::collections::BTreeMap;

use secrecy::SecretString;
use serde::Deserialize;

use crate::ValidationError;
use crate::validate::is_env_addressable;

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

/// A reference names an `APP__INBOUND_WEBHOOKS__SECRETS__<REF>` variable,
/// which the loader lowercases; any other spelling could never resolve.
fn validate_key_ref(key: &str, reference: &str) -> Result<(), ValidationError> {
    if !is_env_addressable(reference) {
        return Err(ValidationError::new(
            key,
            "key references must be lowercase letters, digits, `_`, or `-`, without `__` or a trailing `_`",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{InboundWebhookEndpointConfig, InboundWebhooksConfig};

    fn with_endpoint(active_key: &str, previous_key: Option<&str>) -> InboundWebhooksConfig {
        let mut config = InboundWebhooksConfig::default();
        config.endpoints.insert(
            "partner".to_owned(),
            InboundWebhookEndpointConfig {
                active_key: active_key.to_owned(),
                previous_key: previous_key.map(str::to_owned),
            },
        );
        config
    }

    #[test]
    fn defaults_are_inert() {
        let config = InboundWebhooksConfig::default();
        assert!(config.endpoints.is_empty());
        assert!(config.secrets.is_empty());
        config.validate(false).unwrap();
    }

    #[test]
    fn a_configured_endpoint_requires_postgres() {
        let config = with_endpoint("partner_v2", None);
        assert_eq!(config.validate(false).unwrap_err().key, "postgres.enabled");
        config.validate(true).unwrap();
    }

    #[test]
    fn rejects_an_empty_key_reference() {
        let err = with_endpoint("", None).validate(true).unwrap_err();
        assert_eq!(err.key, "inbound_webhooks.endpoints.partner.active_key");
    }

    #[test]
    fn rejects_a_reference_no_secret_variable_can_name() {
        for reference in ["Partner_V2", "partner__v2", "partner_"] {
            let err = with_endpoint(reference, None).validate(true).unwrap_err();
            assert_eq!(err.key, "inbound_webhooks.endpoints.partner.active_key");
        }
    }

    #[test]
    fn rejects_reused_active_and_previous_references() {
        let err = with_endpoint("partner_v2", Some("partner_v2"))
            .validate(true)
            .unwrap_err();
        assert_eq!(err.key, "inbound_webhooks.endpoints.partner.previous_key");
        assert!(err.message.contains("must differ"), "{err}");
    }
}
