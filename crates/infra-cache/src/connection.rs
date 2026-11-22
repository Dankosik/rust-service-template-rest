use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use backon::BackoffBuilder;
use metrics::{Counter, Gauge};
use operation_context::{BoundedContext, BoundedStop, Deadline, OperationContext};
use parking_lot::Mutex;
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_util::task::TaskTracker;

use crate::admission::Admitted;
use crate::credentials::{Disposition, PASSWORD_REFRESH_INTERVAL, Rotation};
use crate::dial::Dialed;
use crate::error::{
    ErrorType, FailureCause, MutationCommandError, MutationKind, ReadFailure, denies_permission,
    error_type,
};
use crate::exchange::{AbandonedReply, Exchange, drain_abandoned_replies, send_accepted};
use crate::observe::{ADMISSION_REFUSED_METRIC, COMMANDS_IN_FLIGHT_METRIC, RETIREMENTS_METRIC};
use crate::{CloseOutcome, ServerEndpoint};

const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const SETUP_BACKOFF_MIN_DELAY: Duration = Duration::from_millis(100);
const SETUP_BACKOFF_JITTERED_MAX_DELAY: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_UNJITTERED_MAX_DELAY: Duration = Duration::from_secs(1);
const MIN_GENERATION_SPACING: Duration = Duration::from_secs(2);
const SETUP_BACKOFF_FACTOR: f32 = 2.0;
const PING_INTERVAL: Duration = Duration::from_secs(2);

#[cfg(test)]
#[path = "tests/full_pipeline.rs"]
mod full_pipeline_tests;

#[derive(Clone)]
pub(crate) struct Retirement(Arc<RetirementInner>);

struct RetirementInner {
    retired: AtomicBool,
    waiters: Notify,
}

impl Retirement {
    pub(crate) fn new() -> Self {
        Self(Arc::new(RetirementInner {
            retired: AtomicBool::new(false),
            waiters: Notify::new(),
        }))
    }

    pub(crate) fn is_retired(&self) -> bool {
        self.0.retired.load(Ordering::Acquire)
    }

    pub(crate) fn retire(&self) {
        if !self.0.retired.swap(true, Ordering::AcqRel) {
            self.0.waiters.notify_waiters();
        }
    }

    pub(crate) async fn wait(&self) {
        if self.is_retired() {
            return;
        }
        let notified = self.0.waiters.notified();
        if self.is_retired() {
            return;
        }
        notified.await;
    }
}

struct ApplicationSlots(AtomicUsize);

impl ApplicationSlots {
    fn new(limit: usize) -> Self {
        Self(AtomicUsize::new(limit))
    }

    fn try_acquire(&self) -> Option<impl Drop + '_> {
        self.0
            .try_update(Ordering::Acquire, Ordering::Relaxed, |available| {
                available.checked_sub(1)
            })
            .ok()
            .map(|_| {
                std::mem::DropGuard::new(&self.0, |slots| {
                    slots.fetch_add(1, Ordering::Release);
                })
            })
    }
}

#[derive(derive_more::Debug)]
pub(crate) struct Link {
    pub(crate) server: ServerEndpoint,
    command_timeout: Duration,
    #[debug(skip)]
    shared: Arc<Shared>,
    #[debug(skip)]
    application_slots: ApplicationSlots,
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
    Unavailable(ErrorType),
    Closed,
}

struct Generation {
    connection: MultiplexedConnection,
    caller_capacity: Arc<Semaphore>,
    abandoned: mpsc::UnboundedSender<AbandonedReply>,
    retired: Retirement,
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

    fn exchange<T: Send + 'static, F: Future<Output = redis::RedisResult<T>> + Send + 'static>(
        &self,
        reservation: OwnedSemaphorePermit,
        reply: F,
        wire_deadline: Deadline,
    ) -> Exchange<'_, F> {
        Exchange::new(
            &self.retired,
            &self.abandoned,
            reservation,
            reply,
            wire_deadline,
        )
    }
}

impl Shared {
    fn close(&self) {
        let state_guard = self.state_lock.lock();
        let previous = self.state.swap(Arc::new(State::Closed));
        if let State::Connected(current) = &*previous {
            current.retired.retire();
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
        generation.retired.retire();
        if !matches!(
            &**self.state.load(),
            State::Connected(current) if Arc::ptr_eq(current, generation)
        ) {
            return;
        }
        let cooldown = generation.cooldown_error();
        self.state.store(Arc::new(
            cooldown.map_or(State::Connecting, State::Unavailable),
        ));
        self.retirements.increment(1);
        drop(state_guard);
        if cooldown.is_some() {
            self.changed.notify_waiters();
        }
    }

    #[must_use]
    fn publish(&self, generation: Arc<Generation>) -> bool {
        self.transition(State::Connected(generation))
    }

    #[must_use]
    fn record_failure(&self, error: ErrorType) -> bool {
        self.transition(State::Unavailable(error))
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

    async fn wait_settled(&self) -> Result<Arc<Generation>, ErrorType> {
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
                if current.retired.is_retired() {
                    current.cooldown_error().map(Err)
                } else {
                    Some(Ok(current.clone()))
                }
            }
            State::Unavailable(error) => Some(Err(*error)),
            State::Connecting => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum AfterReply {
    HonorStop,
    KeepConfirmed,
}

pub(crate) enum DispatchError {
    NotApplied(FailureCause),
    OutcomeUnknown(FailureCause),
    Reply {
        error: redis::RedisError,
        stopped: Option<FailureCause>,
    },
}

impl DispatchError {
    pub(crate) fn into_mutation_error(self, kind: MutationKind) -> MutationCommandError {
        match self {
            Self::NotApplied(cause) => MutationCommandError::NotApplied(cause.into()),
            Self::OutcomeUnknown(cause) => MutationCommandError::OutcomeUnknown(cause),
            Self::Reply { error, stopped } => {
                MutationCommandError::from_reply(&error, kind, stopped)
            }
        }
    }
}

impl From<FailureCause> for DispatchError {
    fn from(cause: FailureCause) -> Self {
        Self::NotApplied(cause)
    }
}

impl From<BoundedStop> for DispatchError {
    fn from(stop: BoundedStop) -> Self {
        Self::NotApplied(stop.into())
    }
}

impl From<ErrorType> for DispatchError {
    fn from(error: ErrorType) -> Self {
        Self::NotApplied(FailureCause::Failed(error))
    }
}

impl From<DispatchError> for ReadFailure {
    fn from(error: DispatchError) -> Self {
        match error {
            DispatchError::NotApplied(cause) | DispatchError::OutcomeUnknown(cause) => {
                Self::Cause(cause)
            }
            DispatchError::Reply { error, stopped } => Self::from_reply(&error, stopped),
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
        let supervisor = shared
            .tasks
            .spawn(supervise(
                std::mem::DropGuard::new((shared.clone(), admitted), close_supervisor),
                command_timeout,
            ))
            .abort_handle();
        Self {
            server,
            command_timeout,
            shared,
            application_slots: ApplicationSlots::new(max_concurrency),
            admission_refused,
            commands_in_flight,
            supervisor,
        }
    }

    pub(crate) async fn close(&self, budget: Duration) -> CloseOutcome {
        self.stop();
        if timeout(budget, self.shared.tasks.wait()).await.is_ok() {
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

    pub(crate) async fn dispatch<T: Send + 'static, F>(
        &self,
        build_query: impl FnOnce(MultiplexedConnection) -> F,
        parent: &OperationContext,
        wait_ceiling: Option<Duration>,
        after_reply: AfterReply,
    ) -> Result<T, DispatchError>
    where
        F: Future<Output = redis::RedisResult<T>> + Send + 'static,
    {
        let wait_timeout = wait_ceiling.map_or(self.command_timeout, |limit| {
            self.command_timeout.min(limit)
        });
        let budget = parent.bounded(wait_timeout);
        budget.check()?;
        let _slot = self.claim_slot()?;
        let (generation, reservation) = self.reserve_generation(&budget).await?;
        let query = build_query(generation.connection.clone());
        check_before_dispatch(&budget, &generation)?;
        let mut exchange =
            generation.exchange(reservation, query, Deadline::after(self.command_timeout));
        let reply = tokio::select! {
            biased;
            reply = &mut exchange => reply,
            stop = budget.wait_stopped() => {
                return Err(DispatchError::OutcomeUnknown(stop.into()));
            }
            () = generation.retired.wait() => {
                return Err(DispatchError::OutcomeUnknown(FailureCause::Failed(ErrorType::Io)));
            }
        };
        match (reply, after_reply) {
            (Ok(value), AfterReply::KeepConfirmed) => Ok(value),
            (Ok(value), AfterReply::HonorStop) => match budget.stopped() {
                Some(stop) => Err(DispatchError::OutcomeUnknown(stop.into())),
                None => Ok(value),
            },
            (Err(error), _) => Err(DispatchError::Reply {
                error,
                stopped: budget.stopped().map(Into::into),
            }),
        }
    }

    async fn reserve_generation(
        &self,
        budget: &BoundedContext,
    ) -> Result<(Arc<Generation>, OwnedSemaphorePermit), FailureCause> {
        let settled = if let Some(settled) = self.shared.settled() {
            settled
        } else {
            let settled = tokio::select! {
                biased;
                settled = self.shared.wait_settled() => settled,
                stop = budget.wait_stopped() => return Err(stop.into()),
            };
            budget.check()?;
            settled
        };
        let generation = settled.map_err(FailureCause::Failed)?;
        let reservation = tokio::select! {
            biased;
            result = generation.reserve() => result.map_err(FailureCause::Failed)?,
            stop = budget.wait_stopped() => return Err(stop.into()),
            () = generation.retired.wait() => return Err(FailureCause::Failed(ErrorType::Io)),
        };
        check_before_dispatch(budget, &generation)?;
        Ok((generation, reservation))
    }

    fn claim_slot(&self) -> Result<impl Drop + '_, ErrorType> {
        let slot = self.application_slots.try_acquire().ok_or_else(|| {
            self.admission_refused.increment(1);
            ErrorType::Other
        })?;
        self.commands_in_flight.increment(1.0);
        Ok(std::mem::DropGuard::new(slot, |_slot| {
            self.commands_in_flight.decrement(1.0);
        }))
    }
}

fn check_before_dispatch(
    budget: &BoundedContext,
    generation: &Generation,
) -> Result<(), FailureCause> {
    budget.check()?;
    if generation.retired.is_retired() {
        return Err(FailureCause::Failed(ErrorType::Io));
    }
    Ok(())
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
    let reply = send_accepted(&mut connection, command, accepts);
    let deadline = Deadline::after(command_timeout);
    tokio::select! {
        biased;
        () = generation.retired.wait() => Err(MaintenanceError::Aborted(ErrorType::Io)),
        result = reply => result.map_err(MaintenanceError::Redis),
        () = deadline.wait() => Err(MaintenanceError::Aborted(ErrorType::Timeout)),
    }
}

async fn authenticate_generation(
    generation: &Generation,
    command: &redis::Cmd,
    command_timeout: Duration,
) -> Result<(), ErrorType> {
    maintenance_query(
        generation,
        command,
        |value| matches!(value, redis::Value::Okay),
        command_timeout,
    )
    .await
    .map_err(|error| error.error_type())
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
        Err(MaintenanceError::Redis(redis_error)) if denies_permission(&redis_error) => {
            PingOutcome::Refused(error_type(&redis_error))
        }
        Err(error) => PingOutcome::Retire(error.error_type()),
    }
}

type SupervisorGuard = std::mem::DropGuard<(Arc<Shared>, Admitted), fn((Arc<Shared>, Admitted))>;

fn close_supervisor((shared, admitted): (Arc<Shared>, Admitted)) {
    if std::thread::panicking() {
        tracing::error!(
            server.address = %admitted.server.host,
            server.port = admitted.server.port,
            "cache_supervisor_panicked"
        );
    }
    shared.close();
}

async fn supervise(exit: SupervisorGuard, command_timeout: Duration) {
    let (shared, admitted) = &*exit;
    let server = &admitted.server;
    let mut backoff = setup_backoff();
    let mut next_attempt = Instant::now();
    loop {
        sleep_until(next_attempt).await;
        if shared.is_closed() {
            return;
        }
        let retired = Retirement::new();
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
                    .unwrap_or(SETUP_BACKOFF_JITTERED_MAX_DELAY)
                    .min(SETUP_BACKOFF_JITTERED_MAX_DELAY);
                next_attempt = Instant::now() + delay;
                continue;
            }
        };
        let Some(cooldown_until) =
            run_generation(shared, admitted, dialed, rotation, retired, command_timeout).await
        else {
            return;
        };
        next_attempt = cooldown_until;
        backoff = setup_backoff();
    }
}

async fn run_generation(
    shared: &Shared,
    admitted: &Admitted,
    dialed: Dialed,
    rotation: Option<Rotation<'_>>,
    retired: Retirement,
    command_timeout: Duration,
) -> Option<Instant> {
    let server = &admitted.server;
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
        cache.tls = admitted.transport.is_tls(),
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

pub(crate) fn setup_backoff() -> backon::ExponentialBackoff {
    backon::ExponentialBuilder::default()
        .with_min_delay(SETUP_BACKOFF_MIN_DELAY)
        .with_max_delay(SETUP_BACKOFF_UNJITTERED_MAX_DELAY)
        .with_factor(SETUP_BACKOFF_FACTOR)
        .with_jitter()
        .without_max_times()
        .build()
}

async fn setup_attempt<'a>(
    admitted: &'a Admitted,
    retired: &Retirement,
    shared: &'a Shared,
) -> Result<(Dialed, Option<Rotation<'a>>), ErrorType> {
    let setup = async {
        let (settings, rotation) = match &admitted.password_file {
            Some(file) => {
                let (settings, rotation) = file
                    .connect_settings(&admitted.settings, &admitted.server, &shared.tasks)
                    .await?;
                (settings, Some(rotation))
            }
            None => (admitted.settings.clone(), None),
        };
        let dialed = connect(admitted, &settings, retired.clone(), shared).await?;
        Ok((dialed, rotation))
    };
    match timeout(SETUP_TIMEOUT, setup).await {
        Err(_elapsed) => Err(ErrorType::Timeout),
        Ok(Ok(_)) if retired.is_retired() => Err(ErrorType::Io),
        Ok(connected) => connected,
    }
}

fn warn_ping_failed(server: &ServerEndpoint, error: ErrorType) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        error.type = error.label(),
        "cache_ping_failed"
    );
}

async fn connect(
    admitted: &Admitted,
    settings: &redis::RedisConnectionInfo,
    retired: Retirement,
    shared: &Shared,
) -> Result<Dialed, ErrorType> {
    let config = redis::AsyncConnectionConfig::new()
        .set_response_timeout(None)
        .set_connection_timeout(None)
        .set_pipeline_buffer_size(shared.max_concurrency + 1)
        .set_push_sender(move |push: redis::PushInfo| {
            if push.kind == redis::PushKind::Disconnection {
                retired.retire();
            }
            Ok::<(), redis::aio::SendError>(())
        });
    admitted
        .connect(settings, config, &shared.tasks)
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
            () = generation.retired.wait() => return,
            () = sleep_until(next_refresh), if rotation.is_some() && next_refresh <= next_ping => {
                next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
                let Some(rotation) = rotation.as_mut() else { continue };
                let disposition = rotation
                    .refresh_and_record(server, &generation.retired, async |command| {
                        authenticate_generation(generation, command, command_timeout).await
                    })
                    .await;
                if disposition == Disposition::Retire {
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
