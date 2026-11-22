use std::borrow::Cow;
use std::pin::Pin;
use std::sync::LazyLock;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use futures_util::{Stream, TryFutureExt as _, TryStreamExt as _};
use smallvec::SmallVec;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{
    PgArguments, PgConnection, PgQueryResult, PgRow, PgStatement, PgTypeInfo, Postgres,
};
use sqlx::{
    Arguments as _, AssertSqlSafe, Connection, Either, Execute, Executor, Row as _, SqlStr,
};
use tokio::time::Instant;
use tracing::Instrument;

use crate::error::{QUERY_CANCELED, STATEMENT_COMPLETION_UNKNOWN, sqlstate};
use crate::observe::{self, TransactionObservation};
use crate::observed;
use crate::pool::{Pool, format_pg_duration};

#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("postgres transaction: acquire connection: {0}")]
    Acquire(#[source] sqlx::Error),
    #[error("postgres transaction: begin: {0}")]
    Begin(#[source] sqlx::Error),
    #[error("postgres transaction: not committed: {0}")]
    CommitFailed(#[source] sqlx::Error),
    #[error("postgres transaction: commit outcome unknown: {0}")]
    CommitUnknown(#[source] sqlx::Error),
}

impl TxError {
    #[must_use]
    pub const fn driver(&self) -> &sqlx::Error {
        match self {
            Self::Acquire(err)
            | Self::Begin(err)
            | Self::CommitFailed(err)
            | Self::CommitUnknown(err) => err,
        }
    }
}

#[derive(Debug)]
pub struct Tx<'c, Mode = ()> {
    conn: &'c mut PgConnection,
    proven_alive: bool,
    mode: Mode,
    notifications: SmallVec<[Notification; 1]>,
    next_savepoint: u32,
}

pub type BudgetedTx<'c> = Tx<'c, OperationDeadline>;

#[derive(Debug)]
pub struct OperationDeadline {
    at: Instant,
    armed_timeout_ms: Option<i32>,
    expired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("postgres operation deadline elapsed before commit")]
pub struct OperationDeadlineElapsed;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SavepointEnd<T, A = std::convert::Infallible> {
    Keep(T),
    Rollback(T),
    AbortTransaction(A),
}

#[derive(Debug, thiserror::Error)]
pub enum SavepointError<A = std::convert::Infallible> {
    #[error("postgres savepoint: begin: {0}")]
    Begin(#[source] sqlx::Error),
    #[error("postgres savepoint: rollback: {0}")]
    Rollback(#[source] sqlx::Error),
    #[error(transparent)]
    Aborted(A),
}

mod sealed {
    #[derive(Debug)]
    pub struct Private;

    pub trait TxMode: std::fmt::Debug + Send {
        fn operation_deadline(&mut self, _: Private) -> Option<&mut super::OperationDeadline>;
    }

    impl TxMode for () {
        fn operation_deadline(&mut self, _: Private) -> Option<&mut super::OperationDeadline> {
            None
        }
    }

    impl TxMode for super::OperationDeadline {
        fn operation_deadline(&mut self, _: Private) -> Option<&mut super::OperationDeadline> {
            Some(self)
        }
    }
}

pub trait TxMode: sealed::TxMode {}

impl TxMode for () {}
impl TxMode for OperationDeadline {}

trait TxModeExt<E: From<TxError>>: TxMode {
    fn acquisition_expired(&self, _error: &TxError) -> Option<E> {
        None
    }

    fn check_expiry(&self) -> Result<(), E>;

    fn check_before_commit(&mut self) -> impl Future<Output = Result<(), E>> + Send;
}

impl<E: From<TxError>> TxModeExt<E> for () {
    fn check_expiry(&self) -> Result<(), E> {
        Ok(())
    }

    fn check_before_commit(&mut self) -> impl Future<Output = Result<(), E>> + Send {
        std::future::poll_fn(|_| Poll::Ready(Ok(())))
    }
}

impl<E: From<TxError> + From<OperationDeadlineElapsed>> TxModeExt<E> for OperationDeadline {
    fn acquisition_expired(&self, error: &TxError) -> Option<E> {
        if matches!(error, TxError::Acquire(sqlx::Error::PoolTimedOut)) && Instant::now() >= self.at
        {
            Some(OperationDeadlineElapsed.into())
        } else {
            None
        }
    }

    fn check_expiry(&self) -> Result<(), E> {
        if self.expired {
            return Err(OperationDeadlineElapsed.into());
        }
        Ok(())
    }

    async fn check_before_commit(&mut self) -> Result<(), E> {
        if self.remaining_ms_or_expire().await.is_none() {
            return Err(OperationDeadlineElapsed.into());
        }
        Ok(())
    }
}

fn deadline_elapsed_error() -> sqlx::Error {
    sqlx::Error::Io(std::io::ErrorKind::TimedOut.into())
}

impl OperationDeadline {
    async fn remaining_ms_or_expire(&mut self) -> Option<i32> {
        let ms = self
            .at
            .saturating_duration_since(Instant::now())
            .as_millis();
        if self.expired || ms == 0 {
            self.expired = true;
            if Instant::now() < self.at {
                tokio::time::sleep_until(self.at).await;
            }
            return None;
        }
        Some(i32::try_from(ms).unwrap_or(i32::MAX))
    }

    async fn arm(&mut self, conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        const ARM_ARG_COUNT: usize = 2;
        const ARM_ARG_BYTES: usize = 2 * (size_of::<i32>() + size_of::<i32>());

        let remaining_ms = self
            .remaining_ms_or_expire()
            .await
            .ok_or_else(deadline_elapsed_error)?;
        let mut arguments = PgArguments::default();
        arguments.reserve(ARM_ARG_COUNT, ARM_ARG_BYTES);
        let result = sqlx::query_with(
            "WITH prior AS MATERIALIZED (\
             SELECT (EXTRACT(EPOCH FROM pg_catalog.current_setting('statement_timeout')::interval) * 1000)::integer AS ms\
             ) \
             SELECT pg_catalog.set_config('statement_timeout', LEAST(NULLIF(ms, 0), $1)::text, true), \
             ms = 0 OR $1 < ms OR (ms = $1 AND ms IS NOT DISTINCT FROM $2) FROM prior",
            arguments,
        )
        .bind(remaining_ms)
        .bind(self.armed_timeout_ms)
        .try_map(|row: PgRow| {
            let _: &str = row.try_get(0usize)?;
            row.try_get::<bool, _>(1usize)
        })
        .fetch_one(conn)
        .await;
        let deadline_controls_statement_timeout = self.observe_cancellation(result).await?;
        self.armed_timeout_ms = deadline_controls_statement_timeout.then_some(remaining_ms);
        self.remaining_ms_or_expire()
            .await
            .map(|_| ())
            .ok_or_else(deadline_elapsed_error)
    }

    fn observe_error<T>(
        &mut self,
        error: sqlx::Error,
    ) -> impl Future<Output = Result<T, sqlx::Error>> + use<T> {
        let rounded_expiry = self.armed_timeout_ms.is_some()
            && sqlstate(&error).as_deref() == Some(QUERY_CANCELED)
            && self.at.saturating_duration_since(Instant::now()) < Duration::from_millis(1);
        self.expired |= rounded_expiry;
        let at = self.at;
        async move {
            if rounded_expiry && Instant::now() < at {
                tokio::time::sleep_until(at).await;
            }
            Err(error)
        }
    }

    async fn observe_cancellation<T>(
        &mut self,
        result: Result<T, sqlx::Error>,
    ) -> Result<T, sqlx::Error> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.observe_error(error).await,
        }
    }
}

struct SingleStatement<E>(E);

impl<'q, E: Execute<'q, Postgres>> Execute<'q, Postgres> for SingleStatement<E> {
    fn sql(self) -> SqlStr {
        self.0.sql()
    }

    fn statement(&self) -> Option<&PgStatement> {
        self.0.statement()
    }

    fn take_arguments(&mut self) -> Result<Option<PgArguments>, sqlx::error::BoxDynError> {
        Ok(Some(self.0.take_arguments()?.unwrap_or_default()))
    }

    fn persistent(&self) -> bool {
        self.0.persistent()
    }
}

impl<M> Tx<'_, M> {
    pub fn notify_after_commit(
        &mut self,
        channel: &'static str,
        payload: &'static str,
        min_interval: Duration,
    ) {
        const MAX_NOTIFICATIONS: usize = 32;
        let key = NotificationKey { channel, payload };
        if self.notifications.len() < MAX_NOTIFICATIONS
            && !self
                .notifications
                .iter()
                .any(|notification| notification.key == key)
        {
            self.notifications.push(Notification { key, min_interval });
        }
    }
}

impl Tx<'_> {
    pub fn connection(&mut self) -> &mut PgConnection {
        self.proven_alive = false;
        self.conn
    }

    pub async fn set_statement_timeout(&mut self, timeout: Duration) -> Result<(), sqlx::Error> {
        observed!(
            "set statement timeout",
            sqlx::query!(
                "SELECT set_config('statement_timeout', $1, true)",
                format_pg_duration(timeout)
            )
            .fetch_one(self),
        )
        .await?;
        Ok(())
    }
}

impl<M: TxMode> Tx<'_, M> {
    fn run_unproven<'a, T: Send + 'a>(
        &'a mut self,
        call: impl FnOnce(&'a mut PgConnection) -> DriverFuture<'a, T> + Send + 'a,
    ) -> DriverFuture<'a, T> {
        self.proven_alive = false;
        let conn = &mut *self.conn;
        let Some(budget) = self.mode.operation_deadline(sealed::Private) else {
            return call(conn);
        };
        Box::pin(async move {
            budget.arm(conn).await?;
            let result = call(conn).await;
            budget.observe_cancellation(result).await
        })
    }

    pub async fn with_savepoint<T, A, F>(&mut self, f: F) -> Result<T, SavepointError<A>>
    where
        F: AsyncFnOnce(&mut Tx<'_, M>) -> SavepointEnd<T, A>,
    {
        let id = self.next_savepoint;
        self.next_savepoint = id.checked_add(1).ok_or_else(|| {
            SavepointError::Begin(sqlx::Error::InvalidArgument(
                "transaction savepoint limit reached".into(),
            ))
        })?;
        let saved_notifications = self.notifications.len();
        self.execute(savepoint_sql(id, false))
            .await
            .map_err(SavepointError::Begin)?;
        match f(self).await {
            SavepointEnd::Keep(value) => Ok(value),
            SavepointEnd::Rollback(value) => {
                self.rollback_savepoint(id, saved_notifications)
                    .await
                    .map_err(SavepointError::Rollback)?;
                Ok(value)
            }
            SavepointEnd::AbortTransaction(reason) => Err(SavepointError::Aborted(reason)),
        }
    }

    async fn rollback_savepoint(
        &mut self,
        id: u32,
        saved_notifications: usize,
    ) -> Result<(), sqlx::Error> {
        self.proven_alive = false;
        let rollback = collect_query_result(self.conn.fetch_many(savepoint_sql(id, true)));
        if let Some(budget) = self.mode.operation_deadline(sealed::Private) {
            budget
                .remaining_ms_or_expire()
                .await
                .ok_or_else(deadline_elapsed_error)?;
            let response = tokio::time::timeout_at(budget.at, rollback)
                .await
                .map_err(|_| {
                    budget.expired = true;
                    deadline_elapsed_error()
                })?;
            budget.observe_cancellation(response).await?;
        } else {
            rollback.await?;
        }
        self.proven_alive = true;
        self.notifications.truncate(saved_notifications);
        Ok(())
    }
}

fn savepoint_sql(id: u32, rollback: bool) -> AssertSqlSafe<Cow<'static, str>> {
    AssertSqlSafe(match (id, rollback) {
        (0, false) => Cow::Borrowed("SAVEPOINT _tx_sp_0"),
        (0, true) => Cow::Borrowed("ROLLBACK TO SAVEPOINT _tx_sp_0"),
        (id, false) => Cow::Owned(format!("SAVEPOINT _tx_sp_{id}")),
        (id, true) => Cow::Owned(format!("ROLLBACK TO SAVEPOINT _tx_sp_{id}")),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct NotificationKey {
    channel: &'static str,
    payload: &'static str,
}

#[derive(Debug)]
struct Notification {
    key: NotificationKey,
    min_interval: Duration,
}

impl Notification {
    fn record_if_due(
        &self,
        last_sent: &mut foldhash::HashMap<NotificationKey, Instant>,
        now: Instant,
    ) -> bool {
        match last_sent.entry(self.key) {
            std::collections::hash_map::Entry::Occupied(entry)
                if now.saturating_duration_since(*entry.get()) < self.min_interval =>
            {
                false
            }
            entry => {
                entry.insert_entry(now);
                true
            }
        }
    }
}

fn dispatch_notifications(
    pool: &Pool,
    mut guard: DiscardOnDrop,
    mut notifications: SmallVec<[Notification; 1]>,
) {
    const SEND_BUDGET: Duration = Duration::from_millis(100);
    static LAST_SENT: LazyLock<parking_lot::Mutex<foldhash::HashMap<NotificationKey, Instant>>> =
        LazyLock::new(|| parking_lot::Mutex::new(foldhash::HashMap::default()));

    if notifications.is_empty() || !pool.notifications_enabled() {
        return;
    }
    let now = Instant::now();
    let mut last_sent = LAST_SENT.lock();
    notifications.retain(|notification| notification.record_if_due(&mut last_sent, now));
    drop(last_sent);
    if notifications.is_empty() {
        return;
    }
    let closing = pool.close_event();
    let return_in_task = pool.options().get_min_connections() == 0;
    tokio::spawn(async move {
        guard.discard = true;
        let send = async {
            for notification in notifications {
                let NotificationKey { channel, payload } = notification.key;
                let query = sqlx::query!("SELECT pg_notify($1, $2)", channel, payload);
                observed!(
                    "post-commit notification",
                    collect_query_result((&mut *guard.connection).fetch_many(query)),
                )
                .await?;
            }
            Ok::<(), sqlx::Error>(())
        };
        tokio::select! {
            biased;
            () = closing => {}
            result = tokio::time::timeout_at(now + SEND_BUDGET, send) => match result {
                Ok(Ok(())) => {
                    guard.discard = false;
                    if return_in_task {
                        guard.connection.return_to_pool().await;
                    }
                }
                Ok(Err(error)) => {
                    guard.discard = !matches!(error, sqlx::Error::Database(_));
                    tracing::warn!(error.type = crate::failure_cause(&error), "postgres_notification_failed");
                }
                Err(_) => tracing::warn!("postgres_notification_timed_out"),
            }
        }
        drop(guard);
    });
}

type DriverFuture<'e, T> = BoxFuture<'e, Result<T, sqlx::Error>>;
type DriverStream<'e, T> = BoxStream<'e, Result<T, sqlx::Error>>;

impl<'c, M: TxMode> Executor<'c> for &'c mut Tx<'_, M> {
    type Database = Postgres;

    fn execute<'e, 'q: 'e, E>(self, query: E) -> DriverFuture<'e, PgQueryResult>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        let proven_alive = &mut self.proven_alive;
        let conn = &mut *self.conn;
        if let Some(budget) = self.mode.operation_deadline(sealed::Private) {
            Box::pin(async move {
                budget.arm(conn).await?;
                let result = collect_query_result(conn.fetch_many(SingleStatement(query))).await;
                let result = budget.observe_cancellation(result).await?;
                *proven_alive = true;
                Ok(result)
            })
        } else {
            let stream = conn.fetch_many(query);
            Box::pin(async move {
                let result = collect_query_result(stream).await?;
                *proven_alive = true;
                Ok(result)
            })
        }
    }

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> DriverStream<'e, Either<PgQueryResult, PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        let proof_to_restore = Some(&mut self.proven_alive);
        let conn = &mut *self.conn;
        if let Some(budget) = self.mode.operation_deadline(sealed::Private) {
            return Box::pin(
                async move {
                    budget.arm(conn).await?;
                    let statement = Proving {
                        statement: conn.fetch_many(SingleStatement(query)),
                        proof_to_restore,
                    };
                    Ok(statement.or_else(move |error| budget.observe_error(error)))
                }
                .try_flatten_stream(),
            );
        }
        Box::pin(Proving {
            statement: conn.fetch_many(query),
            proof_to_restore,
        })
    }

    fn fetch_optional<'e, 'q: 'e, E>(self, query: E) -> DriverFuture<'e, Option<PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        let proven_alive = &mut self.proven_alive;
        let conn = &mut *self.conn;
        if let Some(budget) = self.mode.operation_deadline(sealed::Private) {
            Box::pin(async move {
                budget.arm(conn).await?;
                let row = budget
                    .observe_cancellation(conn.fetch_optional(SingleStatement(query)).await)
                    .await?;
                *proven_alive = true;
                Ok(row)
            })
        } else {
            let row = conn.fetch_optional(query);
            Box::pin(async move {
                let row = row.await?;
                *proven_alive = true;
                Ok(row)
            })
        }
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [PgTypeInfo],
    ) -> DriverFuture<'e, PgStatement>
    where
        'c: 'e,
    {
        self.run_unproven(move |conn| conn.prepare_with(sql, parameters))
    }

    fn describe<'e>(self, sql: SqlStr) -> DriverFuture<'e, sqlx::Describe<Postgres>>
    where
        'c: 'e,
    {
        self.run_unproven(move |conn| conn.describe(sql))
    }
}

async fn collect_query_result(
    mut stream: DriverStream<'_, Either<PgQueryResult, PgRow>>,
) -> Result<PgQueryResult, sqlx::Error> {
    let mut result = PgQueryResult::default();
    while let Some(step) = stream.try_next().await? {
        if let Either::Left(done) = step {
            result.extend(std::iter::once(done));
        }
    }
    Ok(result)
}

struct Proving<'e, T> {
    statement: DriverStream<'e, T>,
    proof_to_restore: Option<&'e mut bool>,
}

impl<T> Stream for Proving<'_, T> {
    type Item = Result<T, sqlx::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let item = ready!(this.statement.as_mut().poll_next(cx));
        match &item {
            Some(Ok(_)) => {}
            Some(Err(_)) => this.proof_to_restore = None,
            None => {
                if let Some(proven_alive) = this.proof_to_restore.take() {
                    *proven_alive = true;
                }
            }
        }
        Poll::Ready(item)
    }
}

struct DiscardOnDrop {
    connection: PoolConnection<Postgres>,
    discard: bool,
}

impl Drop for DiscardOnDrop {
    fn drop(&mut self) {
        if self.discard {
            self.connection.close_on_drop();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Isolation {
    #[default]
    ServerDefault,
    ReadCommitted,
    RepeatableRead,
    Serializable,
}

impl Isolation {
    #[must_use]
    pub(crate) const fn as_sql(self) -> Option<&'static str> {
        match self {
            Self::ServerDefault => None,
            Self::ReadCommitted => Some("READ COMMITTED"),
            Self::RepeatableRead => Some("REPEATABLE READ"),
            Self::Serializable => Some("SERIALIZABLE"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxOptions {
    pub isolation: Isolation,
    pub read_only: bool,
}

impl TxOptions {
    fn begin_statement(self) -> Option<&'static str> {
        match (self.isolation, self.read_only) {
            (Isolation::ServerDefault, false) => None,
            (Isolation::ServerDefault, true) => Some("BEGIN READ ONLY"),
            (Isolation::ReadCommitted, false) => Some("BEGIN ISOLATION LEVEL READ COMMITTED"),
            (Isolation::ReadCommitted, true) => {
                Some("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
            }
            (Isolation::RepeatableRead, false) => Some("BEGIN ISOLATION LEVEL REPEATABLE READ"),
            (Isolation::RepeatableRead, true) => {
                Some("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
            }
            (Isolation::Serializable, false) => Some("BEGIN ISOLATION LEVEL SERIALIZABLE"),
            (Isolation::Serializable, true) => Some("BEGIN ISOLATION LEVEL SERIALIZABLE READ ONLY"),
        }
    }
}

impl Pool {
    pub async fn in_tx<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
        E: From<TxError>,
    {
        self.in_tx_with(TxOptions::default(), f).await
    }

    pub async fn in_tx_with<T, E, F>(&self, options: TxOptions, f: F) -> Result<T, E>
    where
        F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
        E: From<TxError>,
    {
        transaction(self, options, None, (), f).await
    }

    pub async fn in_tx_with_acquire_deadline<T, E, F>(
        &self,
        deadline: tokio::time::Instant,
        options: TxOptions,
        f: F,
    ) -> Result<T, E>
    where
        F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
        E: From<TxError>,
    {
        transaction(self, options, Some(deadline), (), f).await
    }

    pub async fn in_tx_with_deadline<T, E, F>(
        &self,
        deadline: Instant,
        options: TxOptions,
        f: F,
    ) -> Result<T, E>
    where
        F: AsyncFnOnce(&mut BudgetedTx<'_>) -> Result<T, E>,
        E: From<TxError> + From<OperationDeadlineElapsed>,
    {
        let mode = OperationDeadline {
            at: deadline,
            armed_timeout_ms: None,
            expired: false,
        };
        transaction(self, options, Some(deadline), mode, f).await
    }
}

enum StepFailure<E> {
    Driver { error: TxError, mapped: Option<E> },
    RolledBack(E),
}

impl<E> From<TxError> for StepFailure<E> {
    fn from(error: TxError) -> Self {
        Self::Driver {
            error,
            mapped: None,
        }
    }
}

async fn transaction<M: TxModeExt<E>, T, E, F>(
    pool: &Pool,
    options: TxOptions,
    acquire_deadline: Option<tokio::time::Instant>,
    mode: M,
    f: F,
) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_, M>) -> Result<T, E>,
    E: From<TxError>,
{
    let span = observe::transaction_span(pool);
    let mut observation = TransactionObservation::start(span.clone(), pool.metrics());
    let outcome = async {
        let acquired = Box::pin(pool.acquire_for_transaction(acquire_deadline)).await;
        observation.record_connection_wait();
        let connection = acquired.map_err(|error| {
            let error = TxError::Acquire(error);
            let mapped = mode.acquisition_expired(&error);
            StepFailure::Driver { error, mapped }
        })?;
        let mut guard = DiscardOnDrop {
            connection,
            discard: true,
        };
        let mut tx = match options.begin_statement() {
            None => guard.connection.begin().await,
            Some(statement) => guard.connection.begin_with(statement).await,
        }
        .map_err(TxError::Begin)?;
        guard.discard = false;

        let mut handle = Tx {
            conn: &mut tx,
            proven_alive: true,
            mode,
            notifications: SmallVec::new(),
            next_savepoint: 0,
        };
        let result = f(&mut handle).await;
        handle
            .mode
            .check_expiry()
            .map_err(StepFailure::RolledBack)?;
        let value = result.map_err(StepFailure::RolledBack)?;
        if !(options.read_only || handle.proven_alive) {
            let proof = (&mut handle).execute("SELECT 1").await;
            handle
                .mode
                .check_expiry()
                .map_err(StepFailure::RolledBack)?;
            proof.map_err(TxError::CommitFailed)?;
        }
        handle
            .mode
            .check_before_commit()
            .await
            .map_err(StepFailure::RolledBack)?;
        let notifications = handle.notifications;
        observation.begin_commit();
        tx.commit().await.map_err(classify_commit)?;
        dispatch_notifications(pool, guard, notifications);
        Ok(value)
    }
    .instrument(span)
    .await;
    match outcome {
        Ok(value) => {
            observation.commit();
            Ok(value)
        }
        Err(StepFailure::RolledBack(error)) => {
            observation.roll_back();
            Err(error)
        }
        Err(StepFailure::Driver { error, mapped }) => {
            observation.fail(&error);
            Err(mapped.unwrap_or_else(|| error.into()))
        }
    }
}

fn classify_commit(err: sqlx::Error) -> TxError {
    const CLASS_INTEGRITY_CONSTRAINT_VIOLATION: &str = "23";
    const CLASS_TRANSACTION_ROLLBACK: &str = "40";

    let rejected = sqlstate(&err).is_some_and(|code| {
        code.starts_with(CLASS_INTEGRITY_CONSTRAINT_VIOLATION)
            || (code.starts_with(CLASS_TRANSACTION_ROLLBACK)
                && code != STATEMENT_COMPLETION_UNKNOWN)
    });
    if rejected {
        TxError::CommitFailed(err)
    } else {
        TxError::CommitUnknown(err)
    }
}

#[cfg(test)]
mod tests {
    use futures_util::{StreamExt, stream};

    use super::*;

    #[test]
    fn notification_cooldowns_are_independent_per_channel_and_payload() {
        let now = tokio::time::Instant::now();
        let interval = Duration::from_millis(25);
        let mut last_sent = foldhash::HashMap::default();
        let first = Notification {
            key: NotificationKey {
                channel: "jobs",
                payload: "first",
            },
            min_interval: interval,
        };
        assert!(first.record_if_due(&mut last_sent, now));
        assert!(!first.record_if_due(&mut last_sent, now + interval / 2));
        for key in [
            NotificationKey {
                channel: "jobs",
                payload: "second",
            },
            NotificationKey {
                channel: "another",
                payload: "first",
            },
        ] {
            let other = Notification {
                key,
                min_interval: interval,
            };
            assert!(other.record_if_due(&mut last_sent, now + interval / 2));
        }
        assert!(first.record_if_due(&mut last_sent, now + interval));
    }

    async fn proof_after(items: Vec<Result<u8, sqlx::Error>>, take: usize) -> bool {
        let mut proven_alive = false;
        let mut statement = Proving {
            statement: Box::pin(stream::iter(items)),
            proof_to_restore: Some(&mut proven_alive),
        };
        for _ in 0..take {
            let _ = statement.next().await;
        }
        drop(statement);
        proven_alive
    }

    #[tokio::test]
    async fn only_a_statement_that_ended_without_an_error_proves_the_transaction() {
        assert!(proof_after(vec![Ok(1), Ok(2)], 3).await);
        assert!(proof_after(vec![], 1).await);
        assert!(!proof_after(vec![Ok(1), Ok(2)], 2).await);
        let failed = || vec![Ok(1), Err(sqlx::Error::Protocol("failed".into()))];
        assert!(!proof_after(failed(), 2).await);
        assert!(!proof_after(failed(), 3).await);

        let mut proven_alive = false;
        let mut statement = Proving::<()> {
            statement: Box::pin(stream::pending()),
            proof_to_restore: Some(&mut proven_alive),
        };
        let mut cx = Context::from_waker(std::task::Waker::noop());
        assert!(Pin::new(&mut statement).poll_next(&mut cx).is_pending());
        drop(statement);
        assert!(!proven_alive);
    }

    #[test]
    fn commit_classification_by_sqlstate() {
        for code in ["23505", "23503", "40001", "40P01"] {
            let err = classify_commit(crate::error::tests::database(code));
            std::assert_matches!(err, TxError::CommitFailed(_), "{code}");
        }
        for code in ["40003", "25P02", "57014", "08006", "XX000"] {
            let err = classify_commit(crate::error::tests::database(code));
            std::assert_matches!(err, TxError::CommitUnknown(_), "{code}");
        }
        let err = classify_commit(sqlx::Error::Io(std::io::Error::other("reset")));
        std::assert_matches!(err, TxError::CommitUnknown(_), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn only_caller_limited_cancellation_in_the_rounding_window_reaches_the_cutoff() {
        for (armed_timeout_ms, remaining_us, sqlstate, expires) in [
            (Some(10), 750, "57014", true),
            (Some(10), 1_000, "57014", false),
            (None, 750, "57014", false),
            (Some(10), 750, "23505", false),
        ] {
            let start = Instant::now();
            let mut budget = OperationDeadline {
                at: start + Duration::from_micros(remaining_us),
                armed_timeout_ms,
                expired: false,
            };
            let original = crate::error::tests::database(sqlstate);
            let result = budget.observe_cancellation(Err::<(), _>(original)).await;
            assert_eq!(
                crate::sqlstate(&result.unwrap_err()).as_deref(),
                Some(sqlstate)
            );
            assert_eq!(Instant::now() >= budget.at, expires);
            assert_eq!(budget.expired, expires);
        }
    }

    #[tokio::test]
    async fn collect_query_result_drains_the_stream_and_surfaces_late_errors() {
        let done = collect_query_result(Box::pin(stream::iter(vec![Ok(Either::Left(
            PgQueryResult::default(),
        ))])))
        .await
        .unwrap();
        assert_eq!(done.rows_affected(), 0);

        let late = collect_query_result(Box::pin(stream::iter(vec![
            Ok(Either::Left(PgQueryResult::default())),
            Err(sqlx::Error::Protocol("late".into())),
        ])))
        .await;
        std::assert_matches!(late, Err(sqlx::Error::Protocol(_)));
    }

    #[test]
    fn savepoint_types_default_to_infallible_abort() {
        let keep: SavepointEnd<()> = SavepointEnd::Keep(());
        let rollback: SavepointEnd<()> = SavepointEnd::Rollback(());
        assert_ne!(keep, rollback);
        let begin: SavepointError = SavepointError::Begin(sqlx::Error::RowNotFound);
        assert!(begin.to_string().contains("begin"));
        let aborted = SavepointError::Aborted(OperationDeadlineElapsed);
        assert_eq!(aborted.to_string(), OperationDeadlineElapsed.to_string());
    }
}
