use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::BytesMut;
use infra_postgres::{Dsn, PgPool};
use integration_tests::{DATABASE_URL, dsn_for, url_for};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinError, JoinSet};
use tokio_util::sync::{CancellationToken, DropGuard};
use tokio_util::task::AbortOnDropHandle;

const COMMIT: &[u8] = b"COMMIT";

const JOIN_BUDGET: Duration = Duration::from_secs(5);

const READY_HOLD_BUDGET: Duration = Duration::from_secs(5);

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

#[derive(Clone, Copy, Debug)]
pub(crate) struct Armed {
    fault: Fault,
    boundary: Boundary,
}

#[derive(Clone, Copy, Debug)]
enum Boundary {
    Commit(Option<&'static str>),
    Autocommit(&'static str),
}

impl From<Fault> for Armed {
    fn from(fault: Fault) -> Self {
        Self {
            fault,
            boundary: Boundary::Commit(None),
        }
    }
}

impl From<(Fault, &'static str)> for Armed {
    fn from((fault, statement): (Fault, &'static str)) -> Self {
        Self {
            fault,
            boundary: Boundary::Commit(Some(statement)),
        }
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyBoundary {
    Begin,
    Commit,
    Sync,
    ExecuteSync,
}

#[derive(Debug)]
struct ReadyHold(watch::Sender<ReadyHoldState>);

impl ReadyHold {
    fn new() -> Self {
        let (tx, _) = watch::channel(ReadyHoldState::Idle);
        Self(tx)
    }

    fn arm(&self, boundary: ReadyBoundary) {
        self.0.send_replace(ReadyHoldState::Armed(boundary));
    }

    fn claim(&self, boundary: ReadyBoundary) -> bool {
        self.0.send_if_modified(|state| {
            if *state == ReadyHoldState::Armed(boundary) {
                *state = ReadyHoldState::Claimed;
                true
            } else {
                false
            }
        })
    }

    fn hold(&self) {
        self.0.send_modify(|state| {
            assert_eq!(*state, ReadyHoldState::Claimed);
            *state = ReadyHoldState::Held;
        });
    }

    async fn held(&self) {
        let mut rx = self.0.subscribe();
        tokio::time::timeout(
            READY_HOLD_BUDGET,
            rx.wait_for(|state| *state == ReadyHoldState::Held),
        )
        .await
        .expect("PostgreSQL answers the held request within its budget")
        .expect("ready hold sender stays open");
    }

    fn release(&self) {
        self.0.send_modify(|state| {
            assert_eq!(*state, ReadyHoldState::Held);
            *state = ReadyHoldState::Released;
        });
    }

    async fn wait_for_release(&self, cancel: &CancellationToken) -> bool {
        let mut rx = self.0.subscribe();
        tokio::select! {
            () = cancel.cancelled() => false,
            result = rx.wait_for(|state| *state == ReadyHoldState::Released) => result.is_ok(),
        }
    }
}

#[derive(Debug)]
struct Silence {
    signal: watch::Sender<()>,
    entered: watch::Sender<usize>,
    fault_baseline: AtomicUsize,
}

#[derive(Debug)]
pub(crate) struct CommitProxy {
    address: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    ready_hold: Arc<ReadyHold>,
    silence: Arc<Silence>,
    cancel: CancellationToken,
    _cancel_guard: DropGuard,
    task: AbortOnDropHandle<()>,
}

impl CommitProxy {
    pub(crate) async fn start(server: SocketAddr) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the proxy listens");
        let address = listener.local_addr().expect("the proxy's address");
        let arming = Arc::new(Mutex::new(Arming::Idle));
        let ready_hold = Arc::new(ReadyHold::new());
        let (signal, _) = watch::channel(());
        let (entered, _) = watch::channel(0);
        let silence = Arc::new(Silence {
            signal,
            entered,
            fault_baseline: AtomicUsize::new(0),
        });
        let cancel = CancellationToken::new();
        let cancel_guard = cancel.clone().drop_guard();
        let task = tokio::spawn(accept(
            listener,
            server,
            Arc::clone(&arming),
            Arc::clone(&ready_hold),
            Arc::clone(&silence),
            cancel.clone(),
        ));
        Self {
            address,
            arming,
            ready_hold,
            silence,
            cancel,
            _cancel_guard: cancel_guard,
            task: AbortOnDropHandle::new(task),
        }
    }

    pub(crate) async fn start_for(pool: &PgPool) -> (Self, Dsn) {
        let dsn = dsn_for(pool).await;
        assert_eq!(
            dsn.ssl_mode_name(),
            "disable",
            "the commit proxy frames the plaintext protocol"
        );
        let host = dsn.host().trim_matches(['[', ']']);
        let server = tokio::time::timeout(JOIN_BUDGET, tokio::net::lookup_host((host, dsn.port())))
            .await
            .expect("the server address resolves within budget")
            .expect("the server address resolves")
            .next()
            .expect("the server has an address");
        let proxy = Self::start(server).await;
        let mut url = url_for(pool, DATABASE_URL).await;
        url.set_ip_host(proxy.address().ip())
            .expect("the proxy address is a host");
        url.set_port(Some(proxy.address().port()))
            .expect("the proxy URL accepts a port");
        let proxied = Dsn::admit(url.as_str()).expect("the proxied DSN is admitted");
        (proxy, proxied)
    }

    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    /// Act once on the next `COMMIT`. A bare [`Fault`] matches any connection;
    /// `(fault, sql_substring)` selects a transaction sending that SQL after arming.
    pub(crate) fn arm(&self, fault: impl Into<Armed>) {
        self.silence
            .fault_baseline
            .store(*self.silence.entered.borrow(), Ordering::Release);
        *lock(&self.arming) = Arming::Armed(fault.into());
    }

    /// Act once on the matching autocommit statement's completion boundary.
    /// Extended queries commit at Sync; simple queries complete in their own message.
    pub(crate) fn arm_autocommit(&self, fault: Fault, statement: &'static str) {
        self.silence
            .fault_baseline
            .store(*self.silence.entered.borrow(), Ordering::Release);
        *lock(&self.arming) = Arming::Armed(Armed {
            fault,
            boundary: Boundary::Autocommit(statement),
        });
    }

    pub(crate) fn fired(&self) -> Option<Fault> {
        match *lock(&self.arming) {
            Arming::Fired(fault) => Some(fault),
            Arming::Idle | Arming::Armed(_) => None,
        }
    }

    pub(crate) fn arm_ready_hold(&self, boundary: ReadyBoundary) {
        self.ready_hold.arm(boundary);
    }

    pub(crate) async fn ready_held(&self) {
        self.ready_hold.held().await;
    }

    pub(crate) fn release_ready(&self) {
        self.ready_hold.release();
    }

    pub(crate) fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Silence the one active connection and wait until its relay has stopped.
    /// Already-silent sockets stay open; later connections relay normally.
    pub(crate) async fn silence_connection(&self) {
        assert_eq!(self.silence.signal.receiver_count(), 1);
        let mut entered = self.silence.entered.subscribe();
        let before = *entered.borrow_and_update();
        self.silence.signal.send_replace(());
        tokio::time::timeout(READY_HOLD_BUDGET, entered.wait_for(|n| *n > before))
            .await
            .expect("the relay reaches the silent boundary")
            .expect("silence counter sender stays open");
    }

    /// Wait until a fault has stopped forwarding without closing either socket.
    pub(crate) async fn silenced(&self) {
        let target = self.silence.fault_baseline.load(Ordering::Acquire);
        let mut entered = self.silence.entered.subscribe();
        tokio::time::timeout(READY_HOLD_BUDGET, entered.wait_for(|n| *n > target))
            .await
            .expect("the relay reaches the silent boundary")
            .expect("silence counter sender stays open");
    }

    pub(crate) async fn shutdown(mut self) {
        self.cancel.cancel();
        let result = tokio::time::timeout(JOIN_BUDGET, &mut self.task).await;
        if result.is_err() {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        let join = result.expect("the commit proxy's tasks join");
        propagate_join_panic(join);
    }
}

fn propagate_join_panic(result: Result<(), JoinError>) {
    if let Err(err) = result
        && err.is_panic()
    {
        std::panic::resume_unwind(err.into_panic());
    }
}

async fn accept(
    listener: TcpListener,
    server: SocketAddr,
    arming: Arc<Mutex<Arming>>,
    ready_hold: Arc<ReadyHold>,
    silence: Arc<Silence>,
    cancel: CancellationToken,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                propagate_join_panic(result);
            }
            accepted = listener.accept() => {
                let Ok((client, _)) = accepted else {
                    break;
                };
                let went_silent = silence.signal.subscribe();
                connections.spawn(relay(
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
    }
    while let Some(result) = connections.join_next().await {
        propagate_join_panic(result);
    }
}

/// Returning drops, and so closes, both sockets.
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
                silence.entered.send_modify(|n| *n += 1);
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
                    let hold_ready = frontend.claim_ready(&message, &ready_hold);
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
                        silence.entered.send_modify(|n| *n += 1);
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

#[derive(Debug, Default)]
struct Frontend {
    pending: BytesMut,
    /// The untyped startup message has passed; every later one is typed.
    started: bool,
    /// Prepared statements may be reused without another Parse message.
    statements: HashMap<Vec<u8>, Vec<u8>>,
    portals: HashMap<Vec<u8>, Vec<u8>>,
    matched: bool,
    executed_since_sync: bool,
}

impl Frontend {
    fn take_message(&mut self) -> Option<Result<BytesMut, Malformed>> {
        let framed = frame(&self.pending, self.started)?;
        Some(framed.map(|length| {
            self.started = true;
            self.pending.split_to(length)
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
        let (statement, at_boundary) = match armed.boundary {
            Boundary::Commit(statement) => (statement, commit),
            Boundary::Autocommit(statement) => (
                Some(statement),
                matches!(message.first(), Some(b'Q' | b'S')),
            ),
        };
        if let (Some(statement), Some(sql)) = (statement, sql) {
            self.matched |= std::str::from_utf8(sql).is_ok_and(|sql| sql.contains(statement));
        }
        if !at_boundary {
            return None;
        }
        let matches = statement.is_none() || self.matched;
        self.matched = false;
        if !matches {
            return None;
        }
        *arming = Arming::Fired(armed.fault);
        Some(armed.fault)
    }

    fn claim_ready(&mut self, message: &[u8], hold: &ReadyHold) -> bool {
        if message.first() == Some(&b'E') {
            self.executed_since_sync = true;
        }
        if message.first() == Some(&b'S') {
            if std::mem::take(&mut self.executed_since_sync)
                && hold.claim(ReadyBoundary::ExecuteSync)
            {
                return true;
            }
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

#[test]
fn frontend_preserves_frame_boundaries_across_fragmented_and_coalesced_reads() {
    let startup = b"\0\0\0\x09\0\x03\0\0\0";
    let sync = b"S\0\0\0\x04";
    let query = b"Q\0\0\0\x06x\0";
    let mut frontend = Frontend::default();

    for fragment in [&startup[..2], &startup[2..6]] {
        frontend.pending.extend_from_slice(fragment);
        assert!(frontend.take_message().is_none());
    }
    for fragment in [&startup[6..], sync.as_slice(), sync.as_slice(), &query[..5]] {
        frontend.pending.extend_from_slice(fragment);
    }
    for expected in [startup.as_slice(), sync.as_slice(), sync.as_slice()] {
        assert_eq!(frontend.take_message().unwrap().unwrap().as_ref(), expected);
    }
    assert!(frontend.take_message().is_none());
    frontend.pending.extend_from_slice(&query[5..]);
    assert_eq!(frontend.take_message().unwrap().unwrap().as_ref(), query);
    assert!(frontend.take_message().is_none());
}
