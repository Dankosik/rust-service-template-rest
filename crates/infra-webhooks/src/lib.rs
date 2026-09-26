//! Standard Webhooks signing and durable work on the existing jobs queue.

pub mod protocol;
// template:begin webhooks:webhooks-outbound-module
pub mod outbound;
// template:end webhooks:webhooks-outbound-module
// template:begin inbound-webhooks:webhooks-inbound-module
pub mod inbound;
// template:end inbound-webhooks:webhooks-inbound-module
