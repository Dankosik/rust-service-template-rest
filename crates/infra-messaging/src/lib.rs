//! Bounded `JetStream` publication and typed event delivery.
//!
//! Composition supplies validated configuration and routes. This crate owns
//! Go-compatible bytes, broker finality and consumer settlement; feature code
//! receives only typed events and a cancellation token.
//!
//! Preparation obligations on the caller are listed on [`PreparedEvent::prepare`].

mod connection;
mod consumer;
mod contract;
mod credentials;
mod error;
mod messaging;
mod observe;
// template:begin outbox:messaging-outbox-module
pub mod outbox;
// template:end outbox:messaging-outbox-module
mod prepared;
mod probe;
mod producer;
mod registry;
mod topology;
mod trace;
pub mod wire;

pub use consumer::{Consumer, ConsumerError, ConsumerHandle};
pub use contract::ContractError;
pub use error::{EnvelopeError, HandlerError, MessagingError, PublishError};
pub use messaging::{
    BROKER_OPERATION_BUDGET, CloseOutcome, ConsumerOptions, Credentials, Messaging,
    MessagingOptions, MessagingStartup,
};
pub use prepared::PreparedEvent;
pub use probe::MessagingProbe;
pub use producer::{Producer, PublishAck};
pub use registry::{Registry, RegistryError, Route};
