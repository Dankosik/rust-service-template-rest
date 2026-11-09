//! Bounded `JetStream` publication and typed event delivery.
//!
//! Composition supplies validated configuration and routes. This crate owns
//! Go-compatible bytes, broker finality and consumer settlement; feature code
//! receives only typed events and a cancellation token.
//!
//! Preparation obligations on the caller are listed on [`PreparedEvent::prepare`].

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
    CloseOutcome, ConsumerOptions, Credentials, Messaging, MessagingOptions, MessagingProbe,
    MessagingStartup,
};
pub use prepared::{PreparedEvent, PublishAck};
pub use producer::Producer;
pub use registry::{Registry, Route};
