use infra_postgres::Pool;
use infra_webhooks::inbound::Verifiers;
use infra_webhooks::inbound::route::WebhookState;
use infra_webhooks::protocol::{EndpointKeyOwners, KeyRing, SigningKey};
use secrecy::ExposeSecret;
use service_config::{Config, InboundWebhookKey};

use super::BootstrapError;

pub(super) fn prepare(
    config: &Config,
    postgres_pool: Option<&Pool>,
) -> Result<WebhookState, BootstrapError> {
    let webhooks = config.inbound_webhooks();
    if webhooks.is_empty() {
        return Ok(WebhookState::inert());
    }
    let pool = postgres_pool.ok_or(BootstrapError::InboundWebhooksWithoutPostgres)?;
    let mut verifiers = Verifiers::new();
    let mut owners = EndpointKeyOwners::default();
    for (endpoint_id, endpoint) in webhooks {
        let active = signing_key(endpoint_id, &endpoint.active_key)?;
        let previous = endpoint
            .previous_key
            .as_deref()
            .map(|key| signing_key(endpoint_id, key))
            .transpose()?;
        let keys = KeyRing::new(active, previous);
        owners.register(endpoint_id, &keys)?;
        verifiers.bind(endpoint_id.as_str(), keys);
    }
    Ok(WebhookState::active(verifiers.into_receiver(pool.clone())))
}

fn signing_key(endpoint: &str, key: &InboundWebhookKey) -> Result<SigningKey, BootstrapError> {
    SigningKey::from_encoded(key.secret.expose_secret()).map_err(|source| {
        BootstrapError::InboundWebhookKey {
            endpoint: endpoint.to_owned(),
            key: key.reference.clone(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn separate_inbound_endpoints_cannot_share_a_rotation_key() {
        let config_for = |second_key| {
            super::super::test_config(&[
                ("APP__POSTGRES__ENABLED", "true"),
                ("APP__POSTGRES__DSN", "postgres://app@localhost/app"),
                ("APP__INBOUND_WEBHOOKS__ENDPOINTS__A__ACTIVE_KEY", "a_new"),
                ("APP__INBOUND_WEBHOOKS__ENDPOINTS__A__PREVIOUS_KEY", "a_old"),
                ("APP__INBOUND_WEBHOOKS__ENDPOINTS__B__ACTIVE_KEY", "b"),
                (
                    "APP__INBOUND_WEBHOOKS__SECRETS__A_NEW",
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                ),
                (
                    "APP__INBOUND_WEBHOOKS__SECRETS__A_OLD",
                    "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB",
                ),
                ("APP__INBOUND_WEBHOOKS__SECRETS__B", second_key),
            ])
        };
        let pool = Pool::connect_lazy_for_test("postgres://127.0.0.1:1/app").unwrap();
        let independent = config_for("AgICAgICAgICAgICAgICAgICAgICAgIC");
        assert!(prepare(&independent, Some(&pool)).is_ok());
        let overlap = config_for("whsec_AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB");
        let error = prepare(&overlap, Some(&pool)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "webhook endpoints a and b share an HMAC signing key"
        );
    }
}
