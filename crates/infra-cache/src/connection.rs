use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use arc_swap::ArcSwap;
use backon::BackoffBuilder;
use futures_util::future::BoxFuture;
use futures_util::stream::StreamExt;
use metrics::{Counter, Gauge};
use operation_context::{Deadline, OperationContext};
use parking_lot::Mutex;
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use zeroize::Zeroizing;

use crate::admission::Admitted;
use crate::credentials::{PasswordFile, ReadFailure};
use crate::dial::Dialed;
use crate::error::{CommandError, CommandFailure, ErrorType, error_type};
use crate::namespace::Operation;
use crate::observe::{
    ADMISSION_REFUSED_METRIC, COMMANDS_IN_FLIGHT_METRIC, PASSWORD_REFRESHES_METRIC,
    RETIREMENTS_METRIC,
};
use crate::{CloseOutcome, ServerEndpoint};

const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const PASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_BACKOFF_MIN_DELAY: Duration = Duration::from_millis(100);
const SETUP_BACKOFF_MAX_DELAY: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_BASE_CAP: Duration = Duration::from_secs(1);
const MIN_GENERATION_SPACING: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_FACTOR: f32 = 2.0;
const PING_INTERVAL: Duration = Duration::from_secs(2);
const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(test)]
#[path = "tests/full_pipeline.rs"]
mod full_pipeline_tests;

#[derive(derive_more::Debug)]
pub(crate) struct Link {
    pub(crate) server: ServerEndpoint,
    pub(crate) command_timeout: Duration,
    #[debug(skip)]
    shared: Arc<Shared>,
    #[debug(skip)]
    application_slots: Semaphore,
    #[debug(skip)]
    admission_refused: Counter,
    #[debug(skip)]
    commands_in_flight: Gauge,
    #[debug(skip)]
    supervisor: AbortHandle,
}

struct LinkMetrics {
    admission_refused: Counter,
    commands_in_flight: Gauge,
    retirements: Counter,
}

struct Shared {
    max_concurrency: usize,
    state: ArcSwap<State>,
    state_lock: Mutex<()>,
    changed: Notify,
    retirements: Counter,
    tasks: TaskTracker,
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
    reply: BoxFuture<'static, ()>,
}

impl AbandonedReply {
    async fn wait(self) {
        let _reservation = self.reservation;
        self.reply.await;
    }
}

type QueryFuture<T> = BoxFuture<'static, redis::RedisResult<T>>;

struct InFlight<T> {
    reply: QueryFuture<T>,
    reservation: OwnedSemaphorePermit,
}

struct Exchange<'a, T: Send + 'static> {
    generation: &'a Generation,
    in_flight: Option<InFlight<T>>,
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
            in_flight: Some(InFlight {
                reply: Box::pin(reply),
                reservation,
            }),
            wire_deadline,
        }
    }
}

impl<T: Send + 'static> Future for Exchange<'_, T> {
    type Output = redis::RedisResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(mut in_flight) = self.in_flight.take() else {
            return Poll::Pending;
        };
        match in_flight.reply.as_mut().poll(cx) {
            Poll::Ready(result) => {
                if reply_requires_retirement(&result) {
                    self.generation.retired.cancel();
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                self.in_flight = Some(in_flight);
                Poll::Pending
            }
        }
    }
}

fn reply_requires_retirement<T>(reply: &redis::RedisResult<T>) -> bool {
    reply.as_ref().is_err_and(|error| {
        error_type(error) != ErrorType::ServerErrorReply || error.code() == Some("READONLY")
    })
}

impl<T: Send + 'static> Drop for Exchange<'_, T> {
    fn drop(&mut self) {
        let Some(InFlight { reply, reservation }) = self.in_flight.take() else {
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
                        if reply_requires_retirement(&result) {
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

pub(super) fn reply_data(mut reply: redis::Value) -> redis::RedisResult<redis::Value> {
    while let redis::Value::Attribute { data, .. } = reply {
        reply = *data;
    }
    match reply {
        redis::Value::ServerError(error) => Err(error.into()),
        data => Ok(data),
    }
}

impl Shared {
    fn close(&self) {
        let state_guard = self.state_lock.lock();
        let previous = self.state.swap(Arc::new(State::Closed));
        if let State::Connected(current) = &*previous {
            current.retired.cancel();
            self.retirements.increment(1);
        }
        drop(state_guard);
        self.changed.notify_waiters();
    }

    fn is_closed(&self) -> bool {
        matches!(&**self.state.load(), State::Closed)
    }

    fn retire(&self, generation: &Arc<Generation>) {
        let state_guard = self.state_lock.lock();
        generation.retired.cancel();
        if matches!(&**self.state.load(), State::Connected(current) if Arc::ptr_eq(current, generation))
        {
            let next = generation
                .cooldown_error()
                .map_or(State::Connecting, State::SetupFailed);
            self.state.store(Arc::new(next));
            self.retirements.increment(1);
        }
        drop(state_guard);
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
        let state_guard = self.state_lock.lock();
        if matches!(&**self.state.load(), State::Closed) {
            return false;
        }
        self.state.store(Arc::new(next));
        drop(state_guard);
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
        admitted: Admitted,
        command_timeout: Duration,
        max_concurrency: usize,
    ) -> Self {
        let server = admitted.server.clone();
        let LinkMetrics {
            admission_refused,
            commands_in_flight,
            retirements,
        } = register_link_metrics();
        let shared = Arc::new(Shared {
            max_concurrency,
            state: ArcSwap::from_pointee(State::Connecting),
            state_lock: Mutex::new(()),
            changed: Notify::new(),
            retirements,
            tasks: TaskTracker::new(),
        });
        let exit = SupervisorExit {
            shared: shared.clone(),
            admitted,
        };
        let supervisor = shared
            .tasks
            .spawn(supervise(exit, command_timeout))
            .abort_handle();
        Self {
            server,
            command_timeout,
            shared,
            application_slots: Semaphore::new(max_concurrency),
            admission_refused,
            commands_in_flight,
            supervisor,
        }
    }

    pub(crate) async fn close(&self, deadline: Instant) -> CloseOutcome {
        self.stop();
        if timeout_at(deadline, self.shared.tasks.wait()).await.is_ok() {
            CloseOutcome::Complete
        } else {
            CloseOutcome::TimedOut
        }
    }

    fn stop(&self) {
        self.shared.close();
        self.supervisor.abort();
        self.shared.tasks.close();
    }

    pub(crate) async fn command<T: Send + 'static, F>(
        &self,
        build_query: impl FnOnce(MultiplexedConnection) -> F,
        parent: &OperationContext,
        operation: Operation,
    ) -> Result<T, CommandError>
    where
        F: Future<Output = redis::RedisResult<T>> + Send + 'static,
    {
        let budget = CommandBudget::new(parent, self.command_timeout);
        budget.check_not_stopped()?;
        let _slot = self.claim_slot()?;
        let (generation, reservation) = self.reserve_generation(&budget).await?;
        let query = build_query(generation.connection.clone());
        budget.check_before_dispatch(&generation)?;
        let mut exchange = Exchange::new(
            &generation,
            reservation,
            query,
            Deadline::after(self.command_timeout),
        );
        let reply = tokio::select! {
            biased;
            reply = &mut exchange => reply,
            () = budget.wait_stopped() => {
                return Err(CommandError::OutcomeUnknown(budget.classify_stop()));
            }
            () = generation.retired.cancelled() => {
                return Err(CommandError::OutcomeUnknown(CommandFailure::Failed(ErrorType::Io)));
            }
        };
        let value = reply.map_err(|error| {
            CommandError::from_reply(&error, operation, budget.stopped_failure())
        })?;
        if matches!(operation, Operation::Get) {
            budget.check_not_stopped()?;
        }
        Ok(value)
    }

    async fn reserve_generation(
        &self,
        budget: &CommandBudget<'_>,
    ) -> Result<(Arc<Generation>, OwnedSemaphorePermit), CommandError> {
        let generation = tokio::select! {
            biased;
            result = self.shared.acquire() => result,
            () = budget.wait_stopped() => {
                return Err(CommandError::NotApplied(budget.classify_stop()));
            }
        };
        budget.check_not_stopped()?;
        let generation = generation?;
        let reservation = tokio::select! {
            biased;
            result = generation.reserve() => result?,
            () = budget.wait_stopped() => {
                return Err(CommandError::NotApplied(budget.classify_stop()));
            }
            () = async { generation.retired.cancelled().await } => return Err(ErrorType::Io.into()),
        };
        budget.check_before_dispatch(&generation)?;
        Ok((generation, reservation))
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

struct CommandBudget<'a> {
    context: OperationContext,
    parent: &'a OperationContext,
}

impl<'a> CommandBudget<'a> {
    fn new(parent: &'a OperationContext, command_timeout: Duration) -> Self {
        Self {
            context: parent.child(command_timeout),
            parent,
        }
    }

    fn classify_stop(&self) -> CommandFailure {
        self.parent.stopped().map_or(
            CommandFailure::Failed(ErrorType::Timeout),
            CommandFailure::Stopped,
        )
    }

    fn stopped_failure(&self) -> Option<CommandFailure> {
        self.context.stopped().map(|_| self.classify_stop())
    }

    fn check_not_stopped(&self) -> Result<(), CommandError> {
        self.context
            .check()
            .map_err(|_| CommandError::NotApplied(self.classify_stop()))
    }

    fn check_before_dispatch(&self, generation: &Generation) -> Result<(), CommandError> {
        self.check_not_stopped()?;
        if generation.retired.is_cancelled() {
            return Err(ErrorType::Io.into());
        }
        Ok(())
    }

    async fn wait_stopped(&self) {
        let _ = self.context.wait_stopped().await;
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop();
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
    admitted: Admitted,
}

impl Drop for SupervisorExit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            tracing::error!(
                server.address = %self.admitted.server.host,
                server.port = self.admitted.server.port,
                "cache_supervisor_panicked"
            );
        }
        self.shared.close();
    }
}

enum MaintenanceError {
    Aborted(ErrorType),
    Redis(redis::RedisError),
}

impl MaintenanceError {
    fn error_type(&self) -> ErrorType {
        match self {
            Self::Aborted(error) => *error,
            Self::Redis(error) => error_type(error),
        }
    }
}

async fn maintenance_query(
    generation: &Generation,
    command: &redis::Cmd,
    accepts: fn(redis::Value) -> bool,
    command_timeout: Duration,
) -> Result<(), MaintenanceError> {
    tokio::task::consume_budget().await;
    let mut connection = generation.connection.clone();
    let reply = tokio::task::unconstrained(connection.send_packed_command(command));
    let result = tokio::select! {
        biased;
        () = generation.retired.cancelled() => return Err(MaintenanceError::Aborted(ErrorType::Io)),
        result = reply => result,
        () = async { Deadline::after(command_timeout).wait().await } => {
            return Err(MaintenanceError::Aborted(ErrorType::Timeout));
        }
    };
    let value = result
        .and_then(reply_data)
        .map_err(MaintenanceError::Redis)?;
    if accepts(value) {
        Ok(())
    } else {
        Err(MaintenanceError::Redis(
            (
                redis::ErrorKind::Parse,
                "unexpected cache maintenance reply",
            )
                .into(),
        ))
    }
}

#[derive(Debug, Eq, PartialEq)]
enum PingOutcome {
    Ok,
    Refused(ErrorType),
    Retire(ErrorType),
}

async fn ping_generation(generation: &Generation, command_timeout: Duration) -> PingOutcome {
    match maintenance_query(
        generation,
        &redis::Cmd::ping(),
        |value| matches!(value, redis::Value::SimpleString(pong) if pong == "PONG"),
        command_timeout,
    )
    .await
    {
        Ok(()) => PingOutcome::Ok,
        Err(MaintenanceError::Redis(redis_error)) if redis_error.code() == Some("NOPERM") => {
            PingOutcome::Refused(error_type(&redis_error))
        }
        Err(error) => PingOutcome::Retire(error.error_type()),
    }
}

async fn supervise(exit: SupervisorExit, command_timeout: Duration) {
    let shared = &exit.shared;
    let admitted = &exit.admitted;
    let server = &admitted.server;
    let mut backoff = setup_backoff();
    let mut next_attempt = Instant::now();
    loop {
        sleep_until(next_attempt).await;
        if shared.is_closed() {
            return;
        }
        let retired = CancellationToken::new();
        let (dialed, rotation) = match setup_attempt(admitted, &retired, shared).await {
            Ok(connected) => connected,
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
                continue;
            }
        };
        let Some(cooldown_until) =
            run_generation(shared, server, dialed, rotation, retired, command_timeout).await
        else {
            return;
        };
        next_attempt = cooldown_until;
        backoff = setup_backoff();
    }
}

async fn run_generation(
    shared: &Shared,
    server: &ServerEndpoint,
    dialed: Dialed,
    rotation: Option<Rotation<'_>>,
    retired: CancellationToken,
    command_timeout: Duration,
) -> Option<Instant> {
    let _driver = dialed.driver;
    let (abandoned, replies) = mpsc::unbounded_channel();
    let generation = Arc::new(Generation {
        connection: dialed.connection,
        caller_capacity: Arc::new(Semaphore::new(shared.max_concurrency)),
        abandoned,
        retired,
        cooldown_until: Instant::now() + MIN_GENERATION_SPACING,
    });
    if !shared.publish(generation.clone()) {
        return None;
    }
    tracing::info!(
        server.address = %server.host,
        server.port = server.port,
        cache.tls = server.tls,
        "cache_connected"
    );
    let mut drain = std::pin::pin!(drain_abandoned_replies(replies));
    let _retire = std::mem::DropGuard::new(&generation, |generation| shared.retire(generation));
    tokio::select! {
        biased;
        () = maintain(server, &generation, rotation, command_timeout) => {}
        () = &mut drain => {}
    }
    Some(generation.cooldown_until)
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
    admitted: &'a Admitted,
    retired: &CancellationToken,
    shared: &'a Shared,
) -> Result<(Dialed, Option<Rotation<'a>>), ErrorType> {
    let setup = async {
        let (settings, rotation) = credentialed_settings(admitted, &shared.tasks).await?;
        let dialed = connect(admitted, &settings, retired.clone(), shared).await?;
        Ok((dialed, rotation))
    };
    match timeout(SETUP_TIMEOUT, setup).await {
        Err(_elapsed) => Err(ErrorType::Timeout),
        Ok(Ok(_)) if retired.is_cancelled() => Err(ErrorType::Io),
        Ok(connected) => connected,
    }
}

async fn credentialed_settings<'a>(
    admitted: &'a Admitted,
    tasks: &'a TaskTracker,
) -> Result<(redis::RedisConnectionInfo, Option<Rotation<'a>>), ErrorType> {
    let Some(file) = admitted.password_file.as_ref() else {
        return Ok((admitted.settings.clone(), None));
    };
    let password = file.read(tasks).await.map_err(|error| {
        warn_password_file_unreadable(&admitted.server, error);
        ErrorType::Other
    })?;
    let settings = admitted
        .settings
        .clone()
        .set_username(file.username())
        .set_password(password.as_str());
    Ok((settings, Some(Rotation::new(file, tasks, password))))
}

fn warn_password_file_unreadable(server: &ServerEndpoint, error: ReadFailure) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        %error,
        "cache_password_file_unreadable"
    );
}

fn warn_ping_failed(server: &ServerEndpoint, error: ErrorType) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        error.type = error.label(),
        "cache_ping_failed"
    );
}

fn warn_password_refresh_failed(server: &ServerEndpoint, error: ErrorType) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        error.type = error.label(),
        "cache_password_refresh_failed"
    );
}

async fn connect(
    admitted: &Admitted,
    settings: &redis::RedisConnectionInfo,
    retired: CancellationToken,
    shared: &Shared,
) -> Result<Dialed, ErrorType> {
    let config = redis::AsyncConnectionConfig::new()
        .set_response_timeout(None)
        .set_connection_timeout(None)
        .set_pipeline_buffer_size(shared.max_concurrency + 1)
        .set_push_sender(move |push: redis::PushInfo| {
            if push.kind == redis::PushKind::Disconnection {
                retired.cancel();
            }
            Ok::<(), redis::aio::SendError>(())
        });
    admitted
        .server
        .connect(admitted.tls.as_ref(), settings, config, &shared.tasks)
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
                match ping_generation(generation, command_timeout).await {
                    PingOutcome::Ok => ping_refusal_logged = false,
                    PingOutcome::Refused(error) => {
                        if !std::mem::replace(&mut ping_refusal_logged, true) {
                            warn_ping_failed(server, error);
                        }
                    }
                    PingOutcome::Retire(error) => {
                        warn_ping_failed(server, error);
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
    tasks: &'a TaskTracker,
    authenticated_password: Zeroizing<String>,
    last_warning: Option<RefreshWarning>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefreshWarning {
    Unreadable,
    AuthRefused,
}

enum RefreshOutcome {
    ReadFailed(ReadFailure),
    Unchanged,
    AuthAccepted,
    AuthRefused,
    Failed(ErrorType),
}

impl RefreshOutcome {
    fn outcome_label(&self) -> &'static str {
        match self {
            Self::ReadFailed(_) => "read_failed",
            Self::Unchanged => "unchanged",
            Self::AuthAccepted => "auth_accepted",
            Self::AuthRefused | Self::Failed(_) => "refresh_failed",
        }
    }

    fn error_type(&self) -> Option<ErrorType> {
        match self {
            Self::AuthRefused => Some(ErrorType::Auth),
            Self::Failed(error) => Some(*error),
            Self::ReadFailed(_) | Self::Unchanged | Self::AuthAccepted => None,
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
    fn new(
        file: &'a PasswordFile,
        tasks: &'a TaskTracker,
        authenticated_password: Zeroizing<String>,
    ) -> Self {
        Self {
            file,
            tasks,
            authenticated_password,
            last_warning: None,
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
        let password = match timeout(PASSWORD_READ_TIMEOUT, self.file.read(self.tasks)).await {
            Ok(Ok(password)) => password,
            Ok(Err(error)) => return RefreshOutcome::ReadFailed(error),
            Err(_elapsed) => return RefreshOutcome::ReadFailed(ReadFailure::TimedOut),
        };
        if password.as_str() == self.authenticated_password.as_str() {
            return RefreshOutcome::Unchanged;
        }
        let mut command = redis::cmd("AUTH");
        command.arg(self.file.username()).arg(password.as_str());
        if let Err(error) = maintenance_query(
            generation,
            &command,
            |value| matches!(value, redis::Value::Okay),
            command_timeout,
        )
        .await
        {
            return match error.error_type() {
                ErrorType::Auth => RefreshOutcome::AuthRefused,
                error => RefreshOutcome::Failed(error),
            };
        }
        if generation.retired.is_cancelled() {
            return RefreshOutcome::Failed(ErrorType::Io);
        }
        self.authenticated_password = password;
        RefreshOutcome::AuthAccepted
    }

    fn record(&mut self, server: &ServerEndpoint, outcome: &RefreshOutcome) {
        let reason = outcome.error_type().map_or("none", ErrorType::label);
        metrics::counter!(
            PASSWORD_REFRESHES_METRIC,
            "outcome" => outcome.outcome_label(),
            "reason" => reason,
        )
        .increment(1);
        let warning = match outcome {
            RefreshOutcome::ReadFailed(_) => Some(RefreshWarning::Unreadable),
            RefreshOutcome::AuthRefused => Some(RefreshWarning::AuthRefused),
            RefreshOutcome::Unchanged
            | RefreshOutcome::AuthAccepted
            | RefreshOutcome::Failed(_) => None,
        };
        let previous = std::mem::replace(&mut self.last_warning, warning);
        if warning.is_some() && warning == previous {
            return;
        }
        match outcome {
            RefreshOutcome::ReadFailed(error) => warn_password_file_unreadable(server, *error),
            RefreshOutcome::Unchanged => {}
            RefreshOutcome::AuthAccepted => tracing::info!(
                server.address = %server.host,
                server.port = server.port,
                "cache_password_reloaded"
            ),
            RefreshOutcome::AuthRefused => warn_password_refresh_failed(server, ErrorType::Auth),
            RefreshOutcome::Failed(error) => warn_password_refresh_failed(server, *error),
        }
    }
}
