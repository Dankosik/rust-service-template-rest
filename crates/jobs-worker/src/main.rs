//! The jobs worker composition registers each retained capability.
//! A derived service supplies its own adapters and business handlers.

use std::process::ExitCode;

// template:begin inbound-webhooks:worker-webhooks-inbound-imports
use infra_webhooks::inbound::Processor;
// template:end inbound-webhooks:worker-webhooks-inbound-imports
// template:begin webhooks:worker-webhooks-outbound-imports
use std::collections::BTreeMap;

use infra_webhooks::outbound::{Dispatcher, Endpoint};
use infra_webhooks::protocol::KeyRing;
use secrecy::ExposeSecret;
// template:end webhooks:worker-webhooks-outbound-imports

// template:begin webhooks:worker-webhooks-outbound-registration
#[derive(Debug, thiserror::Error)]
enum RegistrationError {
    #[error("outbound webhook endpoint {endpoint} signing key is invalid: {source}")]
    OutboundKey {
        endpoint: String,
        #[source]
        source: infra_webhooks::protocol::ProtocolError,
    },
}

fn register_outbound(
    kinds: &mut infra_jobs::Kinds,
    support: &jobs_worker::Support<'_>,
) -> Result<(), jobs_worker::BuildError> {
    let config = support.config();
    let mut endpoints = BTreeMap::new();
    for (endpoint_id, endpoint) in &config.webhooks.endpoints {
        let keys = KeyRing::from_encoded(
            endpoint.secret.expose_secret(),
            endpoint
                .previous_secret
                .as_ref()
                .map(ExposeSecret::expose_secret),
        )
        .map_err(|source| RegistrationError::OutboundKey {
            endpoint: endpoint_id.clone(),
            source,
        })?;
        endpoints.insert(endpoint_id.clone(), Endpoint::new(&endpoint.url, keys)?);
    }
    Dispatcher::new(endpoints).register(kinds);
    Ok(())
}
// template:end webhooks:worker-webhooks-outbound-registration

#[allow(
    clippy::unnecessary_wraps,
    unused_variables,
    reason = "the registration signature stays fixed across independently retained profiles"
)]
fn register(
    // template:begin jobs:worker-main-register-jobs-parameter
    kinds: &mut infra_jobs::Kinds,
    // template:end jobs:worker-main-register-jobs-parameter
    // template:begin messaging:worker-main-register-messaging-parameter
    _messages: &mut infra_messaging::Registry,
    // template:end messaging:worker-main-register-messaging-parameter
    support: &jobs_worker::Support<'_>,
) -> Result<(), jobs_worker::BuildError> {
    // template:begin webhooks:worker-webhooks-register-outbound
    register_outbound(kinds, support)?;
    // template:end webhooks:worker-webhooks-register-outbound
    // template:begin inbound-webhooks:worker-webhooks-register-inbound
    let consumers = webhook_consumers::consumers();
    consumers.require(
        support
            .config()
            .inbound_webhooks
            .endpoints
            .keys()
            .map(String::as_str),
    )?;
    Processor::new(consumers).register(kinds);
    // template:end inbound-webhooks:worker-webhooks-register-inbound
    Ok(())
}

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), register)
}
