//! A one-shot lost-acknowledgement proxy on the `PostgreSQL` wire protocol.
//!
//! It sits between one pool and the server and relays both directions. It
//! frames the frontend's messages, the untyped startup message first and then
//! a type byte and a big-endian `i32` length that counts itself, and it frames
//! the backend's only once it has fired. Armed, it acts once on the first
//! simple-query `COMMIT` of any connection, which is how `sqlx-postgres`
//! 0.9.0 commits:
//!
//! - [`Fault::ForwardThenDrop`] forwards it, waits for the server's
//!   `ReadyForQuery`, and closes both sockets without relaying the answer: a
//!   real commit whose acknowledgement is lost.
//! - [`Fault::DropBeforeForward`] closes both sockets before forwarding it:
//!   nothing commits.
//!
//! It fires only once, so a later connection, such as a readback's, passes
//! through. It frames the plaintext protocol only, so the pool's DSN uses
//! `sslmode=disable`.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The payload of the simple-query message that commits: the text and its
/// NUL terminator.
const COMMIT: &[u8] = b"COMMIT\0";

/// Bound on joining the proxy's tasks.
const JOIN_BUDGET: Duration = Duration::from_secs(5);

/// What an armed proxy does with the first `COMMIT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Forward it, wait for `ReadyForQuery`, then close both sockets: the
    /// commit happens and its acknowledgement is lost.
    ForwardThenDrop,
    /// Close both sockets before forwarding it: nothing commits.
    DropBeforeForward,
}

/// The one-shot fault, shared by every relayed connection.
#[derive(Clone, Copy, Debug)]
enum Arming {
    Idle,
    Armed(Fault),
    Fired(Fault),
}

/// The proxy. Its listener and relays run on its own tracker until
/// [`CommitProxy::shutdown`] joins them; dropping it stops them.
#[derive(Debug)]
pub(crate) struct CommitProxy {
    address: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    cancel: CancellationToken,
    tasks: TaskTracker,
}

impl CommitProxy {
    /// Listen on an ephemeral loopback port and relay every connection to
    /// `server`.
    pub(crate) async fn start(server: SocketAddr) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the proxy listens");
        let address = listener.local_addr().expect("the proxy's address");
        let arming = Arc::new(Mutex::new(Arming::Idle));
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        tasks.spawn(accept(
            listener,
            server,
            Arc::clone(&arming),
            cancel.clone(),
            tasks.clone(),
        ));
        Self {
            address,
            arming,
            cancel,
            tasks,
        }
    }

    /// Where a pool connects to reach the server through the proxy.
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Act once, with `fault`, on the next `COMMIT` any connection sends.
    pub(crate) fn arm(&self, fault: Fault) {
        *lock(&self.arming) = Arming::Armed(fault);
    }

    /// The fault the proxy acted on, once it has.
    pub(crate) fn fired(&self) -> Option<Fault> {
        match *lock(&self.arming) {
            Arming::Fired(fault) => Some(fault),
            Arming::Idle | Arming::Armed(_) => None,
        }
    }

    /// Stop listening and relaying, and join every task within a bound.
    pub(crate) async fn shutdown(self) {
        self.cancel.cancel();
        self.tasks.close();
        tokio::time::timeout(JOIN_BUDGET, self.tasks.wait())
            .await
            .expect("the commit proxy's tasks join");
    }
}

impl Drop for CommitProxy {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Accept connections until cancelled; each gets its own relay.
async fn accept(
    listener: TcpListener,
    server: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    cancel: CancellationToken,
    tasks: TaskTracker,
) {
    loop {
        let client = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((client, _)) => client,
                Err(_) => return,
            },
        };
        tasks.spawn(relay(client, server, Arc::clone(&arming), cancel.clone()));
    }
}

/// Relay one connection until either side closes, the fault fires, or the
/// proxy stops. Returning drops, and so closes, both sockets.
async fn relay(
    mut client: TcpStream,
    server: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    cancel: CancellationToken,
) {
    let Ok(mut upstream) = TcpStream::connect(server).await else {
        return;
    };
    // Small messages both ways: Nagle's delay would only slow the suite.
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let mut frontend = Frontend::default();
    let mut backend = Vec::new();
    // Set once `COMMIT` is forwarded under `ForwardThenDrop`: from then on the
    // client is not read, and the server's answer is framed and swallowed.
    let mut committing = false;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            read = client.read_buf(&mut frontend.pending), if !committing => {
                if !matches!(read, Ok(1..)) {
                    return;
                }
                while let Some(message) = frontend.take_message() {
                    let Ok(message) = message else {
                        return;
                    };
                    match commit_fault(&message, &arming) {
                        Some(Fault::DropBeforeForward) => return,
                        Some(Fault::ForwardThenDrop) => committing = true,
                        None => {}
                    }
                    if upstream.write_all(&message).await.is_err() {
                        return;
                    }
                    if committing {
                        break;
                    }
                }
            }
            read = upstream.read_buf(&mut backend) => {
                if !matches!(read, Ok(1..)) {
                    return;
                }
                if committing {
                    // The answer to `COMMIT` is never relayed: close once
                    // the server is ready again, which it is only after the
                    // commit.
                    if !matches!(holds_ready_for_query(&backend), Ok(false)) {
                        return;
                    }
                } else if client.write_all(&backend).await.is_err() {
                    return;
                } else {
                    backend.clear();
                }
            }
        }
    }
}

/// The client's bytes not yet forwarded, framed into whole messages.
#[derive(Debug, Default)]
struct Frontend {
    pending: Vec<u8>,
    /// The untyped startup message has passed; every later one is typed.
    started: bool,
}

impl Frontend {
    /// The next complete message, taken off the pending bytes.
    fn take_message(&mut self) -> Option<Result<Vec<u8>, Malformed>> {
        let framed = frame(&self.pending, self.started)?;
        Some(framed.map(|length| {
            self.started = true;
            self.pending.drain(..length).collect()
        }))
    }
}

/// A length field that cannot frame a message; the relay closes.
#[derive(Debug)]
struct Malformed;

/// The length of the complete message at the start of `bytes`: a type byte
/// and a big-endian `i32` length that counts itself, or the length alone for
/// the untyped startup message. `None` while the message is incomplete.
fn frame(bytes: &[u8], typed: bool) -> Option<Result<usize, Malformed>> {
    let start = usize::from(typed);
    let field: [u8; 4] = bytes.get(start..start + 4)?.try_into().ok()?;
    let Some(length) = usize::try_from(i32::from_be_bytes(field))
        .ok()
        .filter(|length| *length >= 4)
    else {
        return Some(Err(Malformed));
    };
    let total = start + length;
    (bytes.len() >= total).then_some(Ok(total))
}

/// Whether the backend bytes, read from a message boundary, hold a complete
/// `ReadyForQuery`.
fn holds_ready_for_query(mut bytes: &[u8]) -> Result<bool, Malformed> {
    while let Some(length) = frame(bytes, true) {
        let length = length?;
        if bytes[0] == b'Z' {
            return Ok(true);
        }
        bytes = &bytes[length..];
    }
    Ok(false)
}

/// The armed fault, taken once, when `message` is the simple-query `COMMIT`.
fn commit_fault(message: &[u8], arming: &Mutex<Arming>) -> Option<Fault> {
    if message.first() != Some(&b'Q') || message.get(5..) != Some(COMMIT) {
        return None;
    }
    let mut arming = lock(arming);
    let Arming::Armed(fault) = *arming else {
        return None;
    };
    *arming = Arming::Fired(fault);
    Some(fault)
}

/// The arming state; a relay that panicked cannot leave it inconsistent.
fn lock(arming: &Mutex<Arming>) -> MutexGuard<'_, Arming> {
    arming.lock().unwrap_or_else(PoisonError::into_inner)
}
