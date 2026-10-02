//! The jobs worker composition registers each retained capability.
//! A derived service supplies its own adapters and business handlers.

use std::process::ExitCode;

// jemalloc: 3-11% less worker CPU per job than glibc malloc, with no more
// resident memory over a 20-minute soak; see docs/backend-library-selection.md.
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// template:begin inbound-webhooks:worker-webhooks-inbound-imports
use infra_webhooks::inbound::{Consumers, Processor};
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
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    let config = registration.config();
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
    Dispatcher::new(endpoints).register(&mut registration.jobs);
    Ok(())
}
// template:end webhooks:worker-webhooks-outbound-registration

#[allow(
    clippy::unnecessary_wraps,
    unused_variables,
    reason = "independently retained profiles supply the registrations"
)]
fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    // template:begin webhooks:worker-webhooks-register-outbound
    register_outbound(registration)?;
    // template:end webhooks:worker-webhooks-register-outbound
    // template:begin inbound-webhooks:worker-webhooks-register-inbound
    // Bind each configured endpoint to its adapter here:
    // `consumers.insert("partner", Arc::new(Partner))?`. The template has no
    // business consumer, so a configured endpoint refuses startup below.
    let consumers = Consumers::new();
    consumers.require(
        registration
            .config()
            .inbound_webhooks
            .endpoints
            .keys()
            .map(String::as_str),
    )?;
    Processor::new(consumers).register(&mut registration.jobs);
    // template:end inbound-webhooks:worker-webhooks-register-inbound
    Ok(())
}

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), register)
}
