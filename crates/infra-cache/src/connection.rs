use std::future::poll_fn;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use arc_swap::ArcSwap;
use backon::BackoffBuilder;
use futures_util::stream::StreamExt;
use metrics::{Counter, Gauge};
use operation_context::{Deadline, OperationContext, Stopped};
use parking_lot::Mutex;
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::credentials::{PasswordFile, ReadFailure};
use crate::dial::{Dial, Dialed};
use crate::namespace::Operation;
use crate::observe::{
    ADMISSION_REFUSED_METRIC, COMMANDS_IN_FLIGHT_METRIC, PASSWORD_REFRESHES_METRIC,
    RETIREMENTS_METRIC,
};
use crate::{MutationError, OperationError, ServerEndpoint, SetError};

const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_BACKOFF_MIN_DELAY: Duration = Duration::from_millis(100);
const SETUP_BACKOFF_MAX_DELAY: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_BASE_CAP: Duration = Duration::from_secs(1);
const MIN_GENERATION_SPACING: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_FACTOR: f32 = 2.0;
const PING_INTERVAL: Duration = Duration::from_secs(2);
const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const APPLICATION_SLOTS: usize = 256;
const PIPELINE_BUFFER_SIZE: usize = APPLICATION_SLOTS + 1;

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

#[derive(Clone, Copy)]
pub(crate) enum CommandFailure {
    Stopped(Stopped),
    Failed(ErrorType),
    InvalidTtl,
}

impl CommandFailure {
    fn operation_error(self) -> OperationError {
        match self {
            Self::Stopped(stopped) => OperationError::Stopped(stopped),
            Self::Failed(_) | Self::InvalidTtl => OperationError::Unavailable,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CommandError {
    NotApplied(CommandFailure),
    OutcomeUnknown(CommandFailure),
}

impl CommandError {
    pub(crate) fn failure(self) -> CommandFailure {
        match self {
            Self::NotApplied(failure) | Self::OutcomeUnknown(failure) => failure,
        }
    }

    fn from_reply(
        error: &redis::RedisError,
        operation: Operation,
        stopped: Option<CommandFailure>,
    ) -> Self {
        if matches!(operation, Operation::Set)
            && error.code() == Some("ERR")
            && error.detail().is_some_and(|detail| {
                detail.eq_ignore_ascii_case("invalid expire time in 'set' command")
            })
        {
            return Self::NotApplied(CommandFailure::InvalidTtl);
        }
        let failure = stopped.unwrap_or_else(|| CommandFailure::Failed(error_type(error)));
        match error.code() {
            Some(
                "READONLY" | "NOAUTH" | "NOPERM" | "OOM" | "LOADING" | "MOVED" | "ASK"
                | "CLUSTERDOWN" | "MASTERDOWN" | "MISCONF" | "NOREPLICAS" | "BUSY" | "REDIRECT",
            ) => Self::NotApplied(failure),
            _ => Self::OutcomeUnknown(failure),
        }
    }
}

impl From<CommandError> for OperationError {
    fn from(error: CommandError) -> Self {
        error.failure().operation_error()
    }
}

impl From<CommandError> for MutationError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(failure) => Self::NotApplied(failure.operation_error()),
            CommandError::OutcomeUnknown(failure) => {
                Self::OutcomeUnknown(failure.operation_error())
            }
        }
    }
}

impl From<CommandError> for SetError {
    fn from(error: CommandError) -> Self {
        match error {
            CommandError::NotApplied(CommandFailure::InvalidTtl) => Self::InvalidTtl,
            _ => Self::Mutation(error.into()),
        }
    }
}

impl From<ErrorType> for CommandError {
    fn from(error: ErrorType) -> Self {
        Self::NotApplied(CommandFailure::Failed(error))
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
    cooldown_until: Instant,
}

impl Generation {
    fn cooldown_error(&self) -> Option<ErrorType> {
        (Instant::now() < self.cooldown_until).then_some(ErrorType::Io)
    }

    async fn reserve(&self) -> Result<OwnedSemaphorePermit, ErrorType> {
        let reservation = self
            .caller_capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ErrorType::Io)?;
        tokio::task::consume_budget().await;
        Ok(reservation)
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

type QueryFuture<T> = Pin<Box<dyn Future<Output = redis::RedisResult<T>> + Send>>;

struct Exchange<'a, T: Send + 'static> {
    generation: &'a Generation,
    reservation: Option<OwnedSemaphorePermit>,
    reply: Option<QueryFuture<T>>,
    wire_deadline: Deadline,
}

impl<'a, T: Send + 'static> Exchange<'a, T> {
    fn new(
        generation: &'a Generation,
        reservation: OwnedSemaphorePermit,
        reply: impl Future<Output = redis::RedisResult<T>> + Send + 'static,
        wire_deadline: Deadline,
    ) -> Self {
        Self {
            generation,
            reservation: Some(reservation),
            reply: Some(Box::pin(reply)),
            wire_deadline,
        }
    }

    fn poll_reply(&mut self, cx: &mut Context<'_>) -> Poll<redis::RedisResult<T>> {
        let Some(mut reply) = self.reply.take() else {
            return Poll::Pending;
        };
        match reply.as_mut().poll(cx) {
            Poll::Ready(result) => {
                if !command_keeps_generation(&result) {
                    self.generation.retired.cancel();
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                self.reply = Some(reply);
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

impl<T: Send + 'static> Drop for Exchange<'_, T> {
    fn drop(&mut self) {
        let (Some(reply), Some(reservation)) = (self.reply.take(), self.reservation.take()) else {
            return;
        };
        let wire_deadline = self.wire_deadline;
        let retired = self.generation.retired.clone();
        let _ = self.generation.abandoned.send(AbandonedReply {
            reservation,
            reply: Box::pin(async move {
                tokio::select! {
                    biased;
                    result = reply => {
                        if !command_keeps_generation(&result) {
                            retired.cancel();
                        }
                    }
                    () = wire_deadline.wait() => {
                        retired.cancel();
                    }
                }
            }),
        });
    }
}

async fn query<T: redis::FromRedisValue>(
    command: redis::Cmd,
    mut connection: MultiplexedConnection,
    operation: Operation,
) -> redis::RedisResult<T> {
    let reply = tokio::task::unconstrained(connection.send_packed_command(&command)).await?;
    match (operation, reply_data(reply)?) {
        (Operation::Get, value @ (redis::Value::Nil | redis::Value::BulkString(_)))
        | (Operation::Set, value @ redis::Value::Okay)
        | (Operation::Delete, value @ redis::Value::Int(0 | 1)) => Ok(T::from_redis_value(value)?),
        _ => Err((redis::ErrorKind::Parse, "unexpected cache command reply").into()),
    }
}

fn reply_data(mut reply: redis::Value) -> redis::RedisResult<redis::Value> {
    while let redis::Value::Attribute { data, .. } = reply {
        reply = *data;
    }
    match reply {
        redis::Value::ServerError(error) => Err(error.into()),
        data => Ok(data),
    }
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

    fn is_closed(&self) -> bool {
        matches!(&**self.state.load(), State::Closed)
    }

    fn retire(&self, generation: &Arc<Generation>) {
        let write = self.write.lock();
        generation.retired.cancel();
        if matches!(&**self.state.load(), State::Connected(current) if Arc::ptr_eq(current, generation))
        {
            let next = generation
                .cooldown_error()
                .map_or(State::Connecting, State::SetupFailed);
            self.state.store(Arc::new(next));
            self.retirements.increment(1);
        }
        drop(write);
        self.changed.notify_waiters();
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
            State::Connected(current) => {
                if current.retired.is_cancelled() {
                    current.cooldown_error().map(Err)
                } else {
                    Some(Ok(current.clone()))
                }
            }
            State::SetupFailed(error) => Some(Err(*error)),
            State::Connecting => None,
        }
    }
}

impl Link {
    pub(crate) fn start(
        server: ServerEndpoint,
        dial: Dial,
        settings: redis::RedisConnectionInfo,
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
        let supervisor = tokio::spawn(supervise(
            exit,
            dial,
            settings,
            password_file,
            command_timeout,
        ))
        .abort_handle();
        Self {
            server,
            command_timeout,
            shared,
            application_slots: Semaphore::new(APPLICATION_SLOTS),
            admission_refused,
            commands_in_flight,
            supervisor,
        }
    }

    pub(crate) async fn command<T: redis::FromRedisValue + Send + 'static>(
        &self,
        build_command: impl FnOnce() -> redis::Cmd,
        parent: &OperationContext,
        operation: Operation,
    ) -> Result<T, CommandError> {
        let context = parent.child(self.command_timeout);
        let stop = |_: Stopped| {
            parent.stopped().map_or(
                CommandFailure::Failed(ErrorType::Timeout),
                CommandFailure::Stopped,
            )
        };
        let check = || {
            context
                .check()
                .map_err(|stopped| CommandError::NotApplied(stop(stopped)))
        };
        check()?;
        let _slot = self.claim_slot()?;
        let generation = tokio::select! {
            biased;
            result = self.shared.acquire() => result,
            stopped = context.wait_stopped() => return Err(CommandError::NotApplied(stop(stopped))),
        };
        check()?;
        let generation = generation?;
        let reservation = tokio::select! {
            biased;
            result = generation.reserve() => result?,
            stopped = context.wait_stopped() => return Err(CommandError::NotApplied(stop(stopped))),
            () = generation.retired.cancelled() => return Err(ErrorType::Io.into()),
        };
        let check_dispatch = || -> Result<(), CommandError> {
            check()?;
            if generation.retired.is_cancelled() {
                return Err(ErrorType::Io.into());
            }
            Ok(())
        };
        check_dispatch()?;
        let command = build_command();
        check_dispatch()?;
        let mut exchange = Exchange::new(
            &generation,
            reservation,
            query(command, generation.connection.clone(), operation),
            Deadline::new(Instant::now(), self.command_timeout),
        );
        let reply = tokio::select! {
            biased;
            reply = poll_fn(|cx| exchange.poll_reply(cx)) => reply,
            stopped = context.wait_stopped() => return Err(CommandError::OutcomeUnknown(stop(stopped))),
            () = generation.retired.cancelled() => return Err(CommandError::OutcomeUnknown(CommandFailure::Failed(ErrorType::Io))),
        };
        let value = reply.map_err(|error| {
            CommandError::from_reply(&error, operation, context.stopped().map(stop))
        })?;
        if matches!(operation, Operation::Get) {
            check()?;
        }
        Ok(value)
    }

    fn claim_slot(&self) -> Result<impl Drop + '_, ErrorType> {
        let permit = self.application_slots.try_acquire().map_err(|_| {
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
    accepts: fn(redis::Value) -> bool,
    deadline: impl FnOnce() -> Deadline,
) -> Result<redis::RedisResult<()>, ErrorType> {
    tokio::task::consume_budget().await;
    let mut connection = generation.connection.clone();
    let reply = tokio::task::unconstrained(connection.send_packed_command(command));
    let result = reply_until(&generation.retired, reply, deadline).await?;
    Ok(result.and_then(reply_data).and_then(|value| {
        if accepts(value) {
            Ok(())
        } else {
            Err((
                redis::ErrorKind::Parse,
                "unexpected cache maintenance reply",
            )
                .into())
        }
    }))
}

async fn ping_until(
    generation: &Generation,
    command_timeout: Duration,
) -> Result<(), (ErrorType, Disposition)> {
    let reply = query_until(
        generation,
        &redis::Cmd::ping(),
        |value| matches!(value, redis::Value::SimpleString(pong) if pong == "PONG"),
        || Deadline::new(Instant::now(), command_timeout),
    )
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
    dial: Dial,
    settings: redis::RedisConnectionInfo,
    password_file: Option<PasswordFile>,
    command_timeout: Duration,
) {
    run_supervisor(
        &exit.shared,
        &exit.server,
        &dial,
        &settings,
        password_file.as_ref(),
        command_timeout,
    )
    .await;
}

async fn run_supervisor(
    shared: &Shared,
    server: &ServerEndpoint,
    dial: &Dial,
    settings: &redis::RedisConnectionInfo,
    password_file: Option<&PasswordFile>,
    command_timeout: Duration,
) {
    let mut backoff = setup_backoff();
    let mut next_attempt = Instant::now();
    loop {
        sleep_until(next_attempt).await;
        if shared.is_closed() {
            return;
        }
        let retired = CancellationToken::new();
        match setup_attempt(server, dial, settings, password_file, &retired).await {
            Ok((dialed, rotation)) => {
                let _driver = dialed.driver;
                let (abandoned, replies) = mpsc::unbounded_channel();
                let generation = Arc::new(Generation {
                    connection: dialed.connection,
                    caller_capacity: Arc::new(Semaphore::new(APPLICATION_SLOTS)),
                    abandoned,
                    retired,
                    cooldown_until: Instant::now() + MIN_GENERATION_SPACING,
                });
                if !shared.publish(generation.clone()) {
                    return;
                }
                tracing::info!(
                    server.address = %server.host,
                    server.port = server.port,
                    cache.tls = server.tls,
                    "cache_connected"
                );
                next_attempt = generation.cooldown_until;
                backoff = setup_backoff();
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
                let delay = backoff
                    .next()
                    .unwrap_or(SETUP_BACKOFF_MAX_DELAY)
                    .min(SETUP_BACKOFF_MAX_DELAY);
                next_attempt = Instant::now() + delay;
            }
        }
    }
}

pub(crate) fn setup_backoff() -> <backon::ExponentialBuilder as BackoffBuilder>::Backoff {
    backon::ExponentialBuilder::default()
        .with_min_delay(SETUP_BACKOFF_MIN_DELAY)
        .with_max_delay(SETUP_BACKOFF_BASE_CAP)
        .with_factor(SETUP_BACKOFF_FACTOR)
        .with_jitter()
        .without_max_times()
        .build()
}

async fn drain_abandoned_replies(mut replies: mpsc::UnboundedReceiver<AbandonedReply>) {
    futures_util::stream::poll_fn(|cx| replies.poll_recv(cx))
        .for_each_concurrent(None, AbandonedReply::wait)
        .await;
}

async fn setup_attempt<'a>(
    server: &ServerEndpoint,
    dial: &Dial,
    settings: &redis::RedisConnectionInfo,
    password_file: Option<&'a PasswordFile>,
    retired: &CancellationToken,
) -> Result<(Dialed, Option<Rotation<'a>>), ErrorType> {
    let setup = async {
        let (settings, rotation) = credentialed_settings(server, settings, password_file).await?;
        let dialed = connect(dial, &settings, retired.clone()).await?;
        Ok((dialed, rotation))
    };
    match timeout(SETUP_TIMEOUT, setup).await {
        Err(_elapsed) => Err(ErrorType::Timeout),
        Ok(Ok(_)) if retired.is_cancelled() => Err(ErrorType::Io),
        Ok(connected) => connected,
    }
}

async fn credentialed_settings<'a>(
    server: &ServerEndpoint,
    admitted: &redis::RedisConnectionInfo,
    password_file: Option<&'a PasswordFile>,
) -> Result<(redis::RedisConnectionInfo, Option<Rotation<'a>>), ErrorType> {
    let Some(file) = password_file else {
        return Ok((admitted.clone(), None));
    };
    let password = file.read().await.map_err(|error| {
        warn_password_file_unreadable(server, error);
        ErrorType::Other
    })?;
    let settings = admitted
        .clone()
        .set_username(file.username())
        .set_password(password.as_str());
    Ok((settings, Some(Rotation::new(file, password))))
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
    dial: &Dial,
    settings: &redis::RedisConnectionInfo,
    retired: CancellationToken,
) -> Result<Dialed, ErrorType> {
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
    dial.connect(settings, config)
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
    let mut ping_refusal_logged = false;
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
                let outcome = ping_until(generation, command_timeout).await;
                let repeated_refusal =
                    std::mem::replace(&mut ping_refusal_logged, outcome.is_err());
                if let Err((error, disposition)) = outcome {
                    if disposition == Disposition::Retire || !repeated_refusal {
                        tracing::warn!(
                            server.address = %server.host,
                            server.port = server.port,
                            error.type = error.label(),
                            "cache_ping_failed"
                        );
                    }
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
    refused_logged: bool,
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
            refused_logged: false,
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
        let result = query_until(
            generation,
            &command,
            |value| matches!(value, redis::Value::Okay),
            || Deadline::new(Instant::now(), command_timeout),
        )
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
        let refused = matches!(outcome, RefreshOutcome::AuthRefused);
        let refused_logged = std::mem::replace(&mut self.refused_logged, refused);
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
            RefreshOutcome::AuthRefused if refused_logged => {}
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
