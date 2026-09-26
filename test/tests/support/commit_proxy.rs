//! One-shot `PostgreSQL` wire-protocol faults for transaction-boundary proof.
//!
//! It sits between one pool and the server and relays both directions. It
//! frames the frontend's messages, the untyped startup message first and then
//! a type byte and a big-endian `i32` length that counts itself, and it frames
//! the backend's only once it has fired. Armed, it acts once on the first
//! simple-query `COMMIT` of any connection, optionally restricted to a
//! transaction containing matching SQL. `sqlx-postgres` 0.9.0 commits this way:
//!
//! - [`Fault::ForwardThenDrop`] forwards it, waits for the server's
//!   `ReadyForQuery`, and closes both sockets without relaying the answer: a
//!   real commit whose acknowledgement is lost.
//! - [`Fault::DropBeforeForward`] closes both sockets before forwarding it:
//!   nothing commits.
//!
//! It can also hold the backend's `ReadyForQuery` after forwarding one
//! `BEGIN`. Dropping the client future before release makes the transport
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
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// The payload of the simple-query message that commits: the text and its
/// NUL terminator.
const COMMIT: &[u8] = b"COMMIT\0";

/// Bound on joining the proxy's tasks.
const JOIN_BUDGET: Duration = Duration::from_secs(5);

/// Bound on observing or releasing the one held `BEGIN` acknowledgement.
const BEGIN_HOLD_BUDGET: Duration = Duration::from_secs(5);

/// What an armed proxy does with the first `COMMIT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Forward it, wait for `ReadyForQuery`, then close both sockets: the
    /// commit happens and its acknowledgement is lost.
    ForwardThenDrop,
    /// Close both sockets before forwarding it: nothing commits.
    DropBeforeForward,
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

/// One held `BEGIN` response. The relay signals only after PostgreSQL has
/// answered with `ReadyForQuery`; releasing it lets the relay finish cleanly
/// after the client-side cancellation has dropped its connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BeginHoldState {
    Idle,
    Armed,
    Claimed,
    Held,
    Released,
}

#[derive(Debug)]
struct BeginHold {
    state: Mutex<BeginHoldState>,
    held: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl BeginHold {
    fn arm(&self) {
        *lock(&self.state) = BeginHoldState::Armed;
    }

    fn claim(&self) -> bool {
        let mut state = lock(&self.state);
        if *state != BeginHoldState::Armed {
            return false;
        }
        *state = BeginHoldState::Claimed;
        true
    }

    fn hold(&self) {
        let mut state = lock(&self.state);
        assert_eq!(*state, BeginHoldState::Claimed);
        *state = BeginHoldState::Held;
        self.held.notify_one();
    }

    async fn held(&self) {
        tokio::time::timeout(BEGIN_HOLD_BUDGET, self.held.notified())
            .await
            .expect("PostgreSQL answers the held BEGIN within its budget");
        assert_eq!(*lock(&self.state), BeginHoldState::Held);
    }

    fn release(&self) {
        assert_eq!(*lock(&self.state), BeginHoldState::Held);
        *lock(&self.state) = BeginHoldState::Released;
        self.release.notify_one();
    }

    async fn wait_for_release(&self, cancel: &CancellationToken) -> bool {
        tokio::select! {
            () = cancel.cancelled() => false,
            () = tokio::time::sleep(BEGIN_HOLD_BUDGET) => false,
            () = self.release.notified() => true,
        }
    }
}

/// The proxy. Its listener and relays run on its own tracker until
/// [`CommitProxy::shutdown`] joins them; dropping it stops them.
#[derive(Debug)]
pub(crate) struct CommitProxy {
    address: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    begin_hold: Arc<BeginHold>,
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
        let begin_hold = Arc::new(BeginHold {
            state: Mutex::new(BeginHoldState::Idle),
            held: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        tasks.spawn(accept(
            listener,
            server,
            Arc::clone(&arming),
            Arc::clone(&begin_hold),
            cancel.clone(),
            tasks.clone(),
        ));
        Self {
            address,
            arming,
            begin_hold,
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

    /// Hold the backend acknowledgement of one forwarded `BEGIN`.
    pub(crate) fn arm_begin_ready_hold(&self) {
        self.begin_hold.arm();
    }

    /// Wait until PostgreSQL has accepted the armed `BEGIN` but its
    /// `ReadyForQuery` is still withheld from the client.
    pub(crate) async fn begin_ready_held(&self) {
        self.begin_hold.held().await;
    }

    /// Release the held `BEGIN` response after the client future is dropped.
    pub(crate) fn release_begin_ready(&self) {
        self.begin_hold.release();
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
    begin_hold: Arc<BeginHold>,
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
        tasks.spawn(relay(
            client,
            server,
            Arc::clone(&arming),
            Arc::clone(&begin_hold),
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
    begin_hold: Arc<BeginHold>,
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
    // Set once the commit boundary is forwarded under `ForwardThenDrop`: from then on the
    // client is not read, and the server's answer is framed and swallowed.
    let mut committing = false;
    let mut holding_begin = false;
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            read = client.read_buf(&mut frontend.pending), if !committing && !holding_begin => {
                if !matches!(read, Ok(1..)) {
                    return;
                }
                while let Some(message) = frontend.take_message() {
                    let Ok(message) = message else {
                        return;
                    };
                    let hold_begin = Frontend::claim_begin(&message, &begin_hold);
                    match frontend.operation_fault(&message, &arming) {
                        Some(Fault::DropBeforeForward) => return,
                        Some(Fault::ForwardThenDrop) => committing = true,
                        None => {}
                    }
                    if upstream.write_all(&message).await.is_err() {
                        return;
                    }
                    if hold_begin {
                        holding_begin = true;
                        break;
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
                    // The final acknowledgement is never relayed: close once
                    // the server is ready again, which it is only after the
                    // commit.
                    if !matches!(holds_ready_for_query(&backend), Ok(false)) {
                        return;
                    }
                } else if holding_begin {
                    // TCP may split either frame, or separate CommandComplete
                    // from ReadyForQuery. Keep every held byte until the latter
                    // is complete before publishing the cancellation point.
                    match holds_ready_for_query(&backend) {
                        Ok(false) => continue,
                        Ok(true) => begin_hold.hold(),
                        Err(_) => return,
                    }
                    if !begin_hold.wait_for_release(&cancel).await {
                        return;
                    }
                    if client.write_all(&backend).await.is_err() {
                        return;
                    }
                    backend.clear();
                    holding_begin = false;
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
        let commit = message.first() == Some(&b'Q') && message.get(5..) == Some(COMMIT);
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

    fn claim_begin(message: &[u8], hold: &BeginHold) -> bool {
        let Some(sql) = message
            .first()
            .filter(|kind| **kind == b'Q')
            .and_then(|_| message.get(5..))
            .and_then(|sql| sql.strip_suffix(&[0]))
        else {
            return false;
        };
        sql.starts_with(b"BEGIN") && hold.claim()
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

/// The arming state; a relay that panicked cannot leave it inconsistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
