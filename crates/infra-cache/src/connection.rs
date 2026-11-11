//! One owned supervisor; redis-rs owns transport and multiplexing.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use backon::BackoffBuilder;
use metrics::{Counter, Gauge};
use operation_context::OperationContext;
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, Semaphore, SemaphorePermit};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::ServerEndpoint;
use crate::credentials::PasswordFile;
use crate::observe::{
    self, ADMISSION_REFUSED_METRIC, COMMANDS_IN_FLIGHT_METRIC, ErrorType,
    PASSWORD_REFRESHES_METRIC, RETIREMENTS_METRIC,
};

/// Bounds one whole setup attempt: password read, dial, TLS, and `HELLO`. A
/// hang guard, not a latency target: a slow but live server is waited for.
pub(crate) const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Ceiling on one exchange over an established connection that no caller
/// context bounds: the readiness probe and the password refresh.
const CONNECTED_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);
/// Floor of the supervisor's exponential setup backoff.
const MIN_DELAY: Duration = Duration::from_millis(100);
/// Cap on setup backoff.
const MAX_DELAY: Duration = Duration::from_secs(2);
/// Minimum spacing between published generations, so a server that accepts
/// and then drops every connection cannot drive a reconnect loop.
const MIN_GENERATION_SPACING: Duration = Duration::from_secs(2);
/// Factor of the supervisor's exponential setup backoff.
const EXPONENT_BASE: f32 = 2.0;
/// Bound one reconnect chain; the supervisor starts the next after a pause.
const NUMBER_OF_RETRIES: usize = 6;
/// Spacing of the maintenance `PING` that finds a silently dead connection.
const PING_INTERVAL: Duration = Duration::from_secs(2);
const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// Shared by every namespace, including commands waiting for a connection.
const APPLICATION_SLOTS: usize = 256;

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
    /// The published generation. The driver's disconnect push may cancel its
    /// `retired` token while the state still names it; [`Shared::acquire`]
    /// refuses that token and the supervisor's `retire` moves the state.
    Connected(Arc<Generation>),
    /// Observable during backoff; the supervisor keeps retrying.
    SetupFailed(ErrorType),
    Closed,
}

struct Generation {
    connection: MultiplexedConnection,
    /// Cancelled by the driver's disconnect push, [`Shared::close`], or
    /// [`Shared::retire`]. A cancelled token is never handed out, even while
    /// [`State::Connected`] still names this generation.
    retired: CancellationToken,
}

/// Includes admitted commands still waiting for a connection generation.
struct Admission<'a> {
    _permit: SemaphorePermit<'a>,
    in_flight: &'a Gauge,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.in_flight.decrement(1.0);
    }
}

/// An exchange that does not end in `Ok` or a delivered server error reply,
/// including one dropped mid-flight, retires its generation before its
/// admission is released, so no later command can read a reply it left on
/// the connection.
struct Exchange<'a> {
    shared: &'a Shared,
    generation: Arc<Generation>,
    _admission: Admission<'a>,
    keeps_generation: bool,
}

impl<'a> Exchange<'a> {
    fn new(shared: &'a Shared, generation: Arc<Generation>, admission: Admission<'a>) -> Self {
        Self {
            shared,
            generation,
            _admission: admission,
            keeps_generation: false,
        }
    }

    fn finish<T>(mut self, result: Result<T, ErrorType>) -> Result<T, ErrorType> {
        self.keeps_generation = result.is_ok();
        result
    }

    /// Ends on the command's own reply: `Ok`, or a server error reply that
    /// answered it and left the connection in step.
    fn finish_reply<T>(mut self, reply: redis::RedisResult<T>) -> Result<T, ErrorType> {
        self.keeps_generation = match &reply {
            Ok(_) => true,
            Err(error) => reply_keeps_generation(error),
        };
        reply.map_err(|error| observe::error_type(&error))
    }
}

/// A server error reply such as `WRONGTYPE` or `OOM` consumed its own slot,
/// so later replies stay matched and peers on the generation are unaffected.
/// `READONLY` still retires: after a failover the configured endpoint may
/// resolve to the new primary, and only a new connection reaches it.
fn reply_keeps_generation(error: &redis::RedisError) -> bool {
    observe::error_type(error) == ErrorType::Response && error.code() != Some("READONLY")
}

impl Drop for Exchange<'_> {
    fn drop(&mut self) {
        if !self.keeps_generation {
            self.shared.retire(&self.generation);
        }
        // Accounting and the permit drop only after retirement returns.
    }
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
        loop {
            // Register before examining state so publication cannot be missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match &*self.state() {
                State::Closed => return Err(ErrorType::Io),
                State::Connected(current) if !current.retired.is_cancelled() => {
                    return Ok(current.clone());
                }
                State::SetupFailed(error) => return Err(*error),
                State::Connected(_) | State::Connecting => {}
            }
            changed.await;
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
        let supervisor = tokio::spawn(supervise(
            shared.clone(),
            server.clone(),
            client,
            password_file,
            command_timeout,
        ))
        .abort_handle();
        Self {
            server,
            shared,
            application_slots: Semaphore::new(APPLICATION_SLOTS),
            probe_slots: Semaphore::new(1),
            admission_refused,
            commands_in_flight,
            supervisor,
        }
    }

    pub(crate) async fn command<T: redis::FromRedisValue>(
        &self,
        command: &redis::Cmd,
        context: &OperationContext,
    ) -> Result<T, ErrorType> {
        context.check().map_err(|_| ErrorType::Timeout)?;
        let admission = self.admit(&self.application_slots)?;
        let generation = tokio::select! {
            biased;
            _ = context.wait_stopped() => return Err(ErrorType::Timeout),
            result = self.shared.acquire() => result?,
        };
        // A stop seen only by the exchange would retire this healthy generation.
        context.check().map_err(|_| ErrorType::Timeout)?;
        self.exchange_with_context(generation, admission, command, context)
            .await
    }

    pub(crate) async fn probe(&self) -> Result<(), ErrorType> {
        let admission = self.admit(&self.probe_slots)?;
        let generation = self.shared.acquire().await?;
        self.exchange(
            generation,
            admission,
            &redis::Cmd::ping(),
            Instant::now() + CONNECTED_EXCHANGE_TIMEOUT,
        )
        .await
    }

    fn admit<'a>(&'a self, slots: &'a Semaphore) -> Result<Admission<'a>, ErrorType> {
        let permit = slots.try_acquire().map_err(|_| {
            self.admission_refused.increment(1);
            ErrorType::Other
        })?;
        self.commands_in_flight.increment(1.0);
        Ok(Admission {
            _permit: permit,
            in_flight: &self.commands_in_flight,
        })
    }

    async fn exchange<T: redis::FromRedisValue>(
        &self,
        generation: Arc<Generation>,
        admission: Admission<'_>,
        command: &redis::Cmd,
        deadline: Instant,
    ) -> Result<T, ErrorType> {
        let guard = Exchange::new(&self.shared, generation, admission);
        let result = query_until(&guard.generation, command, deadline).await;
        guard.finish(result)
    }

    async fn exchange_with_context<T: redis::FromRedisValue>(
        &self,
        generation: Arc<Generation>,
        admission: Admission<'_>,
        command: &redis::Cmd,
        context: &OperationContext,
    ) -> Result<T, ErrorType> {
        let guard = Exchange::new(&self.shared, generation, admission);
        let mut connection = guard.generation.connection.clone();
        tokio::select! {
            biased;
            _ = context.wait_stopped() => guard.finish(Err(ErrorType::Timeout)),
            () = guard.generation.retired.cancelled() => guard.finish(Err(ErrorType::Io)),
            reply = command.query_async(&mut connection) => guard.finish_reply(reply),
        }
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

// Ensures a panic or unexpected exit cannot leave an apparently live cache.
struct SupervisorExit(Arc<Shared>);
impl Drop for SupervisorExit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // The process panic hook names the location, not that cache
            // recovery has stopped for the rest of the process.
            tracing::error!("cache_supervisor_panicked");
        }
        self.0.close();
    }
}

/// One command on `generation` until `deadline`. Never retires the generation:
/// a refused password refresh keeps the authenticated session usable.
async fn query_until<T: redis::FromRedisValue>(
    generation: &Generation,
    command: &redis::Cmd,
    deadline: Instant,
) -> Result<T, ErrorType> {
    let mut connection = generation.connection.clone();
    tokio::select! {
        biased;
        () = generation.retired.cancelled() => Err(ErrorType::Io),
        result = timeout_at(deadline, command.query_async(&mut connection)) => {
            result.map_err(|_| ErrorType::Timeout)?.map_err(|error| observe::error_type(&error))
        }
    }
}

async fn supervise(
    shared: Arc<Shared>,
    server: ServerEndpoint,
    client: redis::Client,
    password_file: Option<PasswordFile>,
    command_timeout: Duration,
) {
    let _exit = SupervisorExit(shared.clone());
    run_supervisor(
        &shared,
        &server,
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
    let retry = backon::ExponentialBuilder::default()
        .with_min_delay(MIN_DELAY)
        .with_max_delay(MAX_DELAY)
        .with_factor(EXPONENT_BASE)
        .with_max_times(NUMBER_OF_RETRIES)
        .with_jitter();
    let mut backoff = retry.build();
    let mut next_connection = Instant::now();
    loop {
        sleep_until(next_connection).await;
        if !shared.begin_attempt() {
            return;
        }
        match setup_attempt(server, client, password_file).await {
            Ok((generation, rotation)) => {
                if !shared.publish(generation.clone()) {
                    return;
                }
                next_connection = Instant::now() + MIN_GENERATION_SPACING;
                backoff = retry.build();
                maintain(server, &generation, rotation, command_timeout).await;
                shared.retire(&generation);
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
                    backoff = retry.build();
                    MAX_DELAY
                };
                // backon's jitter is additive after its cap; cap the final delay.
                next_connection = Instant::now() + delay.min(MAX_DELAY);
            }
        }
    }
}

/// One bounded setup attempt. A connection the peer closes before publication
/// is `Io`; the returned generation owns its retirement token.
async fn setup_attempt<'a>(
    server: &ServerEndpoint,
    client: &redis::Client,
    password_file: Option<&'a PasswordFile>,
) -> Result<(Arc<Generation>, Option<Rotation<'a>>), ErrorType> {
    let retired = CancellationToken::new();
    let result = match timeout(
        SETUP_TIMEOUT,
        connect(server, client, password_file, retired.clone()),
    )
    .await
    {
        Err(_elapsed) => Err(ErrorType::Timeout),
        // The server disconnected the new connection before publication.
        Ok(Ok(_)) if retired.is_cancelled() => Err(ErrorType::Io),
        Ok(connected) => connected,
    };
    result.map(|(connection, rotation)| {
        (
            Arc::new(Generation {
                connection,
                retired,
            }),
            rotation,
        )
    })
}

/// The rotation is present exactly when a password file is.
async fn connect<'a>(
    server: &ServerEndpoint,
    admitted: &redis::Client,
    password_file: Option<&'a PasswordFile>,
    retired: CancellationToken,
) -> Result<(MultiplexedConnection, Option<Rotation<'a>>), ErrorType> {
    let (client, rotation) = match password_file {
        Some(file) => {
            // Not `auth`: an unreadable mount must not look like a revoked
            // password, which an operator would answer by rotating the secret.
            let password = file.read().await.map_err(|error| {
                tracing::warn!(
                    server.address = %server.host,
                    server.port = server.port,
                    %error,
                    "cache_password_file_unreadable"
                );
                ErrorType::Other
            })?;
            let info = admitted.get_connection_info().clone();
            let settings = info
                .redis_settings()
                .clone()
                .set_username(file.username())
                .set_password(&password);
            let client = redis::Client::open(info.set_redis_settings(settings))
                .map_err(|_| ErrorType::Other)?;
            (client, Some(Rotation::new(file, password)))
        }
        None => (admitted.clone(), None),
    };
    // redis-rs defaults (500 ms response, 1 s connect) would cut the command
    // context and the setup bound short; those own every deadline here.
    let config = redis::AsyncConnectionConfig::new()
        .set_response_timeout(None)
        .set_connection_timeout(None)
        .set_push_sender(move |push: redis::PushInfo| {
            if push.kind == redis::PushKind::Disconnection {
                retired.cancel();
            }
            Ok::<(), redis::aio::SendError>(())
        });
    let connection = client
        .get_multiplexed_async_connection_with_config(&config)
        .await
        .map_err(|error| observe::error_type(&error))?;
    Ok((connection, rotation))
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
            () = sleep_until(next_refresh), if rotation.is_some() => {
                // A rejected AUTH must not add its duration to the next refresh.
                next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
                if let Some(rotation) = rotation.as_mut()
                    && rotation.refresh_within_bound(server, generation).await == Disposition::Retire
                {
                    return;
                }
            }
            () = sleep_until(next_ping) => {
                // Replies are FIFO, so this PING waits behind every command
                // sent before it. A budget shorter than `command_timeout`
                // would retire the generation, and fail those commands, before
                // their own budget ends.
                let deadline = Instant::now() + command_timeout;
                if let Err(error) = query_until::<()>(generation, &redis::Cmd::ping(), deadline).await {
                    tracing::warn!(
                        server.address = %server.host,
                        server.port = server.port,
                        error.type = error.label(),
                        "cache_ping_failed"
                    );
                    return;
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

/// How one password refresh ended.
enum RefreshOutcome {
    ReadFailed(crate::CacheError),
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

/// Whether a generation stays usable after a password refresh.
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

    /// Runs one refresh under [`CONNECTED_EXCHANGE_TIMEOUT`] and records a
    /// completed outcome; a generation retired mid-refresh records nothing.
    async fn refresh_within_bound(
        &mut self,
        server: &ServerEndpoint,
        generation: &Generation,
    ) -> Disposition {
        let deadline = Instant::now() + CONNECTED_EXCHANGE_TIMEOUT;
        let outcome = tokio::select! {
            biased;
            // A generation retired mid-refresh records nothing.
            () = generation.retired.cancelled() => return Disposition::Retire,
            outcome = timeout_at(deadline, self.refresh(generation, deadline)) => {
                outcome.unwrap_or(RefreshOutcome::Failed(ErrorType::Timeout))
            }
        };
        self.record(server, &outcome);
        outcome.disposition()
    }

    async fn refresh(&mut self, generation: &Generation, deadline: Instant) -> RefreshOutcome {
        // A read that outlives the bound is unreadable, not a connection
        // failure: the authenticated session stays usable.
        let password = match timeout_at(deadline, self.file.read()).await {
            Ok(Ok(password)) => password,
            Ok(Err(error)) => return RefreshOutcome::ReadFailed(error),
            Err(_elapsed) => {
                return RefreshOutcome::ReadFailed(crate::CacheError::PasswordFile {
                    kind: std::io::ErrorKind::TimedOut,
                });
            }
        };
        self.unreadable_logged = false;
        if password == self.authenticated_password {
            return RefreshOutcome::Unchanged;
        }
        let mut command = redis::cmd("AUTH");
        command.arg(self.file.username()).arg(&password);
        match query_until::<()>(generation, &command, deadline).await {
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
        match outcome {
            RefreshOutcome::ReadFailed(error) => {
                if !std::mem::replace(&mut self.unreadable_logged, true) {
                    tracing::warn!(
                        server.address = %server.host,
                        server.port = server.port,
                        %error,
                        "cache_password_file_unreadable"
                    );
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
