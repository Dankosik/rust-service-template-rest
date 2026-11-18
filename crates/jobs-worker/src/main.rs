use std::process::ExitCode;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// template:begin inbound-webhooks:worker-webhooks-inbound-imports
use infra_webhooks::inbound::{Consumers, Processor};
// template:end inbound-webhooks:worker-webhooks-inbound-imports
// template:begin webhooks:worker-outbound-webhooks-module
mod outbound_webhooks;
// template:end webhooks:worker-outbound-webhooks-module

#[allow(
    clippy::unnecessary_wraps,
    unused_variables,
    reason = "independently retained profiles supply the registrations"
)]
fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    // template:begin webhooks:worker-webhooks-register-outbound
    outbound_webhooks::register(registration)?;
    // template:end webhooks:worker-webhooks-register-outbound
    // template:begin inbound-webhooks:worker-webhooks-register-inbound
    let consumers = Consumers::new();
    consumers.require(registration.inbound_webhook_endpoints())?;
    Processor::new(consumers).register(&mut registration.jobs);
    // template:end inbound-webhooks:worker-webhooks-register-inbound
    Ok(())
}

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), register)
}
