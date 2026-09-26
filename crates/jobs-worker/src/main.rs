//! The jobs worker composition registers each retained capability.
//! A derived service supplies its own adapters and business handlers.

use std::process::ExitCode;

// template:begin inbound-webhooks:worker-webhooks-inbound-imports
use infra_webhooks::inbound::{Consumers, Processor};
// template:end inbound-webhooks:worker-webhooks-inbound-imports
// template:begin webhooks:worker-webhooks-outbound-imports
use infra_webhooks::outbound::{Endpoint, Outbound};
use infra_webhooks::protocol::SigningKey;
use secrecy::ExposeSecret;
// template:end webhooks:worker-webhooks-outbound-imports

// template:begin webhooks:worker-webhooks-outbound-registration
#[derive(Debug, thiserror::Error)]
enum RegistrationError {
    #[error("outbound webhook endpoint {endpoint} references unavailable key {key}")]
    OutboundKeyReference { endpoint: String, key: String },
    #[error("outbound webhook key {key} is invalid: {source}")]
    OutboundKey {
        key: String,
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
        .map(|(endpoint_id, endpoint)| {
            (
                endpoint_id.clone(),
                Endpoint::new(
                    endpoint.url.clone(),
                    endpoint.active_key.clone(),
                    endpoint.previous_key.clone(),
                ),
            )
        })
        .collect();
    for (endpoint_id, endpoint) in &config.webhooks.endpoints {
        for reference in std::iter::once(&endpoint.active_key).chain(endpoint.previous_key.iter()) {
            if !config.webhooks.secrets.contains_key(reference) {
                return Err(RegistrationError::OutboundKeyReference {
                    endpoint: endpoint_id.clone(),
                    key: reference.clone(),
                }
                .into());
            }
        }
    }
    let outbound = Outbound::new(endpoints, config.jobs.max_workers()?)?;
    let keys = config
        .webhooks
        .secrets
        .iter()
        .map(|(reference, secret)| {
            SigningKey::from_encoded(secret.expose_secret())
                .map(|key| (reference.clone(), key))
                .map_err(|source| RegistrationError::OutboundKey {
                    key: reference.clone(),
                    source,
                })
        })
        .collect::<Result<_, _>>()?;
    outbound.dispatcher(keys).register(kinds);

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
    // Adopters insert real consumer adapters in both service and worker roots.
    kinds.register(
        infra_jobs::Policy::default(),
        Processor::new(Consumers::new()),
    );
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
