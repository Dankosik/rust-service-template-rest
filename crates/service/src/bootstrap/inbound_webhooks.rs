use std::sync::Arc;

use super::background::Background;
use infra_postgres::PgPool;
use infra_webhooks::inbound::route::WebhookState;
use infra_webhooks::inbound::{Receiver, Verifier};
use infra_webhooks::protocol::{KeyRing, SigningKey};
use secrecy::ExposeSecret;
use service_config::{Config, InboundWebhookKey};

use super::BootstrapError;

pub(super) fn prepare(
    config: &Config,
    postgres_pool: Option<&PgPool>,
    background: &mut Background,
) -> Result<WebhookState, BootstrapError> {
    let webhooks = &config.inbound_webhooks;
    if webhooks.is_empty() {
        return Ok(WebhookState::inert());
    }
    let pool = postgres_pool.ok_or(BootstrapError::InboundWebhooksWithoutPostgres)?;
    let mut bindings = Vec::with_capacity(webhooks.len());
    for (endpoint_id, endpoint) in webhooks {
        let active = signing_key(endpoint_id, &endpoint.active_key)?;
        let previous = endpoint
            .previous_key
            .as_deref()
            .map(|key| signing_key(endpoint_id, key))
            .transpose()?;
        let verifier: Arc<dyn Verifier> = Arc::new(KeyRing::new(active, previous));
        bindings.push((endpoint_id.clone(), verifier));
    }
    let receiver = Receiver::new(pool.clone(), bindings);
    background.spawn(
        "webhook_cleanup",
        receiver.clone().run_cleanup(background.child_token()),
    );
    Ok(WebhookState::active(receiver))
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
