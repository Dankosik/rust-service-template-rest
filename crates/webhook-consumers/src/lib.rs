//! Adopter-owned inbound webhook registration shared by both process roots.

use infra_webhooks::inbound::Consumers;

/// Register real provider adapters here for both service admission and worker
/// processing. The template has no business consumer and returns an empty map.
#[must_use]
pub fn consumers() -> Consumers {
    Consumers::new()
}
