// Copyright 2020-2022 The NATS Authors
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A Rust asynchronous client for the NATS.io ecosystem.
//!
//! To access the repository, you can clone it by running:
//!
//! ```bash
//! git clone https://github.com/nats-io/nats.rs
//! ````
//! NATS.io is a simple, secure, and high-performance open-source messaging
//! system designed for cloud-native applications, IoT messaging, and microservices
//! architectures.
//!
//! **Note**: The synchronous NATS API is deprecated and no longer actively maintained. If you need to use the deprecated synchronous API, you can refer to:
//! <https://crates.io/crates/nats>
//!
//! For more information on NATS.io visit: <https://nats.io>
//!
//! ## Examples
//!
//! Below, you can find some basic examples on how to use this library.
//!
//! For more details, please refer to the specific methods and structures documentation.
//!
//! ### Complete example
//!
//! Connect to the NATS server, publish messages and subscribe to receive messages.
//!
//! ```no_run
//! use bytes::Bytes;
//! use futures_util::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), async_nats::Error> {
//!     // Connect to the NATS server
//!     let client = async_nats::connect("demo.nats.io").await?;
//!
//!     // Subscribe to the "messages" subject
//!     let mut subscriber = client.subscribe("messages").await?;
//!
//!     // Publish messages to the "messages" subject
//!     for _ in 0..10 {
//!         client.publish("messages", "data".into()).await?;
//!     }
//!
//!     // Receive and process messages
//!     while let Some(message) = subscriber.next().await {
//!         println!("Received message {:?}", message);
//!     }
//!
//!     Ok(())
//! }
//! ```
//!
//! ### Publish
//!
//! Connect to the NATS server and publish messages to a subject.
//!
//! ```
//! # use bytes::Bytes;
//! # use std::error::Error;
//! # use std::time::Instant;
//! # #[tokio::main]
//! # async fn main() -> Result<(), async_nats::Error> {
//! // Connect to the NATS server
//! let client = async_nats::connect("demo.nats.io").await?;
//!
//! // Prepare the subject and data
//! let subject = "foo";
//! let data = Bytes::from("bar");
//!
//! // Publish messages to the NATS server
//! for _ in 0..10 {
//!     client.publish(subject, data.clone()).await?;
//! }
//!
//! // Flush internal buffer before exiting to make sure all messages are sent
//! client.flush().await?;
//!
//! #    Ok(())
//! # }
//! ```
//!
//! ### Subscribe
//!
//! Connect to the NATS server, subscribe to a subject and receive messages.
//!
//! ```no_run
//! # use bytes::Bytes;
//! # use futures_util::StreamExt;
//! # use std::error::Error;
//! # use std::time::Instant;
//! # #[tokio::main]
//! # async fn main() -> Result<(), async_nats::Error> {
//! // Connect to the NATS server
//! let client = async_nats::connect("demo.nats.io").await?;
//!
//! // Subscribe to the "foo" subject
//! let mut subscriber = client.subscribe("foo").await.unwrap();
//!
//! // Receive and process messages
//! while let Some(message) = subscriber.next().await {
//!     println!("Received message {:?}", message);
//! }
//! #     Ok(())
//! # }
//! ```
//!
//! ### JetStream
//!
//! To access JetStream API, create a JetStream [jetstream::Context].
//!
//! ```no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), async_nats::Error> {
//! // Connect to the NATS server
//! let client = async_nats::connect("demo.nats.io").await?;
//! // Create a JetStream context.
//! let jetstream = async_nats::jetstream::new(client);
//!
//! // Publish JetStream messages, manage streams, consumers, etc.
//! jetstream.publish("foo", "bar".into()).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ### Key-value Store
//!
//! Key-value [Store][jetstream::kv::Store] is accessed through [jetstream::Context].
//!
//! ```no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), async_nats::Error> {
//! // Connect to the NATS server
//! let client = async_nats::connect("demo.nats.io").await?;
//! // Create a JetStream context.
//! let jetstream = async_nats::jetstream::new(client);
//! // Access an existing key-value.
//! let kv = jetstream.get_key_value("store").await?;
//! # Ok(())
//! # }
//! ```
//! ### Object Store
//!
//! Object [Store][jetstream::object_store::ObjectStore] is accessed through [jetstream::Context].
//!
//! ```no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), async_nats::Error> {
//! // Connect to the NATS server
//! let client = async_nats::connect("demo.nats.io").await?;
//! // Create a JetStream context.
//! let jetstream = async_nats::jetstream::new(client);
//! // Access an existing key-value.
//! let kv = jetstream.get_object_store("store").await?;
//! # Ok(())
//! # }
//! ```
//! ### Service API
//!
//! [Service API][service::Service] is accessible through [Client] after importing its trait.
//!
//! ```no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), async_nats::Error> {
//! use async_nats::service::ServiceExt;
//! // Connect to the NATS server
//! let client = async_nats::connect("demo.nats.io").await?;
//! let mut service = client
//!     .service_builder()
//!     .description("some service")
//!     .stats_handler(|endpoint, stats| serde_json::json!({ "endpoint": endpoint }))
//!     .start("products", "1.0.0")
//!     .await?;
//! # Ok(())
//! # }
//! ```

#![deny(unreachable_pub)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_codeblock_attributes)]
#![deny(rustdoc::invalid_rust_codeblocks)]
#![cfg_attr(docsrs, feature(doc_cfg))]

use thiserror::Error;

use futures_util::stream::Stream;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tracing::{debug, error};

use core::fmt;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt::Display;
use std::future::Future;
use std::iter;
use std::mem;
use std::net::SocketAddr;
use std::option;
use std::pin::Pin;
use std::slice;
use std::str::{self, FromStr};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll};
use tokio::io::ErrorKind;
use tokio::time::{Duration, Interval, MissedTickBehavior, interval};
use url::{Host, Url};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use tokio::io;
use tokio::sync::mpsc;
use tokio::task;

pub type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LANG: &str = "rust";
const MAX_PENDING_PINGS: usize = 2;
const MULTIPLEXER_SID: u64 = 0;
const MULTIPLEXER_PRUNE_MIN: usize = 256;
pub(crate) const DEFAULT_SERVER_MAX_PAYLOAD: usize = 1024 * 1024;

/// A re-export of the `rustls` crate used in this crate,
/// for use in cases where manual client configurations
/// must be provided using `Options::tls_client_config`.
pub use tokio_rustls::rustls;

use connection::{Connection, State};
use connector::{Connector, ConnectorOptions};
pub use connector::{ReconnectToServer, Server};
pub use header::{HeaderMap, HeaderName, HeaderValue};
pub use subject::{Subject, SubjectError, ToSubject};

mod auth;
pub(crate) mod auth_utils;
pub mod client;
pub mod connection;
mod connector;
mod options;

pub use auth::Auth;
pub use client::{
    Client, PublishError, PublishErrorKind, Request, RequestError, RequestErrorKind,
    ServerPoolError, ServerPoolErrorKind, SetServerPoolError, SetServerPoolErrorKind, Statistics,
    SubscribeError, SubscribeErrorKind,
};
pub use options::{AuthError, ConnectOptions};

#[cfg(feature = "crypto")]
#[cfg_attr(docsrs, doc(cfg(feature = "crypto")))]
mod crypto;
#[cfg(any(feature = "jetstream", feature = "service", feature = "chrono"))]
#[cfg_attr(
    docsrs,
    doc(cfg(any(feature = "jetstream", feature = "service", feature = "chrono")))
)]
pub mod datetime;

pub mod error;
pub mod header;
mod id_generator;
#[cfg(feature = "jetstream")]
#[cfg_attr(docsrs, doc(cfg(feature = "jetstream")))]
pub mod jetstream;
pub mod message;
#[cfg(feature = "service")]
#[cfg_attr(docsrs, doc(cfg(feature = "service")))]
pub mod service;
pub mod status;
pub mod subject;
mod tls;

pub use message::Message;
pub use status::StatusCode;

/// Information sent by the server back to this client
/// during initial connection, and possibly again later.
#[derive(Debug, Deserialize, Default, Clone, Eq, PartialEq)]
pub struct ServerInfo {
    /// The unique identifier of the NATS server.
    #[serde(default)]
    pub server_id: String,
    /// Generated Server Name.
    #[serde(default)]
    pub server_name: String,
    /// The host specified in the cluster parameter/options.
    #[serde(default)]
    pub host: String,
    /// The port number specified in the cluster parameter/options.
    #[serde(default)]
    pub port: u16,
    /// The version of the NATS server.
    #[serde(default)]
    pub version: String,
    /// If this is set, then the server should try to authenticate upon
    /// connect.
    #[serde(default)]
    pub auth_required: bool,
    /// If this is set, then the server must authenticate using TLS.
    #[serde(default)]
    pub tls_required: bool,
    /// Maximum payload size that the server will accept.
    #[serde(default)]
    pub max_payload: usize,
    /// The protocol version in use.
    #[serde(default)]
    pub proto: i8,
    /// The server-assigned client ID. This may change during reconnection.
    #[serde(default)]
    pub client_id: u64,
    /// The version of golang the NATS server was built with.
    #[serde(default)]
    pub go: String,
    /// The nonce used for nkeys.
    #[serde(default)]
    pub nonce: String,
    /// A list of server urls that a client can connect to.
    #[serde(default)]
    pub connect_urls: Vec<String>,
    /// The client IP as known by the server.
    #[serde(default)]
    pub client_ip: String,
    /// Whether the server supports headers.
    #[serde(default)]
    pub headers: bool,
    /// Whether server goes into lame duck mode.
    #[serde(default, rename = "ldm")]
    pub lame_duck_mode: bool,
    /// Name of the cluster if the server is in cluster-mode
    #[serde(default)]
    pub cluster: Option<String>,
    /// The configured NATS domain of the server.
    #[serde(default)]
    pub domain: Option<String>,
    /// Whether the server supports JetStream.
    #[serde(default)]
    pub jetstream: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ServerOp {
    Ok,
    Info(Box<ServerInfo>),
    Ping,
    Pong,
    Error(ServerError),
    Message {
        sid: u64,
        subject: Subject,
        reply: Option<Subject>,
        payload: Bytes,
        headers: Option<HeaderMap>,
        status: Option<StatusCode>,
        description: Option<String>,
        length: usize,
    },
}

/// An alias. This is done to avoid breaking changes
/// in the public API. However this will get deprecated in the future in favor of
/// [crate::message::OutboundMessage].
#[deprecated(
    since = "0.44.0",
    note = "use `async_nats::message::OutboundMessage` instead"
)]
pub type PublishMessage = crate::message::OutboundMessage;

/// `Command` represents all commands that a [`Client`] can handle
#[derive(Debug)]
pub(crate) enum Command {
    Publish(OutboundMessage),
    Request {
        subject: Subject,
        payload: Bytes,
        respond: Subject,
        headers: Option<HeaderMap>,
        sender: oneshot::Sender<Message>,
    },
    Subscribe {
        sid: u64,
        subject: Subject,
        queue_group: Option<String>,
        sender: mpsc::Sender<Message>,
    },
    Unsubscribe {
        sid: u64,
        max: Option<u64>,
    },
    Flush {
        observer: oneshot::Sender<()>,
    },
    Drain {
        sid: Option<u64>,
    },
    Reconnect,
    SetServerPool {
        servers: Vec<ServerAddr>,
        result: oneshot::Sender<Result<(), String>>,
    },
    ServerPool {
        result: oneshot::Sender<Vec<connector::Server>>,
    },
}

/// `ClientOp` represents all actions of `Client`.
#[derive(Debug)]
pub(crate) enum ClientOp {
    Publish {
        subject: Subject,
        payload: Bytes,
        respond: Option<Subject>,
        headers: Option<HeaderMap>,
    },
    Subscribe {
        sid: u64,
        subject: Subject,
        queue_group: Option<String>,
    },
    Unsubscribe {
        sid: u64,
        max: Option<u64>,
    },
    Ping,
    Pong,
    Connect(ConnectInfo),
}

#[derive(Debug)]
struct Subscription {
    subject: Subject,
    sender: mpsc::Sender<Message>,
    queue_group: Option<String>,
    delivered: u64,
    max: Option<u64>,
}

#[derive(Debug)]
struct Multiplexer {
    subject: Subject,
    prefix: Subject,
    senders: HashMap<String, oneshot::Sender<Message>>,
    /// Size of `senders` at which senders of abandoned requests are pruned: twice the smallest
    /// size since the last prune, and at least [`MULTIPLEXER_PRUNE_MIN`].
    prune_at: usize,
}

impl Multiplexer {
    fn insert(&mut self, token: String, sender: oneshot::Sender<Message>) {
        if self.senders.len() >= self.prune_at {
            self.prune();
        }
        self.senders.insert(token, sender);
    }

    fn remove(&mut self, token: &str) -> Option<oneshot::Sender<Message>> {
        let sender = self.senders.remove(token)?;
        // Follow the map down as replies drain it, so that abandoned requests cannot pile up to
        // the size of a past burst before the next prune.
        self.prune_at = self
            .prune_at
            .min((self.senders.len() * 2).max(MULTIPLEXER_PRUNE_MIN));
        Some(sender)
    }

    /// Removes the senders of requests whose callers stopped waiting, e.g. after a timeout.
    ///
    /// Such a request dropped its receiver and will never read a reply, so nothing else would
    /// ever remove its sender. At least half of `prune_at` requests are inserted between two
    /// prunes, which keeps pruning amortized O(1) per request.
    fn prune(&mut self) {
        let len = self.senders.len();
        self.senders.retain(|_, pending| !pending.is_closed());
        self.prune_at = (self.senders.len() * 2).max(MULTIPLEXER_PRUNE_MIN);
        // `retain` visits every bucket, so give back the capacity a past burst left behind.
        if self.senders.capacity() > 4 * self.prune_at {
            self.senders.shrink_to(self.prune_at);
        }
        debug!(
            "pruned {} abandoned requests, {} pending",
            len - self.senders.len(),
            self.senders.len()
        );
    }
}

/// A connection handler which facilitates communication from channels to a single shared connection.
pub(crate) struct ConnectionHandler {
    connection: Connection,
    connector: Connector,
    subscriptions: HashMap<u64, Subscription>,
    multiplexer: Option<Multiplexer>,
    pending_pings: usize,
    info_sender: tokio::sync::watch::Sender<Option<ServerInfo>>,
    ping_interval: Interval,
    should_reconnect: bool,
    flush_observers: Vec<oneshot::Sender<()>>,
    is_draining: bool,
    drain_pings: VecDeque<u64>,
}

impl ConnectionHandler {
    pub(crate) fn new(
        connection: Connection,
        connector: Connector,
        info_sender: tokio::sync::watch::Sender<Option<ServerInfo>>,
        ping_period: Duration,
    ) -> ConnectionHandler {
        let mut ping_interval = interval(ping_period);
        ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        ConnectionHandler {
            connection,
            connector,
            subscriptions: HashMap::new(),
            multiplexer: None,
            pending_pings: 0,
            info_sender,
            ping_interval,
            should_reconnect: false,
            flush_observers: Vec::new(),
            is_draining: false,
            drain_pings: VecDeque::new(),
        }
    }

    pub(crate) async fn process<'a>(&'a mut self, receiver: &'a mut mpsc::Receiver<Command>) {
        struct ProcessFut<'a> {
            handler: &'a mut ConnectionHandler,
            receiver: &'a mut mpsc::Receiver<Command>,
            recv_buf: &'a mut Vec<Command>,
        }

        enum ExitReason {
            Disconnected(Option<io::Error>),
            ReconnectRequested,
            Closed,
        }

        impl ProcessFut<'_> {
            const RECV_CHUNK_SIZE: usize = 16;

            #[cold]
            fn ping(&mut self) -> Poll<ExitReason> {
                self.handler.pending_pings += 1;

                if self.handler.pending_pings > MAX_PENDING_PINGS {
                    debug!(
                        pending_pings = self.handler.pending_pings,
                        max_pings = MAX_PENDING_PINGS,
                        "disconnecting due to too many pending pings"
                    );

                    Poll::Ready(ExitReason::Disconnected(None))
                } else {
                    self.handler.connection.enqueue_write_op(&ClientOp::Ping);

                    Poll::Pending
                }
            }
        }

        impl Future for ProcessFut<'_> {
            type Output = ExitReason;

            /// Drives the connection forward.
            ///
            /// Returns one of the following:
            ///
            /// * `Poll::Pending` means that the connection
            ///   is blocked on all fronts or there are
            ///   no commands to send or receive
            /// * `Poll::Ready(ExitReason::Disconnected(_))` means
            ///   that an I/O operation failed and the connection
            ///   is considered dead.
            /// * `Poll::Ready(ExitReason::Closed)` means that
            ///   [`Self::receiver`] was closed, so there's nothing
            ///   more for us to do than to exit the client.
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                // We need to be sure the waker is registered, therefore we need to poll until we
                // get a `Poll::Pending`. With a sane interval delay, this means that the loop
                // breaks at the second iteration.
                while self.handler.ping_interval.poll_tick(cx).is_ready() {
                    if let Poll::Ready(exit) = self.ping() {
                        return Poll::Ready(exit);
                    }
                }

                loop {
                    match self.handler.connection.poll_read_op(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Ok(Some(server_op))) => {
                            self.handler.handle_server_op(server_op);
                        }
                        Poll::Ready(Ok(None)) => {
                            return Poll::Ready(ExitReason::Disconnected(None));
                        }
                        Poll::Ready(Err(err)) => {
                            return Poll::Ready(ExitReason::Disconnected(Some(err)));
                        }
                    }
                }

                // Before handling any commands, drop any subscriptions which are draining
                // Note: safe to assume subscription drain has completed at this point, as we would have flushed
                // all outgoing UNSUB messages in the previous call to this fn, and we would have processed and
                // delivered any remaining messages to the subscription in the loop above.
                while let Some(sid) = self.handler.drain_pings.pop_front() {
                    self.handler.subscriptions.remove(&sid);
                }

                if self.handler.is_draining {
                    // The entire connection is draining. This means we flushed outgoing messages in the previous
                    // call to this fn, we handled any remaining messages from the server in the loop above, and
                    // all subs were drained, so drain is complete and we should exit instead of processing any
                    // further messages
                    return Poll::Ready(ExitReason::Closed);
                }

                // WARNING: after the following loop `handle_command`,
                // or other functions which call `enqueue_write_op`,
                // cannot be called anymore. Runtime wakeups won't
                // trigger a call to `poll_write`

                let mut made_progress = true;
                loop {
                    while !self.handler.connection.is_write_buf_full() {
                        debug_assert!(self.recv_buf.is_empty());

                        let Self {
                            recv_buf,
                            handler,
                            receiver,
                        } = &mut *self;
                        match receiver.poll_recv_many(cx, recv_buf, Self::RECV_CHUNK_SIZE) {
                            Poll::Pending => break,
                            Poll::Ready(1..) => {
                                made_progress = true;

                                for cmd in recv_buf.drain(..) {
                                    handler.handle_command(cmd);
                                }
                            }
                            // TODO: replace `_` with `0` after bumping MSRV to 1.75
                            Poll::Ready(_) => return Poll::Ready(ExitReason::Closed),
                        }
                    }

                    // The first round will poll both from
                    // the `receiver` and the writer, giving
                    // them both a chance to make progress
                    // and register `Waker`s.
                    //
                    // If writing is `Poll::Pending` we exit.
                    //
                    // If writing is completed we can repeat the entire
                    // cycle as long as the `receiver` doesn't end-up
                    // `Poll::Pending` immediately.
                    if !mem::take(&mut made_progress) {
                        break;
                    }

                    match self.handler.connection.poll_write(cx) {
                        Poll::Pending => {
                            // Write buffer couldn't be fully emptied
                            break;
                        }
                        Poll::Ready(Ok(())) => {
                            // Write buffer is empty
                            continue;
                        }
                        Poll::Ready(Err(err)) => {
                            return Poll::Ready(ExitReason::Disconnected(Some(err)));
                        }
                    }
                }

                if let (ShouldFlush::Yes, _) | (ShouldFlush::No, false) = (
                    self.handler.connection.should_flush(),
                    self.handler.flush_observers.is_empty(),
                ) {
                    match self.handler.connection.poll_flush(cx) {
                        Poll::Pending => {}
                        Poll::Ready(Ok(())) => {
                            for observer in self.handler.flush_observers.drain(..) {
                                let _ = observer.send(());
                            }
                        }
                        Poll::Ready(Err(err)) => {
                            return Poll::Ready(ExitReason::Disconnected(Some(err)));
                        }
                    }
                }

                if mem::take(&mut self.handler.should_reconnect) {
                    return Poll::Ready(ExitReason::ReconnectRequested);
                }

                Poll::Pending
            }
        }

        let mut recv_buf = Vec::with_capacity(ProcessFut::RECV_CHUNK_SIZE);
        loop {
            let process = ProcessFut {
                handler: self,
                receiver,
                recv_buf: &mut recv_buf,
            };
            match process.await {
                ExitReason::Disconnected(err) => {
                    debug!(error = ?err, "disconnected");
                    if self.handle_disconnect().await.is_err() {
                        break;
                    };
                    debug!("reconnected");
                }
                ExitReason::Closed => {
                    break;
                }
                ExitReason::ReconnectRequested => {
                    debug!("reconnect requested");
                    // Should be ok to ingore error, as that means we are not in connected state.
                    self.connection.stream.shutdown().await.ok();
                    if self.handle_disconnect().await.is_err() {
                        break;
                    };
                }
            }
        }
    }

    fn handle_server_op(&mut self, server_op: ServerOp) {
        self.ping_interval.reset();

        match server_op {
            ServerOp::Ping => {
                debug!("received PING");
                self.connection.enqueue_write_op(&ClientOp::Pong);
            }
            ServerOp::Pong => {
                debug!("received PONG");
                self.pending_pings = self.pending_pings.saturating_sub(1);
            }
            ServerOp::Error(error) => {
                debug!("received ERROR: {:?}", error);
                self.connector
                    .events_tx
                    .try_send(Event::ServerError(error))
                    .ok();
            }
            ServerOp::Message {
                sid,
                subject,
                reply,
                payload,
                headers,
                status,
                description,
                length,
            } => {
                debug!("received MESSAGE: sid={}, subject={}", sid, subject);
                self.connector
                    .connect_stats
                    .in_messages
                    .add(1, Ordering::Relaxed);

                if let Some(subscription) = self.subscriptions.get_mut(&sid) {
                    let message: Message = Message {
                        subject,
                        reply,
                        payload,
                        headers,
                        status,
                        description,
                        length,
                    };

                    // if the channel for subscription was dropped, remove the
                    // subscription from the map and unsubscribe.
                    match subscription.sender.try_send(message) {
                        Ok(_) => {
                            subscription.delivered += 1;
                            // if this `Subscription` has set `max` value, check if it
                            // was reached. If yes, remove the `Subscription` and in
                            // the result, `drop` the `sender` channel.
                            if let Some(max) = subscription.max {
                                if subscription.delivered.ge(&max) {
                                    debug!("max messages reached for subscription {}", sid);
                                    self.subscriptions.remove(&sid);
                                }
                            }
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            debug!("slow consumer detected for subscription {}", sid);
                            self.connector
                                .events_tx
                                .try_send(Event::SlowConsumer(sid))
                                .ok();
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            debug!("subscription {} channel closed", sid);
                            self.subscriptions.remove(&sid);
                            self.connection
                                .enqueue_write_op(&ClientOp::Unsubscribe { sid, max: None });
                        }
                    }
                } else if sid == MULTIPLEXER_SID {
                    debug!("received message for multiplexer");
                    if let Some(multiplexer) = self.multiplexer.as_mut() {
                        let maybe_token =
                            subject.strip_prefix(multiplexer.prefix.as_ref()).to_owned();

                        if let Some(token) = maybe_token {
                            if let Some(sender) = multiplexer.remove(token) {
                                debug!("forwarding message to request with token {}", token);
                                let message = Message {
                                    subject,
                                    reply,
                                    payload,
                                    headers,
                                    status,
                                    description,
                                    length,
                                };

                                let _ = sender.send(message);
                            }
                        }
                    }
                }
            }
            // TODO: we should probably update advertised server list here too.
            ServerOp::Info(info) => {
                debug!("received INFO: server_id={}", info.server_id);
                if info.lame_duck_mode {
                    debug!("server in lame duck mode");
                    self.connector.events_tx.try_send(Event::LameDuckMode).ok();
                }
            }

            _ => {
                // TODO: don't ignore.
            }
        }
    }

    fn handle_command(&mut self, command: Command) {
        match command {
            Command::Unsubscribe { sid, max } => {
                if let Some(subscription) = self.subscriptions.get_mut(&sid) {
                    subscription.max = max;
                    match subscription.max {
                        Some(n) => {
                            if subscription.delivered >= n {
                                self.subscriptions.remove(&sid);
                            }
                        }
                        None => {
                            self.subscriptions.remove(&sid);
                        }
                    }

                    self.connection
                        .enqueue_write_op(&ClientOp::Unsubscribe { sid, max });
                }
            }
            Command::Flush { observer } => {
                self.flush_observers.push(observer);
            }
            Command::Drain { sid } => {
                let mut drain_sub = |sid: u64| {
                    self.drain_pings.push_back(sid);
                    self.connection
                        .enqueue_write_op(&ClientOp::Unsubscribe { sid, max: None });
                };

                if let Some(sid) = sid {
                    if self.subscriptions.get_mut(&sid).is_some() {
                        drain_sub(sid);
                    }
                } else {
                    // sid isn't set, so drain the whole client
                    self.connector.events_tx.try_send(Event::Draining).ok();
                    self.is_draining = true;
                    for &sid in self.subscriptions.keys() {
                        drain_sub(sid);
                    }
                }
                self.connection.enqueue_write_op(&ClientOp::Ping);
            }
            Command::Subscribe {
                sid,
                subject,
                queue_group,
                sender,
            } => {
                let subscription = Subscription {
                    sender,
                    delivered: 0,
                    max: None,
                    subject: subject.to_owned(),
                    queue_group: queue_group.to_owned(),
                };

                self.subscriptions.insert(sid, subscription);

                self.connection.enqueue_write_op(&ClientOp::Subscribe {
                    sid,
                    subject,
                    queue_group,
                });
            }
            Command::Request {
                subject,
                payload,
                respond,
                headers,
                sender,
            } => {
                let (prefix, token) = respond.rsplit_once('.').expect("malformed request subject");

                let multiplexer = if let Some(multiplexer) = self.multiplexer.as_mut() {
                    multiplexer
                } else {
                    let prefix = Subject::from(format!("{}.{}.", prefix, id_generator::next()));
                    let subject = Subject::from(format!("{prefix}*"));

                    self.connection.enqueue_write_op(&ClientOp::Subscribe {
                        sid: MULTIPLEXER_SID,
                        subject: subject.clone(),
                        queue_group: None,
                    });

                    self.multiplexer.insert(Multiplexer {
                        subject,
                        prefix,
                        senders: HashMap::new(),
                        prune_at: MULTIPLEXER_PRUNE_MIN,
                    })
                };
                self.connector
                    .connect_stats
                    .out_messages
                    .add(1, Ordering::Relaxed);

                multiplexer.insert(token.to_owned(), sender);

                let respond: Subject = format!("{}{}", multiplexer.prefix, token).into();

                let pub_op = ClientOp::Publish {
                    subject,
                    payload,
                    respond: Some(respond),
                    headers,
                };

                self.connection.enqueue_write_op(&pub_op);
            }

            Command::Publish(OutboundMessage {
                subject,
                payload,
                reply: respond,
                headers,
            }) => {
                self.connector
                    .connect_stats
                    .out_messages
                    .add(1, Ordering::Relaxed);

                let header_len = headers
                    .as_ref()
                    .map(|headers| headers.len())
                    .unwrap_or_default();

                self.connector.connect_stats.out_bytes.add(
                    (payload.len()
                        + respond.as_ref().map_or_else(|| 0, |r| r.len())
                        + subject.len()
                        + header_len) as u64,
                    Ordering::Relaxed,
                );

                self.connection.enqueue_write_op(&ClientOp::Publish {
                    subject,
                    payload,
                    respond,
                    headers,
                });
            }

            Command::Reconnect => {
                self.should_reconnect = true;
            }

            Command::SetServerPool { servers, result } => {
                let _ = result.send(self.connector.set_server_pool(servers));
            }

            Command::ServerPool { result } => {
                let _ = result.send(self.connector.server_pool());
            }
        }
    }

    async fn handle_disconnect(&mut self) -> Result<(), ConnectError> {
        self.pending_pings = 0;
        self.connector.events_tx.try_send(Event::Disconnected).ok();
        self.connector.state_tx.send(State::Disconnected).ok();

        self.handle_reconnect().await
    }

    async fn handle_reconnect(&mut self) -> Result<(), ConnectError> {
        let (info, connection) = self.connector.connect().await?;
        self.connection = connection;
        let _ = self.info_sender.send(Some(info));

        self.subscriptions
            .retain(|_, subscription| !subscription.sender.is_closed());

        for (sid, subscription) in &self.subscriptions {
            self.connection.enqueue_write_op(&ClientOp::Subscribe {
                sid: *sid,
                subject: subscription.subject.to_owned(),
                queue_group: subscription.queue_group.to_owned(),
            });

            if let Some(max) = subscription.max {
                self.connection.enqueue_write_op(&ClientOp::Unsubscribe {
                    sid: *sid,
                    max: Some(max.saturating_sub(subscription.delivered)),
                });
            }
        }

        if let Some(multiplexer) = &self.multiplexer {
            self.connection.enqueue_write_op(&ClientOp::Subscribe {
                sid: MULTIPLEXER_SID,
                subject: multiplexer.subject.to_owned(),
                queue_group: None,
            });
        }
        Ok(())
    }
}

/// Connects to NATS with specified options.
///
/// It is generally advised to use [ConnectOptions] instead, as it provides a builder for whole
/// configuration.
///
/// # Examples
/// ```
/// # #[tokio::main]
/// # async fn main() ->  Result<(), async_nats::Error> {
/// let mut nc =
///     async_nats::connect_with_options("demo.nats.io", async_nats::ConnectOptions::new()).await?;
/// nc.publish("test", "data".into()).await?;
/// # Ok(())
/// # }
/// ```
pub async fn connect_with_options<A: ToServerAddrs>(
    addrs: A,
    options: ConnectOptions,
) -> Result<Client, ConnectError> {
    let ping_period = options.ping_interval;

    let (events_tx, mut events_rx) = mpsc::channel(128);
    let (state_tx, state_rx) = tokio::sync::watch::channel(State::Pending);
    // We're setting it to the default server payload size.
    let max_payload = Arc::new(AtomicUsize::new(DEFAULT_SERVER_MAX_PAYLOAD));
    let statistics = Arc::new(Statistics::default());

    let mut connector = Connector::new(
        addrs,
        ConnectorOptions {
            tls_required: options.tls_required,
            certificates: options.certificates,
            client_key: options.client_key,
            client_cert: options.client_cert,
            tls_client_config: options.tls_client_config,
            tls_first: options.tls_first,
            auth: options.auth,
            no_echo: options.no_echo,
            connection_timeout: options.connection_timeout,
            initial_connect_deadline: options.initial_connect_deadline,
            name: options.name,
            ignore_discovered_servers: options.ignore_discovered_servers,
            retain_servers_order: options.retain_servers_order,
            read_buffer_capacity: options.read_buffer_capacity,
            reconnect_delay_callback: options.reconnect_delay_callback,
            auth_callback: options.auth_callback,
            max_reconnects: options.max_reconnects,
            local_address: options.local_address,
            reconnect_to_server_callback: options.reconnect_to_server_callback,
        },
        events_tx,
        state_tx,
        max_payload.clone(),
        statistics.clone(),
    )
    .map_err(|err| ConnectError::with_source(ConnectErrorKind::ServerParse, err))?;

    let mut info = None;
    let mut connection = None;
    if !options.retry_on_initial_connect {
        debug!("retry on initial connect failure is disabled");
        let (info_ok, connection_ok) = connector.try_connect().await?;
        connector.clear_initial_connect_deadline();
        connection = Some(connection_ok);
        info = Some(info_ok);
    }

    let (info_sender, info_watcher) = tokio::sync::watch::channel(info.clone());
    let (sender, mut receiver) = mpsc::channel(options.sender_capacity);
    let (close_sender, mut close_receiver) = tokio::sync::watch::channel(false);
    let (closed_sender, closed_receiver) = tokio::sync::watch::channel(false);
    let closed_events = connector.events_tx.clone();
    let closed_state = connector.state_tx.clone();

    let client = Client::new(
        info_watcher,
        state_rx,
        sender,
        close_sender,
        closed_receiver,
        options.subscription_capacity,
        options.inbox_prefix,
        options.request_timeout,
        max_payload,
        statistics,
        options.skip_subject_validation,
    );

    task::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            tracing::info!("event: {}", event);
            if let Some(event_callback) = &options.event_callback {
                event_callback.call(event).await;
            }
        }
    });

    task::spawn(async move {
        // The runner owns all transport work, including initial and later recovery.
        // Dropping it releases the handler, socket, connector, and command queue.
        {
            let runner = async move {
                if connection.is_none() && options.retry_on_initial_connect {
                    let (info, connection_ok) = match connector.connect().await {
                        Ok((info, connection)) => (info, connection),
                        Err(err) => {
                            error!("connection closed: {}", err);
                            return;
                        }
                    };
                    connector.clear_initial_connect_deadline();
                    info_sender.send(Some(info)).ok();
                    connection = Some(connection_ok);
                }
                let connection = connection.unwrap();
                let mut connection_handler =
                    ConnectionHandler::new(connection, connector, info_sender, ping_period);
                connection_handler.process(&mut receiver).await
            };
            tokio::select! {
                biased;
                _ = close_receiver.wait_for(|close| *close) => {}
                _ = runner => {}
            }
        }
        closed_state.send_replace(State::Disconnected);
        closed_events.try_send(Event::Closed).ok();
        closed_sender.send_replace(true);
    });

    Ok(client)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connected,
    Disconnected,
    LameDuckMode,
    Draining,
    Closed,
    SlowConsumer(u64),
    ServerError(ServerError),
    ClientError(ClientError),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Connected => write!(f, "connected"),
            Event::Disconnected => write!(f, "disconnected"),
            Event::LameDuckMode => write!(f, "lame duck mode detected"),
            Event::Draining => write!(f, "draining"),
            Event::Closed => write!(f, "closed"),
            Event::SlowConsumer(sid) => write!(f, "slow consumers for subscription {sid}"),
            Event::ServerError(err) => write!(f, "server error: {err}"),
            Event::ClientError(err) => write!(f, "client error: {err}"),
        }
    }
}

/// Connects to NATS with default config.
///
/// Returns cloneable [Client].
///
/// To have customized NATS connection, check [ConnectOptions].
///
/// # Examples
///
/// ## Single URL
/// ```
/// # #[tokio::main]
/// # async fn main() ->  Result<(), async_nats::Error> {
/// let mut nc = async_nats::connect("demo.nats.io").await?;
/// nc.publish("test", "data".into()).await?;
/// # Ok(())
/// # }
/// ```
///
/// ## Connect with [Vec] of [ServerAddr].
/// ```no_run
/// #[tokio::main]
/// # async fn main() -> Result<(), async_nats::Error> {
/// use async_nats::ServerAddr;
/// let client = async_nats::connect(vec![
///     "demo.nats.io".parse::<ServerAddr>()?,
///     "other.nats.io".parse::<ServerAddr>()?,
/// ])
/// .await
/// .unwrap();
/// # Ok(())
/// # }
/// ```
///
/// ## with [Vec], but parse URLs inside [crate::connect()]
/// ```no_run
/// #[tokio::main]
/// # async fn main() -> Result<(), async_nats::Error> {
/// use async_nats::ServerAddr;
/// let servers = vec!["demo.nats.io", "other.nats.io"];
/// let client = async_nats::connect(
///     servers
///         .iter()
///         .map(|url| url.parse())
///         .collect::<Result<Vec<ServerAddr>, _>>()?,
/// )
/// .await?;
/// # Ok(())
/// # }
/// ```
///
///
/// ## with slice.
/// ```no_run
/// #[tokio::main]
/// # async fn main() -> Result<(), async_nats::Error> {
/// use async_nats::ServerAddr;
/// let client = async_nats::connect(
///    [
///        "demo.nats.io".parse::<ServerAddr>()?,
///        "other.nats.io".parse::<ServerAddr>()?,
///    ]
///    .as_slice(),
/// )
/// .await?;
/// # Ok(())
/// # }
pub async fn connect<A: ToServerAddrs>(addrs: A) -> Result<Client, ConnectError> {
    connect_with_options(addrs, ConnectOptions::default()).await
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConnectErrorKind {
    /// Parsing the passed server address failed.
    ServerParse,
    /// DNS related issues.
    Dns,
    /// Failed authentication process, signing nonce, etc.
    Authentication,
    /// Server returned authorization violation error.
    AuthorizationViolation,
    /// Connect timed out.
    TimedOut,
    /// Erroneous TLS setup.
    Tls,
    /// Other IO error.
    Io,
    /// Reached the maximum number of reconnects.
    MaxReconnects,
}

impl Display for ConnectErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerParse => write!(f, "failed to parse server or server list"),
            Self::Dns => write!(f, "DNS error"),
            Self::Authentication => write!(f, "failed signing nonce"),
            Self::AuthorizationViolation => write!(f, "authorization violation"),
            Self::TimedOut => write!(f, "timed out"),
            Self::Tls => write!(f, "TLS error"),
            Self::Io => write!(f, "IO error"),
            Self::MaxReconnects => write!(f, "reached maximum number of reconnects"),
        }
    }
}

/// Returned when initial connection fails.
/// To be enumerate over the variants, call [ConnectError::kind].
pub type ConnectError = error::Error<ConnectErrorKind>;

impl From<io::Error> for ConnectError {
    fn from(err: io::Error) -> Self {
        ConnectError::with_source(ConnectErrorKind::Io, err)
    }
}

/// Retrieves messages from given `subscription` created by [Client::subscribe].
///
/// Implements [futures_util::stream::Stream] for ergonomic async message processing.
///
/// # Examples
/// ```
/// # #[tokio::main]
/// # async fn main() ->  Result<(), async_nats::Error> {
/// let mut nc = async_nats::connect("demo.nats.io").await?;
/// # nc.publish("test", "data".into()).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct Subscriber {
    sid: u64,
    receiver: mpsc::Receiver<Message>,
    sender: mpsc::Sender<Command>,
    _close_sender: tokio::sync::watch::Sender<bool>,
}

impl Subscriber {
    fn new(
        sid: u64,
        sender: mpsc::Sender<Command>,
        close_sender: tokio::sync::watch::Sender<bool>,
        receiver: mpsc::Receiver<Message>,
    ) -> Subscriber {
        Subscriber {
            sid,
            sender,
            receiver,
            _close_sender: close_sender,
        }
    }

    /// Unsubscribes from subscription, draining all remaining messages.
    ///
    /// # Examples
    /// ```
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), async_nats::Error> {
    /// let client = async_nats::connect("demo.nats.io").await?;
    ///
    /// let mut subscriber = client.subscribe("foo").await?;
    ///
    /// subscriber.unsubscribe().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn unsubscribe(&mut self) -> Result<(), UnsubscribeError> {
        self.sender
            .send(Command::Unsubscribe {
                sid: self.sid,
                max: None,
            })
            .await?;
        self.receiver.close();
        Ok(())
    }

    /// Unsubscribes from subscription after reaching given number of messages.
    /// This is the total number of messages received by this subscription in it's whole
    /// lifespan. If it already reached or surpassed the passed value, it will immediately stop.
    ///
    /// # Examples
    /// ```
    /// # use futures_util::StreamExt;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), async_nats::Error> {
    /// let client = async_nats::connect("demo.nats.io").await?;
    ///
    /// let mut subscriber = client.subscribe("test").await?;
    /// subscriber.unsubscribe_after(3).await?;
    ///
    /// for _ in 0..3 {
    ///     client.publish("test", "data".into()).await?;
    /// }
    ///
    /// while let Some(message) = subscriber.next().await {
    ///     println!("message received: {:?}", message);
    /// }
    /// println!("no more messages, unsubscribed");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn unsubscribe_after(&mut self, unsub_after: u64) -> Result<(), UnsubscribeError> {
        self.sender
            .send(Command::Unsubscribe {
                sid: self.sid,
                max: Some(unsub_after),
            })
            .await?;
        Ok(())
    }

    /// Unsubscribes immediately but leaves the subscription open to allow any in-flight messages
    /// on the subscription to be delivered. The stream will be closed after any remaining messages
    /// are delivered
    ///
    /// # Examples
    /// ```no_run
    /// # use futures_util::StreamExt;
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), async_nats::Error> {
    /// let client = async_nats::connect("demo.nats.io").await?;
    ///
    /// let mut subscriber = client.subscribe("test").await?;
    ///
    /// tokio::spawn({
    ///     let task_client = client.clone();
    ///     async move {
    ///         loop {
    ///             _ = task_client.publish("test", "data".into()).await;
    ///         }
    ///     }
    /// });
    ///
    /// client.flush().await?;
    /// subscriber.drain().await?;
    ///
    /// while let Some(message) = subscriber.next().await {
    ///     println!("message received: {:?}", message);
    /// }
    /// println!("no more messages, unsubscribed");
    /// # Ok(())
    /// # }
    /// ```
    pub async fn drain(&mut self) -> Result<(), UnsubscribeError> {
        self.sender
            .send(Command::Drain {
                sid: Some(self.sid),
            })
            .await?;

        Ok(())
    }
}

#[derive(Error, Debug, PartialEq)]
#[error("failed to send unsubscribe")]
pub struct UnsubscribeError(String);

impl From<tokio::sync::mpsc::error::SendError<Command>> for UnsubscribeError {
    fn from(err: tokio::sync::mpsc::error::SendError<Command>) -> Self {
        UnsubscribeError(err.to_string())
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.receiver.close();
        // Unsubscribe cleanup must not keep the runner alive after its last
        // application-owned handle disappears.
        tokio::spawn({
            let sender = self.sender.clone();
            let sid = self.sid;
            async move {
                sender
                    .send(Command::Unsubscribe { sid, max: None })
                    .await
                    .ok();
            }
        });
    }
}

impl Stream for Subscriber {
    type Item = Message;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallbackError {
    Client(ClientError),
    Server(ServerError),
}
impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Client(error) => write!(f, "{error}"),
            Self::Server(error) => write!(f, "{error}"),
        }
    }
}

impl From<ServerError> for CallbackError {
    fn from(server_error: ServerError) -> Self {
        CallbackError::Server(server_error)
    }
}

impl From<ClientError> for CallbackError {
    fn from(client_error: ClientError) -> Self {
        CallbackError::Client(client_error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ServerError {
    AuthorizationViolation,
    SlowConsumer(u64),
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    Other(String),
    MaxReconnects,
    /// The reconnect-to-server callback returned a server address that is not
    /// present in the current server pool.
    ServerNotInPool,
}
impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Other(error) => write!(f, "nats: {error}"),
            Self::MaxReconnects => write!(f, "nats: max reconnects reached"),
            Self::ServerNotInPool => {
                write!(f, "nats: reconnect callback returned server not in pool")
            }
        }
    }
}

impl ServerError {
    fn new(error: String) -> ServerError {
        match error.to_lowercase().as_str() {
            "authorization violation" => ServerError::AuthorizationViolation,
            // error messages can contain case-sensitive values which should be preserved
            _ => ServerError::Other(error),
        }
    }
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AuthorizationViolation => write!(f, "nats: authorization violation"),
            Self::SlowConsumer(sid) => write!(f, "nats: subscription {sid} is a slow consumer"),
            Self::Other(error) => write!(f, "nats: {error}"),
        }
    }
}

/// Info to construct a CONNECT message.
#[derive(Clone, Debug, Serialize)]
pub struct ConnectInfo {
    /// Turns on +OK protocol acknowledgments.
    pub verbose: bool,

    /// Turns on additional strict format checking, e.g. for properly formed
    /// subjects.
    pub pedantic: bool,

    /// User's JWT.
    #[serde(rename = "jwt")]
    pub user_jwt: Option<String>,

    /// Public nkey.
    pub nkey: Option<String>,

    /// Signed nonce, encoded to Base64URL.
    #[serde(rename = "sig")]
    pub signature: Option<String>,

    /// Optional client name.
    pub name: Option<String>,

    /// If set to `true`, the server (version 1.2.0+) will not send originating
    /// messages from this connection to its own subscriptions. Clients should
    /// set this to `true` only for server supporting this feature, which is
    /// when proto in the INFO protocol is set to at least 1.
    pub echo: bool,

    /// The implementation language of the client.
    pub lang: String,

    /// The version of the client.
    pub version: String,

    /// Sending 0 (or absent) indicates client supports original protocol.
    /// Sending 1 indicates that the client supports dynamic reconfiguration
    /// of cluster topology changes by asynchronously receiving INFO messages
    /// with known servers it can reconnect to.
    pub protocol: Protocol,

    /// Indicates whether the client requires an SSL connection.
    pub tls_required: bool,

    /// Connection username (if `auth_required` is set)
    pub user: Option<String>,

    /// Connection password (if auth_required is set)
    pub pass: Option<String>,

    /// Client authorization token (if auth_required is set)
    pub auth_token: Option<String>,

    /// Whether the client supports the usage of headers.
    pub headers: bool,

    /// Whether the client supports no_responders.
    pub no_responders: bool,
}

/// Protocol version used by the client.
#[derive(Serialize_repr, Deserialize_repr, PartialEq, Eq, Debug, Clone, Copy)]
#[repr(u8)]
pub enum Protocol {
    /// Original protocol.
    Original = 0,
    /// Protocol with dynamic reconfiguration of cluster and lame duck mode functionality.
    Dynamic = 1,
}

/// Address of a NATS server.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerAddr(Url);

impl FromStr for ServerAddr {
    type Err = io::Error;

    /// Parse an address of a NATS server.
    ///
    /// If not stated explicitly the `nats://` schema and port `4222` is assumed.
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let url: Url = if input.contains("://") {
            input.parse()
        } else {
            format!("nats://{input}").parse()
        }
        .map_err(|e| {
            io::Error::new(
                ErrorKind::InvalidInput,
                format!("NATS server URL is invalid: {e}"),
            )
        })?;

        Self::from_url(url)
    }
}

impl ServerAddr {
    /// Check if the URL is a valid NATS server address.
    pub fn from_url(url: Url) -> io::Result<Self> {
        if url.scheme() != "nats"
            && url.scheme() != "tls"
            && url.scheme() != "ws"
            && url.scheme() != "wss"
        {
            return Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("invalid scheme for NATS server URL: {}", url.scheme()),
            ));
        }

        Ok(Self(url))
    }

    /// Turn the server address into a standard URL.
    pub fn into_inner(self) -> Url {
        self.0
    }

    /// Returns if tls is required by the client for this server.
    pub fn tls_required(&self) -> bool {
        self.0.scheme() == "tls"
    }

    /// Returns if the server url had embedded username and password.
    pub fn has_user_pass(&self) -> bool {
        self.0.username() != ""
    }

    pub fn scheme(&self) -> &str {
        self.0.scheme()
    }

    /// Returns the host.
    pub fn host(&self) -> &str {
        match self.0.host() {
            Some(Host::Domain(_)) | Some(Host::Ipv4 { .. }) => self.0.host_str().unwrap(),
            // `host_str()` for Ipv6 includes the []s
            Some(Host::Ipv6 { .. }) => {
                let host = self.0.host_str().unwrap();
                &host[1..host.len() - 1]
            }
            None => "",
        }
    }

    pub fn is_websocket(&self) -> bool {
        self.0.scheme() == "ws" || self.0.scheme() == "wss"
    }

    /// Returns the port.
    /// Delegates to [`Url::port_or_known_default`](https://docs.rs/url/latest/url/struct.Url.html#method.port_or_known_default) and defaults to 4222 if none was explicitly specified in creating this `ServerAddr`.
    pub fn port(&self) -> u16 {
        self.0.port_or_known_default().unwrap_or(4222)
    }

    /// Returns the URL string.
    pub fn as_url_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns the optional username in the url.
    pub fn username(&self) -> Option<&str> {
        let user = self.0.username();
        if user.is_empty() { None } else { Some(user) }
    }

    /// Returns the optional password in the url.
    pub fn password(&self) -> Option<&str> {
        self.0.password()
    }

    /// Return the sockets from resolving the server address.
    pub async fn socket_addrs(&self) -> io::Result<impl Iterator<Item = SocketAddr> + '_> {
        tokio::net::lookup_host((self.host(), self.port())).await
    }
}

/// Capability to convert into a list of NATS server addresses.
///
/// There are several implementations ensuring the easy passing of one or more server addresses to
/// functions like [`crate::connect()`].
pub trait ToServerAddrs {
    /// Returned iterator over socket addresses which this type may correspond
    /// to.
    type Iter: Iterator<Item = ServerAddr>;

    fn to_server_addrs(&self) -> io::Result<Self::Iter>;
}

impl ToServerAddrs for ServerAddr {
    type Iter = option::IntoIter<ServerAddr>;
    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        Ok(Some(self.clone()).into_iter())
    }
}

impl ToServerAddrs for str {
    type Iter = option::IntoIter<ServerAddr>;
    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        self.parse::<ServerAddr>()
            .map(|addr| Some(addr).into_iter())
    }
}

impl ToServerAddrs for String {
    type Iter = option::IntoIter<ServerAddr>;
    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        (**self).to_server_addrs()
    }
}

impl<T: AsRef<str>> ToServerAddrs for [T] {
    type Iter = std::vec::IntoIter<ServerAddr>;
    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        self.iter()
            .map(AsRef::as_ref)
            .map(str::parse)
            .collect::<io::Result<_>>()
            .map(Vec::into_iter)
    }
}

impl<T: AsRef<str>> ToServerAddrs for Vec<T> {
    type Iter = std::vec::IntoIter<ServerAddr>;
    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        self.as_slice().to_server_addrs()
    }
}

impl<'a> ToServerAddrs for &'a [ServerAddr] {
    type Iter = iter::Cloned<slice::Iter<'a, ServerAddr>>;

    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        Ok(self.iter().cloned())
    }
}

impl ToServerAddrs for Vec<ServerAddr> {
    type Iter = std::vec::IntoIter<ServerAddr>;

    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        Ok(self.clone().into_iter())
    }
}

impl<T: ToServerAddrs + ?Sized> ToServerAddrs for &T {
    type Iter = T::Iter;
    fn to_server_addrs(&self) -> io::Result<Self::Iter> {
        (**self).to_server_addrs()
    }
}

/// Checks if a subject contains only protocol-safe characters.
/// Rejects empty subjects and subjects containing whitespace characters
/// (space, tab, CR, LF) which would break protocol framing.
/// Used for publish paths. Matches nats.go `validateSubject`.
pub(crate) fn is_valid_publish_subject<T: AsRef<str>>(subject: T) -> bool {
    let bytes = subject.as_ref().as_bytes();

    if bytes.is_empty() {
        return false;
    }

    memchr::memchr3(b' ', b'\r', b'\n', bytes).is_none() && memchr::memchr(b'\t', bytes).is_none()
}

/// Checks if a subject is structurally valid for subscribing.
/// In addition to protocol-framing checks, also rejects invalid dot structure
/// (leading/trailing dots, consecutive dots). Matches nats.go `badSubject`.
pub(crate) fn is_valid_subject<T: AsRef<str>>(subject: T) -> bool {
    let bytes = subject.as_ref().as_bytes();

    if bytes.is_empty() {
        return false;
    }

    bytes[0] != b'.'
        && bytes[bytes.len() - 1] != b'.'
        && memchr::memmem::find(bytes, b"..").is_none()
        && memchr::memchr3(b' ', b'\r', b'\n', bytes).is_none()
        && memchr::memchr(b'\t', bytes).is_none()
}

/// Checks if a queue group name is valid for the NATS protocol.
/// Queue groups must not be empty and must not contain whitespace characters
/// (space, tab, CR, LF) which would break protocol framing.
pub(crate) fn is_valid_queue_group(queue_group: &str) -> bool {
    let bytes = queue_group.as_bytes();

    if bytes.is_empty() {
        return false;
    }

    memchr::memchr3(b' ', b'\r', b'\n', bytes).is_none() && memchr::memchr(b'\t', bytes).is_none()
}

#[allow(unused_macros)]
macro_rules! from_with_timeout {
    ($t:ty, $k:ty, $origin: ty, $origin_kind: ty) => {
        impl From<$origin> for $t {
            fn from(err: $origin) -> Self {
                match err.kind() {
                    <$origin_kind>::TimedOut => Self::new(<$k>::TimedOut),
                    _ => Self::with_source(<$k>::Other, err),
                }
            }
        }
    };
}
#[allow(unused_imports)]
pub(crate) use from_with_timeout;

use crate::connection::ShouldFlush;
use crate::message::OutboundMessage;

#[cfg(test)]
mod tests {
    use super::*;

    mod transport_resilience {
        use super::*;
        use futures_util::{FutureExt, StreamExt};
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        use tokio::net::{TcpListener, TcpStream};

        const BOUND: Duration = Duration::from_secs(3);
        const INFO: &[u8] = b"INFO {\"server_id\":\"fixture\",\"max_payload\":1048576}\r\n";

        async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
            tokio::time::timeout(BOUND, future)
                .await
                .expect("bounded native transport")
        }

        async fn line(peer: &mut BufReader<TcpStream>) -> String {
            let mut line = String::new();
            assert_ne!(within(peer.read_line(&mut line)).await.unwrap(), 0);
            line
        }

        async fn command(peer: &mut BufReader<TcpStream>) -> String {
            loop {
                let command = line(peer).await;
                if command == "PING\r\n" {
                    within(peer.get_mut().write_all(b"PONG\r\n")).await.unwrap();
                } else {
                    return command;
                }
            }
        }

        async fn handshake(stream: TcpStream) -> BufReader<TcpStream> {
            let mut peer = BufReader::new(stream);
            within(peer.get_mut().write_all(INFO)).await.unwrap();
            assert!(line(&mut peer).await.starts_with("CONNECT "));
            assert_eq!(line(&mut peer).await, "PING\r\n");
            within(peer.get_mut().write_all(b"PONG\r\n")).await.unwrap();
            peer
        }

        async fn connected(options: ConnectOptions) -> (TcpListener, Client, BufReader<TcpStream>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let (client, peer) = within(async {
                tokio::join!(options.connect(addr), async {
                    handshake(listener.accept().await.unwrap().0).await
                })
            })
            .await;
            (listener, client.unwrap(), peer)
        }

        fn events() -> (ConnectOptions, mpsc::UnboundedReceiver<Event>) {
            let (tx, rx) = mpsc::unbounded_channel();
            (
                ConnectOptions::new().event_callback(move |event| {
                    let tx = tx.clone();
                    async move {
                        tx.send(event).ok();
                    }
                }),
                rx,
            )
        }

        async fn closed_events(mut events: mpsc::UnboundedReceiver<Event>) -> usize {
            within(async move {
                let mut closed = 0;
                while let Some(event) = events.recv().await {
                    closed += usize::from(event == Event::Closed);
                }
                closed
            })
            .await
        }

        // Occupy the runtime's only blocking thread so the real system resolver
        // cannot finish. No replacement DNS implementation is involved.
        async fn block_resolver() -> (std::sync::mpsc::Sender<()>, task::JoinHandle<()>) {
            let (release, held) = std::sync::mpsc::channel();
            let (started, ready) = tokio::sync::oneshot::channel();
            let blocker = task::spawn_blocking(move || {
                started.send(()).unwrap();
                let _ = held.recv_timeout(BOUND * 2);
            });
            within(ready).await.unwrap();
            (release, blocker)
        }

        #[test]
        fn pending_system_dns_spends_the_server_attempt_budget() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(1)
                .build()
                .unwrap();
            runtime.block_on(async {
                let (release, blocker) = block_resolver().await;
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    ConnectOptions::new()
                        .connection_timeout(Duration::from_millis(100))
                        .connect("localhost:4222"),
                )
                .await;
                release.send(()).unwrap();
                within(blocker).await.unwrap();
                assert_eq!(
                    result.expect("native DNS deadline").unwrap_err().kind(),
                    ConnectErrorKind::TimedOut
                );
            });
        }

        #[tokio::test]
        async fn stalled_first_address_leaves_time_for_the_next_address() {
            let addrs: Vec<_> = within(tokio::net::lookup_host(("localhost", 0)))
                .await
                .unwrap()
                .collect();
            let first_ip = addrs[0].ip();
            let next_ip = addrs
                .iter()
                .find(|addr| addr.ip() != first_ip)
                .expect("localhost fixture must resolve both loopback families")
                .ip();
            let first = TcpListener::bind((first_ip, 0)).await.unwrap();
            let port = first.local_addr().unwrap().port();
            let next = TcpListener::bind((next_ip, port)).await.unwrap();
            let stalled = task::spawn(async move {
                let (mut socket, _) = within(first.accept()).await.unwrap();
                let mut bytes = Vec::new();
                within(socket.read_to_end(&mut bytes)).await.unwrap();
                assert!(bytes.is_empty(), "no CONNECT before INFO");
            });
            let healthy = task::spawn(async move {
                let mut peer = handshake(within(next.accept()).await.unwrap().0).await;
                let mut bytes = Vec::new();
                within(peer.read_to_end(&mut bytes)).await.unwrap();
            });
            let client = tokio::time::timeout(
                Duration::from_millis(500),
                ConnectOptions::new()
                    .connection_timeout(Duration::from_millis(600))
                    .connect(format!("localhost:{port}")),
            )
            .await
            .expect("later candidate must start before the entire server budget is spent")
            .unwrap();
            within(stalled).await.unwrap();
            client.force_close();
            assert!(within(client.wait_closed()).await);
            within(healthy).await.unwrap();
        }

        #[tokio::test]
        async fn pending_tls_handshake_spends_the_server_attempt_budget() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = format!("tls://{}", listener.local_addr().unwrap());
            let peer = task::spawn(async move {
                let (mut socket, _) = within(listener.accept()).await.unwrap();
                let mut header = [0; 5];
                within(socket.read_exact(&mut header)).await.unwrap();
                assert_eq!(header[0], 22, "client reached the TLS handshake");
                let mut remainder = Vec::new();
                within(socket.read_to_end(&mut remainder)).await.unwrap();
            });
            let error = within(
                ConnectOptions::new()
                    .tls_first()
                    .add_root_certificates(
                        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                            .join("tests/configs/certs/rootCA.pem"),
                    )
                    .connection_timeout(Duration::from_millis(200))
                    .connect(addr),
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), ConnectErrorKind::TimedOut);
            within(peer).await.unwrap();
        }

        #[test]
        fn same_subscriber_recovers_after_pending_system_dns() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .max_blocking_threads(1)
                .build()
                .unwrap();
            runtime.block_on(async {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = format!("localhost:{}", listener.local_addr().unwrap().port());
                let (options, mut events) = events();
                let (client, mut peer) = within(async {
                    tokio::join!(
                        options
                            .connection_timeout(Duration::from_millis(100))
                            .connect(addr),
                        async { handshake(listener.accept().await.unwrap().0).await }
                    )
                })
                .await;
                let client = client.unwrap();
                let mut subscriber = client.subscribe("retained").await.unwrap();
                let original_sub = command(&mut peer).await;
                assert!(original_sub.starts_with("SUB retained "));
                let (release, blocker) = block_resolver().await;
                drop(peer);
                let timed_out = tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        if let Event::ClientError(ClientError::Other(error)) =
                            events.recv().await.expect("native runner events")
                        {
                            if error == "timed out" {
                                break;
                            }
                        }
                    }
                })
                .await;
                release.send(()).unwrap();
                within(blocker).await.unwrap();
                timed_out.expect("reconnect must finish its pending DNS attempt");
                let mut peer = handshake(within(listener.accept()).await.unwrap().0).await;
                assert_eq!(command(&mut peer).await, original_sub);
                let sid = original_sub.split_whitespace().last().unwrap();
                let message = format!("MSG retained {sid} 2\r\nok\r\n");
                within(peer.get_mut().write_all(message.as_bytes()))
                    .await
                    .unwrap();
                assert_eq!(
                    within(subscriber.next()).await.unwrap().payload.as_ref(),
                    b"ok"
                );
                client.force_close();
                assert!(within(client.wait_closed()).await);
                assert!(within(subscriber.next()).await.is_none());
                assert_eq!(closed_events(events).await, 1);
            });
        }

        #[tokio::test]
        async fn forced_close_finishes_recovery_and_drops_queued_work_before_receipt() {
            let (options, events) = events();
            let (listener, client, mut peer) = connected(options).await;
            let mut subscriber = client.subscribe("retained").await.unwrap();
            assert!(command(&mut peer).await.starts_with("SUB retained "));
            drop(peer);
            let (mut pending, _) = within(listener.accept()).await.unwrap();
            let request = client.request("waiting", "body".into());
            tokio::pin!(request);
            assert!((&mut request).now_or_never().is_none());
            client.force_close();
            client.force_close();
            assert!(
                client.wait_closed().now_or_never().is_none(),
                "a close request is not observed completion"
            );
            assert!(within(client.wait_closed()).await);
            assert_eq!(client.connection_state(), State::Disconnected);
            assert!(within(request).await.is_err());
            assert!(within(subscriber.next()).await.is_none());
            assert!(client.flush().await.is_err());
            let mut byte = [0];
            assert_eq!(within(pending.read(&mut byte)).await.unwrap(), 0);
            assert_eq!(closed_events(events).await, 1);
        }

        #[tokio::test]
        async fn last_subscriber_drop_closes_full_queue_during_unavailable_reconnect() {
            let (options, events) = events();
            let (listener, client, mut peer) = connected(
                options
                    .client_capacity(1)
                    .connection_timeout(Duration::from_secs(60)),
            )
            .await;
            let subscriber = client.subscribe("retained").await.unwrap();
            assert!(command(&mut peer).await.starts_with("SUB retained "));
            let queued_senders = subscriber.sender.downgrade();
            drop(peer);
            // Native reconnect has an established TCP socket, but the fixture
            // withholds INFO. The command receiver cannot drain during this wait.
            let (mut unavailable, _) = within(listener.accept()).await.unwrap();
            client
                .publish("queued", Bytes::from_static(b"unconfirmed"))
                .await
                .unwrap();
            assert_eq!(subscriber.sender.capacity(), 0, "the native queue is full");
            drop(client);
            drop(subscriber);
            assert_eq!(closed_events(events).await, 1);
            let mut byte = [0];
            assert_eq!(within(unavailable.read(&mut byte)).await.unwrap(), 0);
            within(async {
                while queued_senders.upgrade().is_some() {
                    tokio::task::yield_now().await;
                }
            })
            .await;
        }

        #[tokio::test]
        async fn raw_subscriber_retains_runner_after_last_client_is_dropped() {
            let (options, events) = events();
            let (_, client, mut peer) = connected(options).await;
            let mut subscriber = client.subscribe("retained").await.unwrap();
            let sub = command(&mut peer).await;
            let sid = sub.split_whitespace().last().unwrap();
            drop(client);
            let message = format!("MSG retained {sid} 2\r\nok\r\n");
            within(peer.get_mut().write_all(message.as_bytes()))
                .await
                .unwrap();
            assert_eq!(
                within(subscriber.next()).await.unwrap().payload.as_ref(),
                b"ok"
            );
            subscriber.unsubscribe().await.unwrap();
            assert_eq!(command(&mut peer).await, format!("UNSUB {sid}\r\n"));
            drop(subscriber);
            let mut remaining = Vec::new();
            // Only after proving subscriber ownership and dropping that final owner,
            // accept either EOF or the TCP reset Linux can report on socket close.
            if let Err(error) = within(peer.read_to_end(&mut remaining)).await {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            }
            assert_eq!(closed_events(events).await, 1);
        }

        #[tokio::test]
        async fn last_owner_drop_terminates_background_initial_recovery() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (options, events) = events();
            let client = options
                .retry_on_initial_connect()
                .connect(listener.local_addr().unwrap().to_string())
                .await
                .unwrap();
            let (mut pending, _) = within(listener.accept()).await.unwrap();
            drop(client);
            let mut byte = [0];
            assert_eq!(within(pending.read(&mut byte)).await.unwrap(), 0);
            assert_eq!(closed_events(events).await, 1);
        }

        #[tokio::test]
        async fn initial_deadline_is_cleared_before_background_reconnect() {
            let end = tokio::time::Instant::now() + Duration::from_secs(1);
            let (listener, client, mut peer) =
                connected(ConnectOptions::new().initial_connect_deadline(end)).await;
            let mut subscriber = client.subscribe("retained").await.unwrap();
            let original = command(&mut peer).await;
            assert!(original.starts_with("SUB retained "));
            tokio::time::sleep_until(end + Duration::from_millis(20)).await;
            drop(peer);
            let mut replacement = handshake(within(listener.accept()).await.unwrap().0).await;
            assert_eq!(command(&mut replacement).await, original);
            let sid = original.split_whitespace().last().unwrap();
            within(
                replacement
                    .get_mut()
                    .write_all(format!("MSG retained {sid} 2\r\nok\r\n").as_bytes()),
            )
            .await
            .unwrap();
            assert_eq!(
                within(subscriber.next()).await.unwrap().payload.as_ref(),
                b"ok"
            );
            client.force_close();
            assert!(within(client.wait_closed()).await);
            assert!(within(subscriber.next()).await.is_none());
        }

        #[tokio::test]
        async fn initial_deadline_terminates_background_initial_retry() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (options, events) = events();
            let end = tokio::time::Instant::now() + Duration::from_secs(1);
            let client = options
                .retry_on_initial_connect()
                .connection_timeout(Duration::from_secs(5))
                .initial_connect_deadline(end)
                .connect(listener.local_addr().unwrap().to_string())
                .await
                .unwrap();
            // The first TCP connection is real but the peer never supplies INFO.
            let (mut pending, _) = within(listener.accept()).await.unwrap();
            assert!(within(client.wait_closed()).await);
            assert!(tokio::time::Instant::now() >= end);
            assert!(client.flush().await.is_err());
            let mut byte = [0];
            assert_eq!(within(pending.read(&mut byte)).await.unwrap(), 0);
            assert_eq!(closed_events(events).await, 1);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "expired initial work must not restart another connection attempt"
            );
        }

        #[tokio::test]
        async fn graceful_close_reports_completion_and_one_closed_event() {
            let (options, events) = events();
            let (_, client, mut peer) = connected(options).await;
            client.drain().await.unwrap();
            let mut remaining = Vec::new();
            within(peer.read_to_end(&mut remaining)).await.unwrap();
            assert!(within(client.wait_closed()).await);
            assert_eq!(closed_events(events).await, 1);
        }

        #[tokio::test]
        async fn lost_runner_does_not_report_observed_completion() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let client = ConnectOptions::with_auth_callback(|_| async {
                panic!("fixture terminates runner before its completion receipt")
            })
            .retry_on_initial_connect()
            .connect(listener.local_addr().unwrap().to_string())
            .await
            .unwrap();
            let (mut peer, _) = within(listener.accept()).await.unwrap();
            within(peer.write_all(INFO)).await.unwrap();
            assert!(!within(client.wait_closed()).await);
            let mut byte = [0];
            assert_eq!(within(peer.read(&mut byte)).await.unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn initial_connect_deadline_refuses_new_attempts_after_expiry() {
        let result = ConnectOptions::new()
            .initial_connect_deadline(tokio::time::Instant::now())
            .connect("nats://127.0.0.1:4222")
            .await;
        assert!(matches!(result, Err(error) if error.kind() == ConnectErrorKind::TimedOut));
    }

    #[test]
    fn server_address_ipv6() {
        let address = ServerAddr::from_str("nats://[::]").unwrap();
        assert_eq!(address.host(), "::")
    }

    #[test]
    fn server_address_ipv4() {
        let address = ServerAddr::from_str("nats://127.0.0.1").unwrap();
        assert_eq!(address.host(), "127.0.0.1")
    }

    #[test]
    fn server_address_domain() {
        let address = ServerAddr::from_str("nats://example.com").unwrap();
        assert_eq!(address.host(), "example.com")
    }

    #[test]
    fn to_server_addrs_vec_str() {
        let vec = vec!["nats://127.0.0.1", "nats://[::]"];
        let mut addrs_iter = vec.to_server_addrs().unwrap();
        assert_eq!(addrs_iter.next().unwrap().host(), "127.0.0.1");
        assert_eq!(addrs_iter.next().unwrap().host(), "::");
        assert_eq!(addrs_iter.next(), None);
    }

    #[test]
    fn to_server_addrs_arr_str() {
        let arr = ["nats://127.0.0.1", "nats://[::]"];
        let mut addrs_iter = arr.to_server_addrs().unwrap();
        assert_eq!(addrs_iter.next().unwrap().host(), "127.0.0.1");
        assert_eq!(addrs_iter.next().unwrap().host(), "::");
        assert_eq!(addrs_iter.next(), None);
    }

    #[test]
    fn to_server_addrs_vec_string() {
        let vec = vec!["nats://127.0.0.1".to_string(), "nats://[::]".to_string()];
        let mut addrs_iter = vec.to_server_addrs().unwrap();
        assert_eq!(addrs_iter.next().unwrap().host(), "127.0.0.1");
        assert_eq!(addrs_iter.next().unwrap().host(), "::");
        assert_eq!(addrs_iter.next(), None);
    }

    #[test]
    fn to_server_addrs_arr_string() {
        let arr = ["nats://127.0.0.1".to_string(), "nats://[::]".to_string()];
        let mut addrs_iter = arr.to_server_addrs().unwrap();
        assert_eq!(addrs_iter.next().unwrap().host(), "127.0.0.1");
        assert_eq!(addrs_iter.next().unwrap().host(), "::");
        assert_eq!(addrs_iter.next(), None);
    }

    #[test]
    fn to_server_ports_arr_string() {
        for (arr, expected_port) in [
            (
                [
                    "nats://127.0.0.1".to_string(),
                    "nats://[::]".to_string(),
                    "tls://127.0.0.1".to_string(),
                    "tls://[::]".to_string(),
                ],
                4222,
            ),
            (
                [
                    "ws://127.0.0.1:80".to_string(),
                    "ws://[::]:80".to_string(),
                    "ws://127.0.0.1".to_string(),
                    "ws://[::]".to_string(),
                ],
                80,
            ),
            (
                [
                    "wss://127.0.0.1".to_string(),
                    "wss://[::]".to_string(),
                    "wss://127.0.0.1:443".to_string(),
                    "wss://[::]:443".to_string(),
                ],
                443,
            ),
        ] {
            let mut addrs_iter = arr.to_server_addrs().unwrap();
            assert_eq!(addrs_iter.next().unwrap().port(), expected_port);
        }
    }
    fn multiplexer() -> Multiplexer {
        Multiplexer {
            subject: Subject::from_static("_INBOX.mux.*"),
            prefix: Subject::from_static("_INBOX.mux."),
            senders: HashMap::new(),
            prune_at: MULTIPLEXER_PRUNE_MIN,
        }
    }

    fn reply() -> Message {
        Message {
            subject: Subject::from_static("_INBOX.mux.reply"),
            reply: None,
            payload: Bytes::from_static(b"reply"),
            headers: None,
            status: None,
            description: None,
            length: 5,
        }
    }

    #[test]
    fn multiplexer_prunes_abandoned_requests() {
        let mut multiplexer = multiplexer();

        // Dropping the receiver is what a request does when its caller stops waiting.
        for i in 0..10_000 {
            let (sender, receiver) = oneshot::channel();
            drop(receiver);
            multiplexer.insert(format!("abandoned{i}"), sender);
        }

        assert!(multiplexer.senders.len() <= MULTIPLEXER_PRUNE_MIN);
    }

    #[test]
    fn multiplexer_keeps_pending_requests() {
        let mut multiplexer = multiplexer();
        let mut pending = Vec::new();

        for i in 0..1_000 {
            let (sender, receiver) = oneshot::channel();
            multiplexer.insert(format!("pending{i}"), sender);
            pending.push((format!("pending{i}"), receiver));

            for j in 0..10 {
                let (sender, receiver) = oneshot::channel();
                drop(receiver);
                multiplexer.insert(format!("abandoned{i}.{j}"), sender);
            }
        }

        // Pruning keeps the map within twice the number of pending requests.
        assert!(multiplexer.senders.len() <= 2_000);

        for (token, mut receiver) in pending {
            let sender = multiplexer
                .remove(&token)
                .expect("pending request was pruned");
            sender.send(reply()).unwrap();
            assert_eq!(receiver.try_recv().unwrap().payload, "reply");
        }
    }

    /// Fills the multiplexer with `count` pending requests, then answers all of them.
    fn answer_burst(multiplexer: &mut Multiplexer, count: usize) {
        let mut pending = Vec::new();
        for i in 0..count {
            let (sender, receiver) = oneshot::channel();
            multiplexer.insert(format!("burst{i}"), sender);
            pending.push((format!("burst{i}"), receiver));
        }
        for (token, mut receiver) in pending {
            let sender = multiplexer
                .remove(&token)
                .expect("pending request was pruned");
            sender.send(reply()).unwrap();
            assert!(receiver.try_recv().is_ok());
        }
    }

    #[test]
    fn multiplexer_prunes_abandoned_requests_after_burst() {
        let mut multiplexer = multiplexer();
        answer_burst(&mut multiplexer, 10_000);

        for i in 0..10_000 {
            let (sender, receiver) = oneshot::channel();
            drop(receiver);
            multiplexer.insert(format!("abandoned{i}"), sender);
        }

        assert!(multiplexer.senders.len() <= MULTIPLEXER_PRUNE_MIN);
    }

    #[test]
    fn multiplexer_releases_capacity_after_burst() {
        let mut multiplexer = multiplexer();
        answer_burst(&mut multiplexer, 100_000);

        for i in 0..1_000 {
            let (sender, receiver) = oneshot::channel();
            drop(receiver);
            multiplexer.insert(format!("abandoned{i}"), sender);
        }

        assert!(multiplexer.senders.capacity() <= 4 * MULTIPLEXER_PRUNE_MIN);
    }
}
