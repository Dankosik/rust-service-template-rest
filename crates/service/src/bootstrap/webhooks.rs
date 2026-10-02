//! Inbound webhook receiver composition.

use infra_http::webhooks::WebhookState;
use infra_postgres::PgPool;
use infra_webhooks::inbound::Receiver;
use infra_webhooks::protocol::{KeyRing, SigningKey};
use secrecy::ExposeSecret;
use service_config::{Config, InboundWebhooksConfig};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::BootstrapError;

/// Build a receiver from the immutable startup snapshot. An empty endpoint
/// map is a retained, inert route; an active endpoint cannot reach listener
/// admission without PostgreSQL and every referenced key. Consumer bindings
/// are the worker's: an admitted receipt is durable until one processes it.
pub(super) fn prepare(
    config: &Config,
    postgres_pool: Option<&PgPool>,
    background: &mut JoinSet<()>,
    cancel: &CancellationToken,
) -> Result<WebhookState, BootstrapError> {
    let webhooks = &config.inbound_webhooks;
    if webhooks.endpoints.is_empty() {
        return Ok(WebhookState::inert());
    }
    // Configuration validation already requires `postgres.enabled` here.
    let pool = postgres_pool.ok_or_else(|| {
        service_config::ValidationError::new(
            "postgres.enabled",
            "must be true when inbound webhook endpoints are configured",
        )
    })?;
    let mut bindings = Vec::with_capacity(webhooks.endpoints.len());
    for (endpoint_id, endpoint) in &webhooks.endpoints {
        let active = signing_key(webhooks, endpoint_id, &endpoint.active_key)?;
        let previous = endpoint
            .previous_key
            .as_deref()
            .map(|key| signing_key(webhooks, endpoint_id, key))
            .transpose()?;
        bindings.push((endpoint_id.clone(), KeyRing::new(active, previous)));
    }
    let receiver = Receiver::new(pool.clone(), bindings);
    background.spawn(receiver.clone().run_cleanup(cancel.child_token()));
    Ok(WebhookState::active(receiver))
}

/// Decode the secret that one endpoint's key reference names. The worker
/// shares the endpoint section but holds no secrets, so this check is ours.
fn signing_key(
    webhooks: &InboundWebhooksConfig,
    endpoint: &str,
    key: &str,
) -> Result<SigningKey, BootstrapError> {
    let secret =
        webhooks
            .secrets
            .get(key)
            .ok_or_else(|| BootstrapError::InboundWebhookKeyReference {
                endpoint: endpoint.to_owned(),
                key: key.to_owned(),
            })?;
    SigningKey::from_encoded(secret.expose_secret()).map_err(|source| {
        BootstrapError::InboundWebhookKey {
            endpoint: endpoint.to_owned(),
            key: key.to_owned(),
            source,
        }
    })
}
