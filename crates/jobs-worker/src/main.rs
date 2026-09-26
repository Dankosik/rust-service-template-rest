//! The jobs worker composition registers each retained capability.
//! A derived service supplies its own adapters and business handlers.

use std::process::ExitCode;

// template:begin inbound-webhooks:worker-webhooks-inbound-imports
use infra_webhooks::inbound::Processor;

#[derive(Debug, thiserror::Error)]
#[error("inbound webhook endpoint {endpoint} has no consumer binding")]
struct MissingInboundConsumer {
    endpoint: String,
}
// template:end inbound-webhooks:worker-webhooks-inbound-imports
// template:begin webhooks:worker-webhooks-outbound-imports
use infra_webhooks::outbound::{Endpoint, Outbound};
use infra_webhooks::protocol::KeyRing;
use secrecy::ExposeSecret;
// template:end webhooks:worker-webhooks-outbound-imports

// template:begin webhooks:worker-webhooks-outbound-registration
#[derive(Debug, thiserror::Error)]
enum RegistrationError {
    #[error("outbound webhook signing key is invalid: {source}")]
    OutboundKey {
        #[source]
        source: infra_webhooks::protocol::ProtocolError,
    },
}

fn register_outbound(
    kinds: &mut infra_jobs::Kinds,
    support: &jobs_worker::Support<'_>,
) -> Result<(), jobs_worker::BuildError> {
    let config = support.config();
    let endpoints = config
        .webhooks
        .endpoints
        .iter()
        .map(|(endpoint_id, endpoint)| (endpoint_id.clone(), Endpoint::new(endpoint.url.clone())))
        .collect();
    let outbound = Outbound::new(endpoints)?;
    let keys = config
        .webhooks
        .endpoints
        .iter()
        .map(|(endpoint_id, endpoint)| {
            KeyRing::from_encoded(
                endpoint.secret.expose_secret(),
                endpoint
                    .previous_secret
                    .as_ref()
                    .map(ExposeSecret::expose_secret),
            )
            .map(|ring| (endpoint_id.clone(), ring))
            .map_err(|source| RegistrationError::OutboundKey { source })
        })
        .collect::<Result<_, _>>()?;
    outbound
        .dispatcher(keys, config.jobs.max_workers()?)?
        .register(kinds);

    Ok(())
}
// template:end webhooks:worker-webhooks-outbound-registration

// template:begin webhooks-common:worker-webhooks-register-prefix
#[allow(
    clippy::unnecessary_wraps,
    unused_variables,
    reason = "the registration signature stays fixed across independently retained profiles"
)]
fn register(
    kinds: &mut infra_jobs::Kinds,
    support: &jobs_worker::Support<'_>,
) -> Result<(), jobs_worker::BuildError> {
    // template:end webhooks-common:worker-webhooks-register-prefix
    // template:begin webhooks:worker-webhooks-register-outbound
    register_outbound(kinds, support)?;
    // template:end webhooks:worker-webhooks-register-outbound
    // template:begin inbound-webhooks:worker-webhooks-register-inbound
    let consumers = webhook_consumers::consumers();
    for endpoint_id in support.config().inbound_webhooks.endpoints.keys() {
        if !consumers.contains(endpoint_id) {
            return Err(MissingInboundConsumer {
                endpoint: endpoint_id.clone(),
            }
            .into());
        }
    }
    kinds.register(infra_jobs::Policy::default(), Processor::new(consumers));
    // template:end inbound-webhooks:worker-webhooks-register-inbound
    // template:begin webhooks-common:worker-webhooks-register-suffix
    Ok(())
}
// template:end webhooks-common:worker-webhooks-register-suffix

fn main() -> ExitCode {
    #[allow(
        unused_variables,
        reason = "retained profiles replace the inert registration"
    )]
    let registration: Option<jobs_worker::Register> = None;
    // template:begin webhooks-common:worker-webhooks-registration
    let registration = Some(register as jobs_worker::Register);
    // template:end webhooks-common:worker-webhooks-registration
    jobs_worker::run(std::env::args_os(), registration)
}
