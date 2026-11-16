use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use arc_swap::ArcSwap;
use backon::BackoffBuilder;
use futures_util::stream::{FuturesUnordered, StreamExt};
use metrics::{Counter, Gauge};
use operation_context::{Deadline, OperationContext, Stopped};
use parking_lot::Mutex;
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, SemaphorePermit, mpsc};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::ServerEndpoint;
use crate::credentials::{PasswordFile, ReadFailure};
use crate::observe::{
    ADMISSION_REFUSED_METRIC, COMMANDS_IN_FLIGHT_METRIC, PASSWORD_REFRESHES_METRIC,
    RETIREMENTS_METRIC,
};

pub(crate) const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTED_PROBE_MIN_BUDGET: Duration = Duration::from_secs(5);
const PASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_BACKOFF_MIN_DELAY: Duration = Duration::from_millis(100);
const SETUP_BACKOFF_MAX_DELAY: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_BASE_CAP: Duration = Duration::from_secs(1);
const MIN_GENERATION_SPACING: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_FACTOR: f32 = 2.0;
pub(crate) const SETUP_BACKOFF_MAX_RETRIES: usize = 6;
const PING_INTERVAL: Duration = Duration::from_secs(2);
const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const APPLICATION_SLOTS: usize = 256;
const PROBE_SLOTS: usize = 1;
const CALLER_PIPELINE_CAPACITY: usize = APPLICATION_SLOTS + PROBE_SLOTS;
const PIPELINE_BUFFER_SIZE: usize = CALLER_PIPELINE_CAPACITY + 1;

#[cfg(test)]
#[path = "tests/full_pipeline.rs"]
mod full_pipeline_tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
pub(crate) enum ErrorType {
    Timeout,
    Io,
    Auth,
    ServerErrorReply,
    Parse,
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

pub(crate) fn error_type(error: &redis::RedisError) -> ErrorType {
    if error.is_timeout() {
        return ErrorType::Timeout;
    }
    if matches!(error.code(), Some("WRONGPASS" | "NOAUTH")) {
        return ErrorType::Auth;
    }
    match error.kind() {
        redis::ErrorKind::Io => ErrorType::Io,
        redis::ErrorKind::AuthenticationFailed => ErrorType::Auth,
        redis::ErrorKind::Server(_) | redis::ErrorKind::Extension => ErrorType::ServerErrorReply,
        redis::ErrorKind::Parse => ErrorType::Parse,
        _ => ErrorType::Other,
    }
}

pub(crate) struct Link {
    pub(crate) server: ServerEndpoint,
    pub(crate) command_timeout: Duration,
    shared: Arc<Shared>,
    application_slots: Semaphore,
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
    state: ArcSwap<State>,
    write: Mutex<()>,
    changed: Notify,
    retirements: Counter,
}

enum State {
    Connecting,
    Connected(Arc<Generation>),
    SetupFailed(ErrorType),
    Closed,
}

struct Generation {
    connection: MultiplexedConnection,
    caller_capacity: Arc<Semaphore>,
    abandoned: mpsc::UnboundedSender<AbandonedReply>,
    retired: CancellationToken,
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

pub(crate) struct CommandScope {
    pub(crate) context: OperationContext,
    ceiling: Deadline,
}

impl CommandScope {
    pub(crate) fn under(parent: &OperationContext, command_timeout: Duration) -> Self {
        let ceiling = Deadline::new(Instant::now(), command_timeout);
        Self {
            context: parent.child_until(ceiling),
            ceiling,
        }
    }

    pub(crate) fn standalone(command_timeout: Duration) -> Self {
        let ceiling = Deadline::new(Instant::now(), command_timeout);
        Self {
            context: OperationContext::from_deadline(ceiling),
            ceiling,
        }
    }
}

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
            .field("command_timeout", &self.command_timeout)
            .finish_non_exhaustive()
    }
}

impl Shared {
    fn close(&self) {
        let write = self.write.lock();
        let previous = self.state.swap(Arc::new(State::Closed));
        if let State::Connected(current) = &*previous {
            current.retired.cancel();
            self.retirements.increment(1);
        }
        drop(write);
        self.changed.notify_waiters();
    }

    fn retire(&self, generation: &Arc<Generation>) {
        let write = self.write.lock();
        generation.retired.cancel();
        if matches!(&**self.state.load(), State::Connected(current) if Arc::ptr_eq(current, generation))
        {
            self.state.store(Arc::new(State::Connecting));
            self.retirements.increment(1);
        }
        drop(write);
        self.changed.notify_waiters();
    }

    #[must_use]
    fn begin_attempt(&self) -> bool {
        self.transition(State::Connecting)
    }

    #[must_use]
    fn publish(&self, generation: Arc<Generation>) -> bool {
        self.transition(State::Connected(generation))
    }

    #[must_use]
    fn record_failure(&self, error: ErrorType) -> bool {
        self.transition(State::SetupFailed(error))
    }

    fn transition(&self, next: State) -> bool {
        let write = self.write.lock();
        if matches!(&**self.state.load(), State::Closed) {
            return false;
        }
        self.state.store(Arc::new(next));
        drop(write);
        self.changed.notify_waiters();
        true
    }

    async fn acquire(&self) -> Result<Arc<Generation>, ErrorType> {
        if let Some(settled) = self.settled() {
            return settled;
        }
        loop {
            let changed = self.changed.notified();
            if let Some(settled) = self.settled() {
                return settled;
            }
            changed.await;
        }
    }

    fn settled(&self) -> Option<Result<Arc<Generation>, ErrorType>> {
        match &**self.state.load() {
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
            state: ArcSwap::from_pointee(State::Connecting),
            write: Mutex::new(()),
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
            command_timeout,
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
        let generation = tokio::select! {
            biased;
            result = self.shared.acquire() => result,
            stopped = context.wait_stopped() => return Err(CommandError::Stopped(stopped)),
        };
        context.check().map_err(CommandError::Stopped)?;
        let generation = generation?;
        let exchange = async {
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
        tokio::select! {
            biased;
            reply = exchange => reply,
            stopped = context.wait_stopped() => Err(CommandError::Stopped(stopped)),
            () = generation.retired.cancelled() => Err(ErrorType::Io.into()),
        }
    }

    pub(crate) async fn probe(&self) -> Result<(), ErrorType> {
        let _slot = self.claim_slot(&self.probe_slots)?;
        let generation = self.shared.acquire().await?;
        let budget = self.command_timeout.max(CONNECTED_PROBE_MIN_BUDGET);
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

    fn claim_slot<'a>(
        &'a self,
        slots: &'a Semaphore,
    ) -> Result<
        std::mem::DropGuard<SemaphorePermit<'a>, impl FnOnce(SemaphorePermit<'a>) + 'a>,
        ErrorType,
    > {
        let permit = slots.try_acquire().map_err(|_| {
            self.admission_refused.increment(1);
            ErrorType::Other
        })?;
        self.commands_in_flight.increment(1.0);
        Ok(std::mem::DropGuard::new(permit, |_permit| {
            self.commands_in_flight.decrement(1.0);
        }))
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
    metrics.admission_refused.increment(0);
    metrics.commands_in_flight.increment(0.0);
    metrics.retirements.increment(0);
    metrics
}

struct SupervisorExit {
    shared: Arc<Shared>,
    server: ServerEndpoint,
}

impl Drop for SupervisorExit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                server.address = %self.server.host,
                server.port = self.server.port,
                "cache_supervisor_panicked"
            );
        }
        self.shared.close();
    }
}

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
    let mut backoff = SetupBackoff::new();
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
                backoff = SetupBackoff::new();
                let mut drain = std::pin::pin!(drain_abandoned_replies(replies));
                let _retire =
                    std::mem::DropGuard::new(&generation, |generation| shared.retire(generation));
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
                next_attempt = Instant::now() + backoff.next_delay();
            }
        }
    }
}

pub(crate) struct SetupBackoff {
    chain: <backon::ExponentialBuilder as BackoffBuilder>::Backoff,
}

impl SetupBackoff {
    pub(crate) fn new() -> Self {
        Self {
            chain: Self::policy(SETUP_BACKOFF_MIN_DELAY)
                .with_max_times(SETUP_BACKOFF_MAX_RETRIES)
                .build(),
        }
    }

    fn policy(min_delay: Duration) -> backon::ExponentialBuilder {
        backon::ExponentialBuilder::default()
            .with_min_delay(min_delay)
            .with_max_delay(SETUP_BACKOFF_BASE_CAP)
            .with_factor(SETUP_BACKOFF_FACTOR)
            .with_jitter()
    }

    pub(crate) fn next_delay(&mut self) -> Duration {
        let delay = self.chain.next().unwrap_or_else(|| {
            *self = Self::new();
            Self::policy(SETUP_BACKOFF_BASE_CAP)
                .build()
                .next()
                .unwrap_or(SETUP_BACKOFF_MAX_DELAY)
        });
        delay.min(SETUP_BACKOFF_MAX_DELAY)
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

async fn credentialed_client<'a>(
    server: &ServerEndpoint,
    admitted: &redis::Client,
    password_file: Option<&'a PasswordFile>,
) -> Result<(redis::Client, Option<Rotation<'a>>), ErrorType> {
    let Some(file) = password_file else {
        return Ok((admitted.clone(), None));
    };
    let password = file.read().await.map_err(|error| {
        warn_password_file_unreadable(server, error);
        ErrorType::Other
    })?;
    let info = admitted.get_connection_info().clone();
    let settings = info
        .redis_settings()
        .clone()
        .set_username(file.username())
        .set_password(password.as_str());
    let client =
        redis::Client::open(info.set_redis_settings(settings)).map_err(|_| ErrorType::Other)?;
    Ok((client, Some(Rotation::new(file, password))))
}

fn warn_password_file_unreadable(server: &ServerEndpoint, error: ReadFailure) {
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
            () = sleep_until(next_refresh), if rotation.is_some() && next_refresh <= next_ping => {
                next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
                if let Some(rotation) = rotation.as_mut()
                    && rotation.refresh_and_record(server, generation, command_timeout).await
                        == Disposition::Retire
                {
                    return;
                }
            }
            () = sleep_until(next_ping) => {
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

struct Rotation<'a> {
    file: &'a PasswordFile,
    authenticated_password: Zeroizing<String>,
    unreadable_logged: bool,
}

enum RefreshOutcome {
    ReadFailed(ReadFailure),
    Unchanged,
    AuthAccepted,
    AuthRefused,
    Failed(ErrorType),
}

impl RefreshOutcome {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Disposition {
    Keep,
    Retire,
}

impl<'a> Rotation<'a> {
    fn new(file: &'a PasswordFile, authenticated_password: Zeroizing<String>) -> Self {
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
        let read_deadline = Instant::now() + PASSWORD_READ_TIMEOUT;
        let password = match timeout_at(read_deadline, self.file.read()).await {
            Ok(Ok(password)) => password,
            Ok(Err(error)) => return RefreshOutcome::ReadFailed(error),
            Err(_elapsed) => return RefreshOutcome::ReadFailed(ReadFailure::TimedOut),
        };
        if password.as_str() == self.authenticated_password.as_str() {
            return RefreshOutcome::Unchanged;
        }
        let mut command = redis::cmd("AUTH");
        command.arg(self.file.username()).arg(password.as_str());
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
                    warn_password_file_unreadable(server, *error);
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
