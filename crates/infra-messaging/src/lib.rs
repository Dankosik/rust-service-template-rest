//! Bounded JetStream publication and typed event delivery.
//!
//! Composition supplies validated configuration and routes. This crate owns
//! Go-compatible bytes, broker finality and consumer settlement; feature code
//! receives only typed events and a cancellation token.

mod consumer;
mod error;
mod messaging;
// template:begin outbox:messaging-outbox-module
pub mod outbox;
// template:end outbox:messaging-outbox-module
mod prepared;
mod producer;
mod registry;
pub mod wire;

pub use consumer::{Consumer, ConsumerError, ConsumerHandle};
pub use error::{HandlerError, MessagingError, PublishError, RegistryError};
pub use messaging::{CloseOutcome, ConsumerOptions, Messaging, MessagingOptions, MessagingProbe};
pub use prepared::{PreparedEvent, PublishAck};
pub use producer::Producer;
pub use registry::{Registry, Route};
