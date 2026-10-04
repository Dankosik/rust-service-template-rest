//! One-shot `PostgreSQL` wire-protocol faults for transaction-boundary proof.
//!
//! It sits between one pool and the server and relays both directions. It
//! frames the frontend's messages, the untyped startup message first and then
//! a type byte and a big-endian `i32` length that counts itself, and it frames
//! the backend's only once it has fired. Armed, it acts once on the first
//! simple-query commit of any connection, optionally restricted to a
//! transaction containing matching SQL. `sqlx-postgres` 0.9.0 commits with a
//! bare `COMMIT`, which is what `infra_postgres::in_tx` sends:
//!
//! - [`Fault::ForwardThenDrop`] forwards it, waits for the server's
//!   `ReadyForQuery`, and closes both sockets without relaying the answer: a
//!   real commit whose acknowledgement is lost.
//! - [`Fault::ForwardThenSilence`] holds both sockets open after the server
//!   answers, withholding all remaining traffic until explicit shutdown.
//! - [`Fault::DropBeforeForward`] closes both sockets before forwarding it:
//!   nothing commits.
//! - [`Fault::ForwardThenCorruptReady`] forwards it and relays the completed
//!   commit with an invalid `ReadyForQuery` status: the driver sees a protocol
//!   error after the real commit.
//!
//! It can also hold the backend's `ReadyForQuery` after forwarding one
//! `BEGIN`, `COMMIT` or protocol Sync. Dropping the client future before release makes the transport
//! boundary distinguish a provider that discards a pending physical connection
//! from one that returns a potentially in-transaction connection to its pool.
//!
//! It fires only once, so a later connection, such as a readback's, passes
//! through. It frames the plaintext protocol only, so the pool's DSN uses
//! `sslmode=disable`.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The text of the simple-query message that commits.
const COMMIT: &[u8] = b"COMMIT";

/// Bound on joining the proxy's tasks.
const JOIN_BUDGET: Duration = Duration::from_secs(5);

/// Bound on observing a held acknowledgement or silent relay.
const READY_HOLD_BUDGET: Duration = Duration::from_secs(5);

/// What an armed proxy does with the first `COMMIT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Forward it, wait for `ReadyForQuery`, then close both sockets: the
    /// commit happens and its acknowledgement is lost.
    ForwardThenDrop,
    /// Forward the boundary, swallow its acknowledgement and keep both sockets
    /// open until shutdown, even after the client abandons its connection.
    ForwardThenSilence,
    /// Close both sockets before forwarding it: nothing commits.
    DropBeforeForward,
    /// Forward it, then corrupt the transaction-status byte of `ReadyForQuery`.
    /// The commit happens, but the driver cannot decode its final response.
    ForwardThenCorruptReady,
}

/// A fault with an optional SQL substring selecting its transaction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Armed {
    fault: Fault,
    statement: Option<&'static str>,
    autocommit: bool,
}

impl From<Fault> for Armed {
    fn from(fault: Fault) -> Self {
        Self {
            fault,
            statement: None,
            autocommit: false,
        }
    }
}

impl From<(Fault, &'static str)> for Armed {
    fn from((fault, statement): (Fault, &'static str)) -> Self {
        Self {
            fault,
            statement: Some(statement),
            autocommit: false,
        }
    }
}

/// The one-shot fault, shared by every relayed connection.
#[derive(Clone, Copy, Debug)]
enum Arming {
    Idle,
    Armed(Armed),
    Fired(Fault),
}

/// One held protocol response. The relay signals only after PostgreSQL has
/// answered with `ReadyForQuery`; releasing it lets the relay finish cleanly
/// after the client-side cancellation has dropped its connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadyHoldState {
    Idle,
    Armed(ReadyBoundary),
    Claimed,
    Held,
    Released,
}

/// Which request will have its `ReadyForQuery` reply withheld once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyBoundary {
    Begin,
    Commit,
    Sync,
}

#[derive(Debug)]
struct ReadyHold {
    state: Mutex<ReadyHoldState>,
    held: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl ReadyHold {
    fn arm(&self, boundary: ReadyBoundary) {
        *lock(&self.state) = ReadyHoldState::Armed(boundary);
    }

    fn claim(&self, boundary: ReadyBoundary) -> bool {
        let mut state = lock(&self.state);
        if *state != ReadyHoldState::Armed(boundary) {
            return false;
        }
        *state = ReadyHoldState::Claimed;
        true
    }

    fn hold(&self) {
        let mut state = lock(&self.state);
        assert_eq!(*state, ReadyHoldState::Claimed);
        *state = ReadyHoldState::Held;
        self.held.notify_one();
    }

    async fn held(&self) {
        tokio::time::timeout(READY_HOLD_BUDGET, self.held.notified())
            .await
            .expect("PostgreSQL answers the held request within its budget");
        assert_eq!(*lock(&self.state), ReadyHoldState::Held);
    }

    fn release(&self) {
        assert_eq!(*lock(&self.state), ReadyHoldState::Held);
        *lock(&self.state) = ReadyHoldState::Released;
        self.release.notify_one();
    }

    async fn wait_for_release(&self, cancel: &CancellationToken) -> bool {
        tokio::select! {
            () = cancel.cancelled() => false,
            () = self.release.notified() => true,
        }
    }
}

/// Silence established connections without resetting either socket.
#[derive(Debug)]
struct Silence {
    signal: watch::Sender<()>,
    reached: Notify,
}

/// The proxy. Its listener and relays run on its own tracker until
/// [`CommitProxy::shutdown`] joins them; dropping it stops them.
#[derive(Debug)]
pub(crate) struct CommitProxy {
    address: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    ready_hold: Arc<ReadyHold>,
    silence: Arc<Silence>,
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
        let ready_hold = Arc::new(ReadyHold {
            state: Mutex::new(ReadyHoldState::Idle),
            held: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let (signal, _) = watch::channel(());
        let silence = Arc::new(Silence {
            signal,
            reached: Notify::new(),
        });
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        tasks.spawn(accept(
            listener,
            server,
            Arc::clone(&arming),
            Arc::clone(&ready_hold),
            Arc::clone(&silence),
            cancel.clone(),
            tasks.clone(),
        ));
        Self {
            address,
            arming,
            ready_hold,
            silence,
            cancel,
            tasks,
        }
    }

    /// Where a pool connects to reach the server through the proxy.
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Act once on the next `COMMIT`. A bare [`Fault`] matches any connection;
    /// `(fault, sql_substring)` selects a transaction sending that SQL after arming.
    pub(crate) fn arm(&self, fault: impl Into<Armed>) {
        *lock(&self.arming) = Arming::Armed(fault.into());
    }

    /// Act once on the matching autocommit statement's completion boundary.
    /// Extended queries commit at Sync; simple queries complete in their own message.
    pub(crate) fn arm_autocommit(&self, fault: Fault, statement: &'static str) {
        *lock(&self.arming) = Arming::Armed(Armed {
            fault,
            statement: Some(statement),
            autocommit: true,
        });
    }

    /// The fault the proxy acted on, once it has.
    pub(crate) fn fired(&self) -> Option<Fault> {
        match *lock(&self.arming) {
            Arming::Fired(fault) => Some(fault),
            Arming::Idle | Arming::Armed(_) => None,
        }
    }

    /// Hold one backend reply after the selected request reaches PostgreSQL.
    pub(crate) fn arm_ready_hold(&self, boundary: ReadyBoundary) {
        self.ready_hold.arm(boundary);
    }

    /// Wait until PostgreSQL has answered but `ReadyForQuery` is still withheld.
    pub(crate) async fn ready_held(&self) {
        self.ready_hold.held().await;
    }

    /// Release the held response after the caller has observed its boundary.
    pub(crate) fn release_ready(&self) {
        self.ready_hold.release();
    }

    /// Stop relaying while a caller still owns an operation using this proxy.
    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Silence the one active connection and wait until its relay has stopped.
    /// Already-silent sockets stay open; later connections relay normally.
    pub(crate) async fn silence_connection(&self) {
        assert_eq!(self.silence.signal.receiver_count(), 1);
        self.silence.signal.send_replace(());
        self.silenced().await;
    }

    /// Wait until a fault has stopped forwarding without closing either socket.
    pub(crate) async fn silenced(&self) {
        tokio::time::timeout(READY_HOLD_BUDGET, self.silence.reached.notified())
            .await
            .expect("the relay reaches the silent boundary");
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
    ready_hold: Arc<ReadyHold>,
    silence: Arc<Silence>,
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
        let went_silent = silence.signal.subscribe();
        tasks.spawn(relay(
            client,
            server,
            Arc::clone(&arming),
            Arc::clone(&ready_hold),
            Arc::clone(&silence),
            went_silent,
            cancel.clone(),
        ));
    }
}

/// Relay one connection until either side closes, the fault fires, or the
/// proxy stops. Returning drops, and so closes, both sockets.
async fn relay(
    mut client: TcpStream,
    server: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    ready_hold: Arc<ReadyHold>,
    silence: Arc<Silence>,
    mut went_silent: watch::Receiver<()>,
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
    // Once a fault forwards its boundary, stop reading the client and frame
    // the server's answer before swallowing the final acknowledgement.
    let mut committing = None;
    let mut holding_ready = false;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = went_silent.changed() => {
                drop(went_silent);
                silence.reached.notify_one();
                cancel.cancelled().await;
                return;
            }
            read = client.read_buf(&mut frontend.pending), if committing.is_none() && !holding_ready => {
                if !matches!(read, Ok(1..)) {
                    return;
                }
                while let Some(message) = frontend.take_message() {
                    let Ok(message) = message else {
                        return;
                    };
                    let hold_ready = Frontend::claim_ready(&message, &ready_hold);
                    match frontend.operation_fault(&message, &arming) {
                        Some(Fault::DropBeforeForward) => return,
                        Some(fault @ (Fault::ForwardThenDrop | Fault::ForwardThenCorruptReady | Fault::ForwardThenSilence)) => {
                            committing = Some(fault);
                        }
                        None => {}
                    }
                    if upstream.write_all(&message).await.is_err() {
                        return;
                    }
                    if hold_ready {
                        holding_ready = true;
                        break;
                    }
                    if committing.is_some() {
                        break;
                    }
                }
            }
            read = upstream.read_buf(&mut backend) => {
                if !matches!(read, Ok(1..)) {
                    return;
                }
                if let Some(fault) = committing {
                    let ready = match ready_for_query(&backend) {
                        Ok(None) => continue,
                        Ok(Some(ready)) => ready,
                        Err(_) => return,
                    };
                    if fault == Fault::ForwardThenCorruptReady {
                        // A complete ReadyForQuery is type + length + status.
                        // Idle independently establishes that COMMIT ended.
                        assert_eq!(backend[ready + 5], b'I', "the server completed COMMIT");
                        backend[ready + 5] = b'?';
                        let _ = client.write_all(&backend).await;
                    }
                    if fault == Fault::ForwardThenSilence {
                        drop(went_silent);
                        silence.reached.notify_one();
                        cancel.cancelled().await;
                    }
                    return;
                } else if holding_ready {
                    // TCP may split either frame, or separate CommandComplete
                    // from ReadyForQuery. Keep every held byte until the latter
                    // is complete before publishing the cancellation point.
                    match ready_for_query(&backend) {
                        Ok(None) => continue,
                        Ok(Some(_)) => ready_hold.hold(),
                        Err(_) => return,
                    }
                    if !ready_hold.wait_for_release(&cancel).await {
                        return;
                    }
                    if client.write_all(&backend).await.is_err() {
                        return;
                    }
                    backend.clear();
                    holding_ready = false;
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
    /// Prepared statements may be reused without another Parse message.
    statements: HashMap<Vec<u8>, Vec<u8>>,
    portals: HashMap<Vec<u8>, Vec<u8>>,
    matched: bool,
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

    /// Track SQL per connection and fault only its selected completion boundary.
    fn operation_fault(&mut self, message: &[u8], arming: &Mutex<Arming>) -> Option<Fault> {
        if message.first() == Some(&b'P') {
            let mut fields = message.get(5..)?.split(|byte| *byte == 0);
            let name = fields.next()?;
            let sql = fields.next()?;
            self.statements.insert(name.to_vec(), sql.to_vec());
        }
        if message.first() == Some(&b'B') {
            let mut fields = message.get(5..)?.split(|byte| *byte == 0);
            let portal = fields.next()?;
            let statement = fields.next()?;
            self.portals.insert(portal.to_vec(), statement.to_vec());
        }
        let sql = match message.first() {
            Some(b'Q') => Some(message.get(5..)?.strip_suffix(&[0])?),
            Some(b'E') => {
                let portal = message.get(5..)?.split(|byte| *byte == 0).next()?;
                Some(self.statements.get(self.portals.get(portal)?)?.as_slice())
            }
            Some(b'S') => None,
            _ => return None,
        };
        if sql.is_some_and(|sql| sql.starts_with(b"BEGIN") || sql == b"ROLLBACK") {
            self.matched = false;
            return None;
        }
        let commit = message.first() == Some(&b'Q') && sql == Some(COMMIT);
        let mut arming = lock(arming);
        let Arming::Armed(armed) = *arming else {
            if commit {
                self.matched = false;
            }
            return None;
        };
        if let (Some(statement), Some(sql)) = (armed.statement, sql) {
            self.matched |= std::str::from_utf8(sql).is_ok_and(|sql| sql.contains(statement));
        }
        let boundary = if armed.autocommit {
            matches!(message.first(), Some(b'Q' | b'S')) && self.matched
        } else {
            commit
        };
        if !boundary {
            return None;
        }
        let matches = armed.statement.is_none() || self.matched;
        self.matched = false;
        if !matches {
            return None;
        }
        *arming = Arming::Fired(armed.fault);
        Some(armed.fault)
    }

    fn claim_ready(message: &[u8], hold: &ReadyHold) -> bool {
        if message.first() == Some(&b'S') {
            return hold.claim(ReadyBoundary::Sync);
        }
        let Some(sql) = message
            .first()
            .filter(|kind| **kind == b'Q')
            .and_then(|_| message.get(5..))
            .and_then(|sql| sql.strip_suffix(&[0]))
        else {
            return false;
        };
        if sql.starts_with(b"BEGIN") {
            hold.claim(ReadyBoundary::Begin)
        } else if sql == COMMIT {
            hold.claim(ReadyBoundary::Commit)
        } else {
            false
        }
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

/// The offset of a complete `ReadyForQuery` in backend bytes read from a
/// message boundary, or `None` while its frame is incomplete.
fn ready_for_query(bytes: &[u8]) -> Result<Option<usize>, Malformed> {
    let mut offset = 0;
    while let Some(length) = frame(&bytes[offset..], true) {
        let length = length?;
        if bytes[offset] == b'Z' {
            return Ok(Some(offset));
        }
        offset += length;
    }
    Ok(None)
}

/// The arming state; a relay that panicked cannot leave it inconsistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
