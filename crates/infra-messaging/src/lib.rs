//! Bounded `JetStream` publication and typed event delivery.
//!
//! Composition supplies validated configuration and routes. This crate owns
//! Go-compatible bytes, broker finality and consumer settlement; feature code
//! receives only typed events and a cancellation token.
//!
//! Generic preparation runs a trusted serializer once to completion, preserving
//! a late serialization error over a size refusal. Callers must bound source
//! bytes, collection cardinality and concurrent preparation within their own
//! deadline. Retained-output limits do not preempt synchronous callbacks or
//! limit their private allocations; see [`PreparedEvent::prepare`].

mod consumer;
mod contract;
mod credentials;
mod error;
mod messaging;
// template:begin outbox:messaging-outbox-module
pub mod outbox;
// template:end outbox:messaging-outbox-module
mod prepared;
mod producer;
mod registry;
mod trace;
pub mod wire;

pub use consumer::{Consumer, ConsumerError, ConsumerHandle};
pub use contract::ContractError;
pub use error::{HandlerError, MessagingError, PublishError, RegistryError};
pub use messaging::{
    CloseOutcome, ConsumerOptions, Messaging, MessagingOptions, MessagingProbe, MessagingStartup,
};
pub use prepared::{PreparedEvent, PublishAck};
pub use producer::Producer;
pub use registry::{Registry, Route};
