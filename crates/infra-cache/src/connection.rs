//! One owned supervisor; redis-rs owns transport and multiplexing.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use backon::BackoffBuilder;
use metrics::{Counter, Gauge, Unit};
use operation_context::{OperationContext, Stopped};
use redis::aio::MultiplexedConnection;
use tokio::sync::{Notify, Semaphore, SemaphorePermit};
use tokio::time::{Instant, sleep, sleep_until, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::ServerIdentity;
use crate::credentials::{PASSWORD_REFRESH_INTERVAL, PasswordFile};
use crate::observe::{self, ErrorType};

/// One reconnect attempt stays inside the startup check. A hang guard, not a
/// latency target: a slow but live server is waited for.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Floor of the client's exponential reconnect backoff.
const MIN_DELAY: Duration = Duration::from_millis(100);
/// Cap on setup backoff and minimum spacing between published generations.
const MAX_DELAY: Duration = Duration::from_secs(2);
/// Factor of the client's exponential reconnect schedule.
const EXPONENT_BASE: f32 = 2.0;
/// Bound one reconnect chain; the supervisor starts the next after a pause.
const NUMBER_OF_RETRIES: usize = 6;
/// Shared by every namespace, including commands waiting for a connection.
const APPLICATION_SLOTS: usize = 256;

pub(crate) struct Link {
    pub(crate) server: ServerIdentity,
    shared: Arc<Shared>,
    application: Semaphore,
    probes: Semaphore,
    admission_refused: Counter,
    commands_in_flight: Gauge,
    cancelled: CancellationToken,
    supervisor: tokio::task::JoinHandle<()>,
}

struct Shared {
    state: Mutex<State>,
    changed: Notify,
    retirements: Counter,
}

#[derive(Default)]
struct State {
    current: Option<Arc<Generation>>,
    closed: bool,
    // A failed attempt is observable during backoff, but never stops retrying.
    error: Option<ErrorType>,
}

struct Generation {
    connection: MultiplexedConnection,
    retired: CancellationToken,
}

/// Includes admitted commands still waiting for a connection generation.
struct Admission<'a> {
    _permit: SemaphorePermit<'a>,
    in_flight: Gauge,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.in_flight.decrement(1.0);
    }
}

/// Retire possible dispatch before its admission can be reused on cancellation.
struct Exchange<'a> {
    shared: &'a Shared,
    generation: Arc<Generation>,
    _admission: Admission<'a>,
    completed: bool,
}

impl Drop for Exchange<'_> {
    fn drop(&mut self) {
        if !self.completed {
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

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        if let Some(current) = state.current.take() {
            current.retired.cancel();
            self.retirements.increment(1);
        }
        drop(state);
        self.changed.notify_waiters();
    }

    fn retire(&self, generation: &Arc<Generation>) {
        let mut state = self.state();
        generation.retired.cancel();
        if state
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, generation))
        {
            state.current = None;
            self.retirements.increment(1);
        }
        drop(state);
        self.changed.notify_waiters();
    }

    async fn acquire(&self) -> Result<Arc<Generation>, ErrorType> {
        loop {
            // Register before examining state so publication cannot be missed.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state();
                if state.closed {
                    return Err(ErrorType::Io);
                }
                if let Some(current) = &state.current
                    && !current.retired.is_cancelled()
                {
                    return Ok(current.clone());
                }
                if let Some(error) = state.error {
                    return Err(error);
                }
            }
            changed.await;
        }
    }
}

impl Link {
    pub(crate) fn start(
        server: ServerIdentity,
        client: redis::Client,
        password_file: Option<PasswordFile>,
        command_timeout: Duration,
    ) -> Self {
        metrics::describe_counter!(
            "cache_command_admission_refused_total",
            Unit::Count,
            "Cache application or probe commands refused because their admission slots are full"
        );
        metrics::describe_gauge!(
            "cache_commands_in_flight",
            Unit::Count,
            "Admitted cache application and probe commands, including connection waits, until completion or retirement"
        );
        metrics::describe_counter!(
            "cache_connection_retirements_total",
            Unit::Count,
            "Published cache connection generations synchronously removed from reuse, not remote socket finality"
        );
        let admission_refused = metrics::counter!("cache_command_admission_refused_total");
        let commands_in_flight = metrics::gauge!("cache_commands_in_flight");
        let retirements = metrics::counter!("cache_connection_retirements_total");
        admission_refused.increment(0);
        // Several links may share the unlabelled series: never reset active work.
        commands_in_flight.increment(0.0);
        retirements.increment(0);
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Notify::new(),
            retirements,
        });
        let cancelled = CancellationToken::new();
        let supervisor = tokio::spawn(supervise(
            shared.clone(),
            cancelled.clone(),
            client,
            password_file,
            command_timeout,
        ));
        Self {
            server,
            shared,
            application: Semaphore::new(APPLICATION_SLOTS),
            probes: Semaphore::new(1),
            admission_refused,
            commands_in_flight,
            cancelled,
            supervisor,
        }
    }

    pub(crate) async fn command<T: redis::FromRedisValue>(
        &self,
        command: &redis::Cmd,
        context: &OperationContext,
    ) -> Result<T, ErrorType> {
        context.check().map_err(|_| ErrorType::Timeout)?;
        let admission = self.admit(&self.application)?;
        let generation = tokio::select! {
            biased;
            _ = context.wait_stopped() => return Err(ErrorType::Timeout),
            result = self.shared.acquire() => result?,
        };
        context.check().map_err(|_| ErrorType::Timeout)?;
        self.exchange_with_context(generation, admission, command, context)
            .await
    }

    pub(crate) async fn probe(&self) -> Result<(), ErrorType> {
        let admission = self.admit(&self.probes)?;
        let generation = self.shared.acquire().await?;
        self.exchange(
            generation,
            admission,
            &redis::Cmd::ping(),
            Instant::now() + CONNECT_TIMEOUT,
        )
        .await
    }

    fn admit<'a>(&self, slots: &'a Semaphore) -> Result<Admission<'a>, ErrorType> {
        let permit = slots.try_acquire().map_err(|_| {
            self.admission_refused.increment(1);
            ErrorType::Other
        })?;
        self.commands_in_flight.increment(1.0);
        Ok(Admission {
            _permit: permit,
            in_flight: self.commands_in_flight.clone(),
        })
    }

    async fn exchange<T: redis::FromRedisValue>(
        &self,
        generation: Arc<Generation>,
        admission: Admission<'_>,
        command: &redis::Cmd,
        deadline: Instant,
    ) -> Result<T, ErrorType> {
        let mut guard = Exchange {
            shared: &self.shared,
            generation,
            _admission: admission,
            completed: false,
        };
        let result = exchange(&guard.generation, command, deadline).await;
        guard.completed = result.is_ok();
        result
    }

    async fn exchange_with_context<T: redis::FromRedisValue>(
        &self,
        generation: Arc<Generation>,
        admission: Admission<'_>,
        command: &redis::Cmd,
        context: &OperationContext,
    ) -> Result<T, ErrorType> {
        let mut guard = Exchange {
            shared: &self.shared,
            generation,
            _admission: admission,
            completed: false,
        };
        let mut connection = guard.generation.connection.clone();
        let result = tokio::select! {
            biased;
            stopped = context.wait_stopped() => {
                if stopped == Stopped::Deadline {
                    self.shared.retire(&guard.generation);
                }
                Err(ErrorType::Timeout)
            }
            () = guard.generation.retired.cancelled() => Err(ErrorType::Io),
            result = command.query_async(&mut connection) => {
                result.map_err(|error| observe::error_type(&error))
            }
        };
        guard.completed = result.is_ok();
        result
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.close();
        self.cancelled.cancel();
        self.supervisor.abort();
    }
}

// Ensures a panic or unexpected exit cannot leave an apparently live cache.
struct SupervisorExit(Arc<Shared>);
impl Drop for SupervisorExit {
    fn drop(&mut self) {
        self.0.close();
    }
}

async fn exchange<T: redis::FromRedisValue>(
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
    cancelled: CancellationToken,
    client: redis::Client,
    password_file: Option<PasswordFile>,
    command_timeout: Duration,
) {
    let _exit = SupervisorExit(shared.clone());
    tokio::select! {
        biased;
        () = cancelled.cancelled() => {},
        () = run_supervisor(&shared, &client, password_file.as_ref(), command_timeout) => {},
    }
}

async fn run_supervisor(
    shared: &Shared,
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
        {
            let mut state = shared.state();
            if state.closed {
                return;
            }
            state.error = None;
        }
        let retired = CancellationToken::new();
        let result = timeout(
            CONNECT_TIMEOUT,
            connect(client, password_file, retired.clone()),
        )
        .await;
        let result = result
            .unwrap_or(Err(ErrorType::Timeout))
            .and_then(|connected| {
                if retired.is_cancelled() {
                    Err(ErrorType::Io)
                } else {
                    Ok(connected)
                }
            });
        match result {
            Ok((connection, password)) => {
                let generation = Arc::new(Generation {
                    connection,
                    retired,
                });
                {
                    let mut state = shared.state();
                    if state.closed {
                        return;
                    }
                    state.current = Some(generation.clone());
                }
                next_connection = Instant::now() + MAX_DELAY;
                shared.changed.notify_waiters();
                backoff = retry.build();
                maintain(
                    shared,
                    &generation,
                    password_file,
                    password,
                    command_timeout,
                )
                .await;
                shared.retire(&generation);
            }
            Err(error) => {
                {
                    let mut state = shared.state();
                    if state.closed {
                        return;
                    }
                    state.error = Some(error);
                }
                shared.changed.notify_waiters();
                tracing::warn!(error.type = error.label(), "cache_connection_failed");
                let delay = backoff.next().unwrap_or_else(|| {
                    backoff = retry.build();
                    MAX_DELAY
                });
                // backon's jitter is additive after its cap; cap the final delay.
                sleep(delay.min(MAX_DELAY)).await;
            }
        }
    }
}

async fn connect(
    admitted: &redis::Client,
    password_file: Option<&PasswordFile>,
    retired: CancellationToken,
) -> Result<(MultiplexedConnection, Option<String>), ErrorType> {
    let (client, password) = match password_file {
        Some(file) => {
            let password = file.read().await.map_err(|_| ErrorType::Auth)?;
            let info = admitted.get_connection_info().clone();
            let settings = info
                .redis_settings()
                .clone()
                .set_username(file.username())
                .set_password(&password);
            let client = redis::Client::open(info.set_redis_settings(settings))
                .map_err(|_| ErrorType::Other)?;
            (client, Some(password))
        }
        None => (admitted.clone(), None),
    };
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
    Ok((connection, password))
}

async fn maintain(
    shared: &Shared,
    generation: &Arc<Generation>,
    password_file: Option<&PasswordFile>,
    mut authenticated: Option<String>,
    command_timeout: Duration,
) {
    let mut next_ping = Instant::now() + MAX_DELAY;
    let mut next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
    let mut unreadable = false;
    loop {
        tokio::select! {
            biased;
            () = generation.retired.cancelled() => return,
            () = sleep_until(next_refresh), if password_file.is_some() => {
                // A rejected AUTH must not add its duration to the next refresh.
                next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;
                if let Some(file) = password_file {
                    metrics::describe_counter!(
                        "cache_password_file_refreshes_total",
                        "Completed maintained-connection password-file refreshes; auth_accepted requires an accepted AUTH reply."
                    );
                    let refresh = refresh(generation, file, &mut authenticated, &mut unreadable);
                    let result = tokio::select! {
                        biased;
                        () = generation.retired.cancelled() => return,
                        result = timeout(CONNECT_TIMEOUT, refresh) => result.unwrap_or(Err(ErrorType::Timeout)),
                    };
                    if let Err(error) = result {
                        metrics::counter!("cache_password_file_refreshes_total", "outcome" => "refresh_failed", "reason" => error.label()).increment(1);
                        tracing::warn!(error.type = error.label(), "cache_password_refresh_failed");
                        if error != ErrorType::Auth { shared.retire(generation); return; }
                    }
                }
            }
            () = sleep_until(next_ping) => {
                let deadline = Instant::now() + command_timeout.min(CONNECT_TIMEOUT);
                if let Err(error) = exchange::<()>(generation, &redis::Cmd::ping(), deadline).await {
                    tracing::warn!(error.type = error.label(), "cache_ping_failed");
                    shared.retire(generation);
                    return;
                }
                next_ping = Instant::now() + MAX_DELAY;
            }
        }
    }
}

async fn refresh(
    generation: &Generation,
    file: &PasswordFile,
    authenticated: &mut Option<String>,
    unreadable: &mut bool,
) -> Result<(), ErrorType> {
    let password = match file.read().await {
        Ok(password) => {
            *unreadable = false;
            password
        }
        Err(error) => {
            metrics::counter!("cache_password_file_refreshes_total", "outcome" => "read_failed", "reason" => "none").increment(1);
            if !std::mem::replace(unreadable, true) {
                tracing::warn!(%error, "cache_password_file_unreadable");
            }
            return Ok(());
        }
    };
    if authenticated.as_deref() == Some(password.as_str()) {
        metrics::counter!("cache_password_file_refreshes_total", "outcome" => "unchanged", "reason" => "none").increment(1);
        return Ok(());
    }
    let mut command = redis::cmd("AUTH");
    command.arg(file.username()).arg(&password);
    // The caller's shared read/AUTH envelope supplies the actual deadline.
    exchange::<()>(generation, &command, Instant::now() + CONNECT_TIMEOUT).await?;
    if generation.retired.is_cancelled() {
        return Err(ErrorType::Io);
    }
    *authenticated = Some(password);
    metrics::counter!("cache_password_file_refreshes_total", "outcome" => "auth_accepted", "reason" => "none").increment(1);
    tracing::info!("cache_password_reloaded");
    Ok(())
}
