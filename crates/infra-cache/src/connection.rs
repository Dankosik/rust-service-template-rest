//! One owned supervisor; redis-rs owns transport and multiplexing.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use backon::BackoffBuilder;
use futures_util::stream::{FuturesUnordered, StreamExt};
use metrics::{Counter, Gauge};
use operation_context::{Deadline, OperationContext, Stopped};
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, SemaphorePermit, mpsc};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::credentials::PasswordFile;
use crate::observe::{
    ADMISSION_REFUSED_METRIC, COMMANDS_IN_FLIGHT_METRIC, PASSWORD_REFRESHES_METRIC,
    RETIREMENTS_METRIC,
};
use crate::{CacheError, ServerEndpoint};

/// Bounds one whole setup attempt: password read, dial, TLS, and `HELLO`.
/// The same limit applies while the server is making progress.
pub(crate) const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTED_PROBE_MIN_BUDGET: Duration = Duration::from_secs(5);
const PASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_BACKOFF_MIN_DELAY: Duration = Duration::from_millis(100);
const SETUP_BACKOFF_MAX_DELAY: Duration = Duration::from_secs(2);
/// Minimum spacing between published generations, so a server that accepts
/// and then drops every connection cannot drive a reconnect loop.
const MIN_GENERATION_SPACING: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_FACTOR: f32 = 2.0;
/// Bound one reconnect chain; the supervisor starts the next after a pause.
const SETUP_BACKOFF_MAX_RETRIES: usize = 6;
/// Spacing of the maintenance `PING` that finds a silently dead connection.
const PING_INTERVAL: Duration = Duration::from_secs(2);
const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Shared by every namespace, including commands waiting for a connection.
const APPLICATION_SLOTS: usize = 256;
const PROBE_SLOTS: usize = 1;
const CALLER_PIPELINE_CAPACITY: usize = APPLICATION_SLOTS + PROBE_SLOTS;
// One additional producer runs maintenance PING and AUTH serially.
const PIPELINE_BUFFER_SIZE: usize = CALLER_PIPELINE_CAPACITY + 1;

#[cfg(test)]
#[path = "tests/full_pipeline.rs"]
mod full_pipeline_tests;

/// Bounded cause of an unsuccessful cache exchange. Server text is never copied.
#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum ErrorType {
    /// A deadline or setup or probe bound elapsed, or the client reported a timeout.
    Timeout,
    /// A transport failure, a closed link, or a connection generation that
    /// retired under the command.
    Io,
    /// The server refused the credentials, including `WRONGPASS` and
    /// `NOAUTH` replies to `HELLO`.
    Auth,
    ServerErrorReply,
    Parse,
    /// A local command slot refusal, which
    /// `cache_command_admission_refused_total` counts apart, an unreadable
    /// password file, a client that could not be rebuilt, or any other
    /// redis-rs error kind.
    Other,
}

pub(crate) enum CommandError {
    Stopped(Stopped),
    Failed(ErrorType),
}

impl From<ErrorType> for CommandError {
    fn from(error: ErrorType) -> Self {
        Self::Failed(error)
    }
}

impl ErrorType {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Io => "io",
            Self::Auth => "auth",
            Self::ServerErrorReply => "response",
            Self::Parse => "parse",
            Self::Other => "other",
        }
    }
}

/// Bounded cause of a Redis client failure.
pub(crate) fn error_type(error: &redis::RedisError) -> ErrorType {
    if error.is_timeout() {
        return ErrorType::Timeout;
    }
    // RESP3 authenticates inside `HELLO`, whose refusal is a plain server error.
    if matches!(error.code(), Some("WRONGPASS" | "NOAUTH")) {
        return ErrorType::Auth;
    }
    match error.kind() {
        redis::ErrorKind::Io => ErrorType::Io,
        redis::ErrorKind::AuthenticationFailed => ErrorType::Auth,
        // redis-rs names only some server codes; `WRONGTYPE`, `OOM` and the
        // other unnamed codes are `Extension`, still a server error reply.
        redis::ErrorKind::Server(_) | redis::ErrorKind::Extension => ErrorType::ServerErrorReply,
        redis::ErrorKind::Parse => ErrorType::Parse,
        // InvalidClientConfig and every other kind stay `other`.
        _ => ErrorType::Other,
    }
}

pub(crate) struct Link {
    pub(crate) server: ServerEndpoint,
    shared: Arc<Shared>,
    /// A full slot set refuses at once rather than queueing.
    application_slots: Semaphore,
    /// Separate from the application slots so application load cannot
    /// hide readiness.
    probe_slots: Semaphore,
    admission_refused: Counter,
    commands_in_flight: Gauge,
    supervisor: AbortHandle,
}

struct LinkMetrics {
    admission_refused: Counter,
    commands_in_flight: Gauge,
    retirements: Counter,
}

struct Shared {
    state: Mutex<State>,
    changed: Notify,
    retirements: Counter,
}

enum State {
    Connecting,
    Connected(Arc<Generation>),
    /// Observable during backoff; the supervisor keeps retrying.
    SetupFailed(ErrorType),
    Closed,
}

struct Generation {
    connection: MultiplexedConnection,
    caller_capacity: Arc<Semaphore>,
    // Each message owns a caller reservation, which bounds the channel.
    abandoned: mpsc::UnboundedSender<AbandonedReply>,
    /// Once cancelled, [`Shared::acquire`] stops handing out this generation
    /// even while [`State::Connected`] still names it. The supervisor then
    /// withdraws it through [`Shared::retire`].
    retired: CancellationToken,
}

struct RetireOnDrop<'a> {
    shared: &'a Shared,
    generation: &'a Arc<Generation>,
}

impl Drop for RetireOnDrop<'_> {
    fn drop(&mut self) {
        self.shared.retire(self.generation);
    }
}

impl Generation {
    async fn reserve(&self) -> Result<OwnedSemaphorePermit, ErrorType> {
        self.caller_capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ErrorType::Io)
    }
}

struct AbandonedReply {
    reservation: OwnedSemaphorePermit,
    reply: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl AbandonedReply {
    async fn wait(self) {
        let _reservation = self.reservation;
        self.reply.await;
    }
}

/// Held from the slot claim until the command ends, including the wait for a
/// connection generation.
struct CommandSlot<'a> {
    _permit: SemaphorePermit<'a>,
    in_flight: &'a Gauge,
}

impl Drop for CommandSlot<'_> {
    fn drop(&mut self) {
        self.in_flight.decrement(1.0);
    }
}

/// A command's stop scope and the cache's own ceiling within it.
pub(crate) struct CommandScope {
    pub(crate) context: OperationContext,
    ceiling: Deadline,
}

impl CommandScope {
    /// `parent` under a `command_timeout` ceiling starting now.
    pub(crate) fn under(parent: &OperationContext, command_timeout: Duration) -> Self {
        let ceiling = Deadline::new(Instant::now(), command_timeout);
        Self {
            context: parent.child_until(ceiling),
            ceiling,
        }
    }

    /// Without a parent there is no child token for anything to cancel.
    pub(crate) fn standalone(command_timeout: Duration) -> Self {
        let ceiling = Deadline::new(Instant::now(), command_timeout);
        Self {
            context: OperationContext::from_deadline(ceiling),
            ceiling,
        }
    }
}

/// The reservation outlives the driver's queue entry, including when the
/// caller leaves before the driver answers.
struct Exchange<'a, F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    shared: &'a Shared,
    generation: &'a Arc<Generation>,
    reservation: Option<OwnedSemaphorePermit>,
    reply: ReplyState<F>,
    keeps_generation: fn(&F::Output) -> bool,
    reply_deadline: Option<Deadline>,
}

enum ReplyState<F> {
    Unpolled(Pin<Box<F>>),
    Pending(Pin<Box<F>>),
    Keep,
    Retire,
}

impl<'a, F> Exchange<'a, F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn new(
        shared: &'a Shared,
        generation: &'a Arc<Generation>,
        reservation: OwnedSemaphorePermit,
        reply: F,
        keeps_generation: fn(&F::Output) -> bool,
        reply_deadline: Option<Deadline>,
    ) -> Self {
        Self {
            shared,
            generation,
            reservation: Some(reservation),
            reply: ReplyState::Unpolled(Box::pin(reply)),
            keeps_generation,
            reply_deadline,
        }
    }

    fn poll_reply(&mut self, cx: &mut Context<'_>) -> Poll<F::Output> {
        // Arm retirement before a poll that could unwind into this exchange's drop.
        let mut reply = match std::mem::replace(&mut self.reply, ReplyState::Retire) {
            ReplyState::Unpolled(reply) | ReplyState::Pending(reply) => reply,
            settled => {
                self.reply = settled;
                return Poll::Pending;
            }
        };
        match reply.as_mut().poll(cx) {
            Poll::Ready(result) => {
                self.reply = if (self.keeps_generation)(&result) {
                    ReplyState::Keep
                } else {
                    ReplyState::Retire
                };
                Poll::Ready(result)
            }
            Poll::Pending => {
                self.reply = ReplyState::Pending(reply);
                Poll::Pending
            }
        }
    }
}

/// A server error reply such as `WRONGTYPE` or `OOM` consumed its own slot,
/// so later replies stay matched and peers on the generation are unaffected.
/// `READONLY` still retires: after a failover the configured endpoint may
/// resolve to the new primary, and only a new connection reaches it.
fn command_keeps_generation<T>(reply: &redis::RedisResult<T>) -> bool {
    reply.as_ref().err().is_none_or(|error| {
        error_type(error) == ErrorType::ServerErrorReply && error.code() != Some("READONLY")
    })
}

impl<F> Drop for Exchange<'_, F>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn drop(&mut self) {
        // The unconstrained first poll has room to enqueue, so re-polling
        // can collect a delivered reply without dispatching new work.
        if matches!(self.reply, ReplyState::Pending(_)) && !std::thread::panicking() {
            let _ = self.poll_reply(&mut Context::from_waker(Waker::noop()));
        }
        let retire = match self.reply {
            ReplyState::Unpolled(_) | ReplyState::Keep => false,
            ReplyState::Pending(_) => self.reply_deadline.is_some_and(Deadline::expired),
            ReplyState::Retire => true,
        };
        if retire {
            self.shared.retire(self.generation);
        } else if let ReplyState::Pending(reply) =
            std::mem::replace(&mut self.reply, ReplyState::Keep)
            && let Some(reservation) = self.reservation.take()
        {
            let keeps_generation = self.keeps_generation;
            let retired = self.generation.retired.clone();
            let _ = self.generation.abandoned.send(AbandonedReply {
                reservation,
                reply: Box::pin(async move {
                    if !keeps_generation(&reply.await) {
                        retired.cancel();
                    }
                }),
            });
        }
    }
}

async fn query<T: redis::FromRedisValue>(
    command: redis::Cmd,
    mut connection: MultiplexedConnection,
) -> redis::RedisResult<T> {
    tokio::task::unconstrained(command.query_async(&mut connection)).await
}

impl std::fmt::Debug for Link {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Link")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

// A transition notifies only after releasing the lock, so a woken `acquire`
// does not block on it.
impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn close(&self) {
        let previous = std::mem::replace(&mut *self.state(), State::Closed);
        if let State::Connected(current) = previous {
            current.retired.cancel();
            self.retirements.increment(1);
        }
        self.changed.notify_waiters();
    }

    fn retire(&self, generation: &Arc<Generation>) {
        let mut state = self.state();
        generation.retired.cancel();
        if matches!(&*state, State::Connected(current) if Arc::ptr_eq(current, generation)) {
            *state = State::Connecting;
            self.retirements.increment(1);
        }
        drop(state);
        self.changed.notify_waiters();
    }

    /// Clears the previous setup failure. Returns whether the link is still open.
    #[must_use]
    fn begin_attempt(&self) -> bool {
        self.transition(State::Connecting)
    }

    /// Returns whether the link is still open.
    #[must_use]
    fn publish(&self, generation: Arc<Generation>) -> bool {
        self.transition(State::Connected(generation))
    }

    /// Returns whether the link is still open.
    #[must_use]
    fn record_failure(&self, error: ErrorType) -> bool {
        self.transition(State::SetupFailed(error))
    }

    fn transition(&self, next: State) -> bool {
        let mut state = self.state();
        if matches!(*state, State::Closed) {
            return false;
        }
        *state = next;
        drop(state);
        self.changed.notify_waiters();
        true
    }

    async fn acquire(&self) -> Result<Arc<Generation>, ErrorType> {
        // A connected link answers without touching the shared waiter list.
        if let Some(settled) = self.settled() {
            return settled;
        }
        loop {
            // Construction captures any subsequent notify_waiters(), even before the first poll.
            let changed = self.changed.notified();
            if let Some(settled) = self.settled() {
                return settled;
            }
            changed.await;
        }
    }

    /// Returns `None` while a command must wait for the next transition.
    fn settled(&self) -> Option<Result<Arc<Generation>, ErrorType>> {
        match &*self.state() {
            State::Closed => Some(Err(ErrorType::Io)),
            State::Connected(current) if !current.retired.is_cancelled() => {
                Some(Ok(current.clone()))
            }
            State::SetupFailed(error) => Some(Err(*error)),
            State::Connected(_) | State::Connecting => None,
        }
    }
}

impl Link {
    pub(crate) fn start(
        server: ServerEndpoint,
        client: redis::Client,
        password_file: Option<PasswordFile>,
        command_timeout: Duration,
    ) -> Self {
        let LinkMetrics {
            admission_refused,
            commands_in_flight,
            retirements,
        } = register_link_metrics();
        let shared = Arc::new(Shared {
            state: Mutex::new(State::Connecting),
            changed: Notify::new(),
            retirements,
        });
        let exit = SupervisorExit {
            shared: shared.clone(),
            server: server.clone(),
        };
        let supervisor =
            tokio::spawn(supervise(exit, client, password_file, command_timeout)).abort_handle();
        Self {
            server,
            shared,
            application_slots: Semaphore::new(APPLICATION_SLOTS),
            probe_slots: Semaphore::new(PROBE_SLOTS),
            admission_refused,
            commands_in_flight,
            supervisor,
        }
    }

    pub(crate) async fn command<T: redis::FromRedisValue + Send + 'static>(
        &self,
        command: redis::Cmd,
        scope: &CommandScope,
    ) -> Result<T, CommandError> {
        let context = &scope.context;
        context.check().map_err(CommandError::Stopped)?;
        let _slot = self.claim_slot(&self.application_slots)?;
        // Acquisition first: a connected link then never arms the deadline timer.
        let generation = tokio::select! {
            biased;
            result = self.shared.acquire() => result,
            stopped = context.wait_stopped() => return Err(CommandError::Stopped(stopped)),
        };
        // Acquisition may have completed at the caller's cutoff.
        context.check().map_err(CommandError::Stopped)?;
        let generation = generation?;
        let exchange = async {
            // The generation's reservation includes abandoned queries. A free
            // reservation guarantees room in the redis-rs sender.
            let reservation = generation.reserve().await?;
            tokio::task::consume_budget().await;
            context.check().map_err(CommandError::Stopped)?;
            if generation.retired.is_cancelled() {
                return Err(ErrorType::Io.into());
            }
            let mut exchange = Exchange::new(
                &self.shared,
                &generation,
                reservation,
                query(command, generation.connection.clone()),
                command_keeps_generation,
                Some(scope.ceiling),
            );
            let reply = poll_fn(|cx| exchange.poll_reply(cx)).await;
            drop(exchange);
            reply.map_err(|error| {
                context
                    .stopped()
                    .map_or_else(|| error_type(&error).into(), CommandError::Stopped)
            })
        };
        // A delivered reply has already cleared its slot. Observe it before
        // stopping the wait so a stop at the ceiling cannot retire an answered exchange.
        tokio::select! {
            biased;
            reply = exchange => reply,
            stopped = context.wait_stopped() => Err(CommandError::Stopped(stopped)),
            () = generation.retired.cancelled() => Err(ErrorType::Io.into()),
        }
    }

    pub(crate) async fn probe(&self, command_timeout: Duration) -> Result<(), ErrorType> {
        let _slot = self.claim_slot(&self.probe_slots)?;
        let generation = self.shared.acquire().await?;
        let budget = command_timeout.max(CONNECTED_PROBE_MIN_BUDGET);
        let reservation = reply_until(&generation.retired, generation.reserve(), || {
            Deadline::new(Instant::now(), budget)
        })
        .await??;
        tokio::task::consume_budget().await;
        if generation.retired.is_cancelled() {
            return Err(ErrorType::Io);
        }
        let mut exchange = Exchange::new(
            &self.shared,
            &generation,
            reservation,
            query(redis::Cmd::ping(), generation.connection.clone()),
            ping_keeps_generation,
            None,
        );
        let first_poll = poll_fn(|cx| Poll::Ready(exchange.poll_reply(cx))).await;
        let reply = match first_poll {
            Poll::Ready(reply) => reply,
            Poll::Pending => {
                // Start the deadline after enqueueing PING, so every earlier command
                // has already started its own budget.
                let deadline = Deadline::new(Instant::now(), budget);
                exchange.reply_deadline = Some(deadline);
                reply_until(
                    &generation.retired,
                    poll_fn(|cx| exchange.poll_reply(cx)),
                    || deadline,
                )
                .await?
            }
        };
        reply.map_err(|error| error_type(&error))
    }

    fn claim_slot<'a>(&'a self, slots: &'a Semaphore) -> Result<CommandSlot<'a>, ErrorType> {
        let permit = slots.try_acquire().map_err(|_| {
            self.admission_refused.increment(1);
            ErrorType::Other
        })?;
        self.commands_in_flight.increment(1.0);
        Ok(CommandSlot {
            _permit: permit,
            in_flight: &self.commands_in_flight,
        })
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.close();
        self.supervisor.abort();
    }
}

fn register_link_metrics() -> LinkMetrics {
    let metrics = LinkMetrics {
        admission_refused: metrics::counter!(ADMISSION_REFUSED_METRIC),
        commands_in_flight: metrics::gauge!(COMMANDS_IN_FLIGHT_METRIC),
        retirements: metrics::counter!(RETIREMENTS_METRIC),
    };
    // Exported at zero so a scrape tells "no events yet" from "not exported".
    metrics.admission_refused.increment(0);
    // Several links may share the unlabelled series: never reset active work.
    metrics.commands_in_flight.increment(0.0);
    metrics.retirements.increment(0);
    metrics
}

// Captured before spawn so an exit before the first poll also closes the link.
struct SupervisorExit {
    shared: Arc<Shared>,
    server: ServerEndpoint,
}

impl Drop for SupervisorExit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // The process panic hook names the location, not that cache
            // recovery has stopped for the rest of the process.
            tracing::error!(
                server.address = %self.server.host,
                server.port = self.server.port,
                "cache_supervisor_panicked"
            );
        }
        self.shared.close();
    }
}

// Check retirement first and poll the work before sampling the deadline.
async fn reply_until<T>(
    retired: &CancellationToken,
    work: impl Future<Output = T>,
    deadline: impl FnOnce() -> Deadline,
) -> Result<T, ErrorType> {
    tokio::select! {
        biased;
        () = retired.cancelled() => Err(ErrorType::Io),
        result = work => Ok(result),
        () = async { deadline().wait().await } => Err(ErrorType::Timeout),
    }
}

async fn query_until(
    generation: &Generation,
    command: &redis::Cmd,
    deadline: impl FnOnce() -> Deadline,
) -> Result<redis::RedisResult<()>, ErrorType> {
    tokio::task::consume_budget().await;
    let mut connection = generation.connection.clone();
    let reply = tokio::task::unconstrained(command.query_async(&mut connection));
    // Maintenance has a reserved entry, so its first poll enqueues before
    // the timer starts, after every earlier command has started its budget.
    reply_until(&generation.retired, reply, deadline).await
}

async fn ping_until(
    generation: &Generation,
    command_timeout: Duration,
) -> Result<(), (ErrorType, Disposition)> {
    let reply = query_until(generation, &redis::Cmd::ping(), || {
        Deadline::new(Instant::now(), command_timeout)
    })
    .await
    .map_err(|error| (error, Disposition::Retire))?;
    let disposition = if ping_keeps_generation(&reply) {
        Disposition::Keep
    } else {
        Disposition::Retire
    };
    reply.map_err(|error| (error_type(&error), disposition))
}

fn ping_keeps_generation(reply: &redis::RedisResult<()>) -> bool {
    // ACL can refuse PING while still allowing cache operations.
    reply
        .as_ref()
        .err()
        .is_none_or(|error| error.code() == Some("NOPERM"))
}

async fn supervise(
    exit: SupervisorExit,
    client: redis::Client,
    password_file: Option<PasswordFile>,
    command_timeout: Duration,
) {
    run_supervisor(
        &exit.shared,
        &exit.server,
        &client,
        password_file.as_ref(),
        command_timeout,
    )
    .await;
}

async fn run_supervisor(
    shared: &Shared,
    server: &ServerEndpoint,
    client: &redis::Client,
    password_file: Option<&PasswordFile>,
    command_timeout: Duration,
) {
    let backoff_policy = backon::ExponentialBuilder::default()
        .with_min_delay(SETUP_BACKOFF_MIN_DELAY)
        .with_max_delay(SETUP_BACKOFF_MAX_DELAY)
        .with_factor(SETUP_BACKOFF_FACTOR)
        .with_max_times(SETUP_BACKOFF_MAX_RETRIES)
        .with_jitter();
    let mut backoff = backoff_policy.build();
    let mut next_attempt = Instant::now();
    loop {
        sleep_until(next_attempt).await;
        if !shared.begin_attempt() {
            return;
        }
        let (abandoned, replies) = mpsc::unbounded_channel();
        match setup_attempt(server, client, password_file, abandoned).await {
            Ok((generation, rotation)) => {
                if !shared.publish(generation.clone()) {
                    return;
                }
                next_attempt = Instant::now() + MIN_GENERATION_SPACING;
                backoff = backoff_policy.build();
                let mut drain = std::pin::pin!(drain_abandoned_replies(replies));
                // Withdraw the generation before the drain releases its
                // reservations, including on unwind.
                let _retire = RetireOnDrop {
                    shared,
                    generation: &generation,
                };
                tokio::select! {
                    biased;
                    () = maintain(server, &generation, rotation, command_timeout) => {}
                    () = &mut drain => {}
                }
            }
            Err(error) => {
                if !shared.record_failure(error) {
                    return;
                }
                tracing::warn!(
                    server.address = %server.host,
                    server.port = server.port,
                    error.type = error.label(),
                    "cache_connection_failed"
                );
                let delay = if let Some(delay) = backoff.next() {
                    delay
                } else {
                    // The chain is exhausted: pause, then start the next one.
                    backoff = backoff_policy.build();
                    SETUP_BACKOFF_MAX_DELAY
                };
                // backon's jitter is additive after its cap; cap the final delay.
                next_attempt = Instant::now() + delay.min(SETUP_BACKOFF_MAX_DELAY);
            }
        }
    }
}

async fn drain_abandoned_replies(mut replies: mpsc::UnboundedReceiver<AbandonedReply>) {
    let mut pending = FuturesUnordered::new();
    loop {
        tokio::select! {
            biased;
            _ = pending.next(), if !pending.is_empty() => {}
            reply = replies.recv() => match reply {
                Some(reply) => pending.push(reply.wait()),
                None => return,
            }
        }
    }
}

/// One bounded setup attempt. A connection the peer closes before publication
/// is [`ErrorType::Io`]; the returned generation owns its retirement token.
async fn setup_attempt<'a>(
    server: &ServerEndpoint,
    client: &redis::Client,
    password_file: Option<&'a PasswordFile>,
    abandoned: mpsc::UnboundedSender<AbandonedReply>,
) -> Result<(Arc<Generation>, Option<Rotation<'a>>), ErrorType> {
    let retired = CancellationToken::new();
    let setup = async {
        let (client, rotation) = credentialed_client(server, client, password_file).await?;
        let connection = connect(&client, retired.clone()).await?;
        Ok((connection, rotation))
    };
    let result = match timeout(SETUP_TIMEOUT, setup).await {
        Err(_elapsed) => Err(ErrorType::Timeout),
        // The server disconnected the new connection before publication.
        Ok(Ok(_)) if retired.is_cancelled() => Err(ErrorType::Io),
        Ok(connected) => connected,
    };
    result.map(|(connection, rotation)| {
        (
            Arc::new(Generation {
                connection,
                caller_capacity: Arc::new(Semaphore::new(CALLER_PIPELINE_CAPACITY)),
                abandoned,
                retired,
            }),
            rotation,
        )
    })
}

/// The admitted client, or one rebuilt with the password file's current
/// content. The rotation is present exactly when a password file is.
async fn credentialed_client<'a>(
    server: &ServerEndpoint,
    admitted: &redis::Client,
    password_file: Option<&'a PasswordFile>,
) -> Result<(redis::Client, Option<Rotation<'a>>), ErrorType> {
    let Some(file) = password_file else {
        return Ok((admitted.clone(), None));
    };
    // Not `auth`: an unreadable mount must not look like a revoked
    // password, which an operator would answer by rotating the secret.
    let password = file.read().await.map_err(|error| {
        warn_password_file_unreadable(server, &error);
        ErrorType::Other
    })?;
    let info = admitted.get_connection_info().clone();
    let settings = info
        .redis_settings()
        .clone()
        .set_username(file.username())
        .set_password(&password);
    let client =
        redis::Client::open(info.set_redis_settings(settings)).map_err(|_| ErrorType::Other)?;
    Ok((client, Some(Rotation::new(file, password))))
}

fn warn_password_file_unreadable(server: &ServerEndpoint, error: &CacheError) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        %error,
        "cache_password_file_unreadable"
    );
}

async fn connect(
    client: &redis::Client,
    retired: CancellationToken,
) -> Result<MultiplexedConnection, ErrorType> {
    // redis-rs defaults (500 ms response, 1 s connect) would cut the command
    // context and the setup bound short; those own every deadline here.
    let config = redis::AsyncConnectionConfig::new()
        .set_response_timeout(None)
        .set_connection_timeout(None)
        .set_pipeline_buffer_size(PIPELINE_BUFFER_SIZE)
        .set_push_sender(move |push: redis::PushInfo| {
            if push.kind == redis::PushKind::Disconnection {
                retired.cancel();
            }
            Ok::<(), redis::aio::SendError>(())
        });
    client
        .get_multiplexed_async_connection_with_config(&config)
        .await
        .map_err(|error| error_type(&error))
}

/// Keeps `generation` checked until it must retire. The caller retires it
/// when this returns.
async fn maintain(
    server: &ServerEndpoint,
    generation: &Generation,
    mut rotation: Option<Rotation<'_>>,
    command_timeout: Duration,
) {
    let mut next_ping = Instant::now() + PING_INTERVAL;
    let mut next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
    loop {
        tokio::select! {
            biased;
            () = generation.retired.cancelled() => return,
            // Keep the start-to-start refresh cadence while letting an older
            // PING run after a refresh that consumed its entire interval.
            () = sleep_until(next_refresh), if rotation.is_some() && next_refresh <= next_ping => {
                // A rejected AUTH must not add its duration to the next refresh.
                next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
                if let Some(rotation) = rotation.as_mut()
                    && rotation.refresh_and_record(server, generation, command_timeout).await
                        == Disposition::Retire
                {
                    return;
                }
            }
            () = sleep_until(next_ping) => {
                // Replies are FIFO, so this PING waits behind every command
                // sent before it. A budget shorter than `command_timeout`
                // would retire the generation, and fail those commands, before
                // their own budget ends.
                if let Err((error, disposition)) = ping_until(generation, command_timeout).await {
                    tracing::warn!(
                        server.address = %server.host,
                        server.port = server.port,
                        error.type = error.label(),
                        "cache_ping_failed"
                    );
                    if disposition == Disposition::Retire {
                        return;
                    }
                }
                next_ping = Instant::now() + PING_INTERVAL;
            }
        }
    }
}

/// Password-file rotation for one generation.
struct Rotation<'a> {
    file: &'a PasswordFile,
    /// The password the server last accepted on this generation.
    authenticated_password: String,
    /// The current run of read failures has already been logged.
    unreadable_logged: bool,
}

enum RefreshOutcome {
    ReadFailed(CacheError),
    Unchanged,
    AuthAccepted,
    /// The server refused the new password; the authenticated session stays.
    AuthRefused,
    Failed(ErrorType),
}

impl RefreshOutcome {
    /// The `outcome` and `reason` labels.
    fn labels(&self) -> (&'static str, &'static str) {
        match self {
            Self::ReadFailed(_) => ("read_failed", "none"),
            Self::Unchanged => ("unchanged", "none"),
            Self::AuthAccepted => ("auth_accepted", "none"),
            Self::AuthRefused => ("refresh_failed", ErrorType::Auth.label()),
            Self::Failed(error) => ("refresh_failed", error.label()),
        }
    }

    fn disposition(&self) -> Disposition {
        match self {
            Self::ReadFailed(_) | Self::Unchanged | Self::AuthAccepted | Self::AuthRefused => {
                Disposition::Keep
            }
            Self::Failed(_) => Disposition::Retire,
        }
    }
}

/// Whether a maintenance exchange leaves the generation usable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    Keep,
    Retire,
}

impl<'a> Rotation<'a> {
    fn new(file: &'a PasswordFile, authenticated_password: String) -> Self {
        Self {
            file,
            authenticated_password,
            unreadable_logged: false,
        }
    }

    async fn refresh_and_record(
        &mut self,
        server: &ServerEndpoint,
        generation: &Generation,
        command_timeout: Duration,
    ) -> Disposition {
        let outcome = tokio::select! {
            biased;
            // A generation retired mid-refresh records nothing.
            () = generation.retired.cancelled() => return Disposition::Retire,
            outcome = self.refresh(generation, command_timeout) => outcome,
        };
        self.record(server, &outcome);
        outcome.disposition()
    }

    async fn refresh(
        &mut self,
        generation: &Generation,
        command_timeout: Duration,
    ) -> RefreshOutcome {
        // A read that outlives the bound is unreadable, not a connection
        // failure: the authenticated session stays usable.
        let read_deadline = Instant::now() + PASSWORD_READ_TIMEOUT;
        let password = match timeout_at(read_deadline, self.file.read()).await {
            Ok(Ok(password)) => password,
            Ok(Err(error)) => return RefreshOutcome::ReadFailed(error),
            Err(_elapsed) => {
                return RefreshOutcome::ReadFailed(CacheError::PasswordFile {
                    kind: std::io::ErrorKind::TimedOut,
                });
            }
        };
        if password == self.authenticated_password {
            return RefreshOutcome::Unchanged;
        }
        let mut command = redis::cmd("AUTH");
        command.arg(self.file.username()).arg(&password);
        // File I/O must not consume the time owed to commands ahead of AUTH.
        let result = query_until(generation, &command, || {
            let now = Instant::now();
            Deadline::new(
                now,
                command_timeout.max(read_deadline.saturating_duration_since(now)),
            )
        })
        .await
        .and_then(|reply| reply.map_err(|error| error_type(&error)));
        match result {
            Ok(()) => {}
            Err(ErrorType::Auth) => return RefreshOutcome::AuthRefused,
            Err(error) => return RefreshOutcome::Failed(error),
        }
        if generation.retired.is_cancelled() {
            return RefreshOutcome::Failed(ErrorType::Io);
        }
        self.authenticated_password = password;
        RefreshOutcome::AuthAccepted
    }

    fn record(&mut self, server: &ServerEndpoint, outcome: &RefreshOutcome) {
        let (label, reason) = outcome.labels();
        metrics::counter!(PASSWORD_REFRESHES_METRIC, "outcome" => label, "reason" => reason)
            .increment(1);
        let read_failed = matches!(outcome, RefreshOutcome::ReadFailed(_));
        let unreadable_logged = std::mem::replace(&mut self.unreadable_logged, read_failed);
        match outcome {
            RefreshOutcome::ReadFailed(error) => {
                if !unreadable_logged {
                    warn_password_file_unreadable(server, error);
                }
            }
            RefreshOutcome::Unchanged => {}
            RefreshOutcome::AuthAccepted => tracing::info!(
                server.address = %server.host,
                server.port = server.port,
                "cache_password_reloaded"
            ),
            RefreshOutcome::AuthRefused | RefreshOutcome::Failed(_) => {
                tracing::warn!(
                    server.address = %server.host,
                    server.port = server.port,
                    error.type = reason,
                    "cache_password_refresh_failed"
                );
            }
        }
    }
}
