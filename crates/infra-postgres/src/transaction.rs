use std::collections::BTreeMap;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::Stream;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{
    PgConnection, PgPool, PgQueryResult, PgRow, PgStatement, PgTypeInfo, Postgres,
};
use sqlx::{Connection, Either, Execute, Executor, SqlStr};
use tracing::Instrument;

use crate::error::sqlstate;
use crate::observe::{self, TransactionObservation, TransactionOutcome, observed};
use crate::pool::{acquire, acquire_until, format_pg_duration};

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
pub struct Tx<'c> {
    conn: &'c mut PgConnection,
    proven_alive: bool,
    notifications: Vec<Notification>,
}

impl Tx<'_> {
    pub fn notify_after_commit(
        &mut self,
        channel: &'static str,
        payload: &'static str,
        min_interval: Duration,
    ) {
        const MAX_HINTS: usize = 32;
        let key = (channel, payload);
        if self.notifications.len() < MAX_HINTS
            && !self.notifications.iter().any(|hint| hint.key == key)
        {
            self.notifications.push(Notification { key, min_interval });
        }
    }

    pub fn connection(&mut self) -> &mut PgConnection {
        self.proven_alive = false;
        self.conn
    }
}

type NotificationKey = (&'static str, &'static str);

#[derive(Debug)]
struct Notification {
    key: NotificationKey,
    min_interval: Duration,
}

impl Notification {
    fn take_slot(
        &self,
        last_sent: &mut BTreeMap<NotificationKey, tokio::time::Instant>,
        now: tokio::time::Instant,
    ) -> bool {
        match last_sent.entry(self.key) {
            std::collections::btree_map::Entry::Occupied(entry)
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
    pool: &PgPool,
    mut guard: DiscardOnDrop,
    mut notifications: Vec<Notification>,
) {
    use tokio::time::Instant;

    const SEND_BUDGET: Duration = Duration::from_millis(100);
    static LAST_SENT: parking_lot::Mutex<BTreeMap<NotificationKey, Instant>> =
        parking_lot::Mutex::new(BTreeMap::new());

    if notifications.is_empty() || !crate::notifications_enabled(pool) {
        return;
    }
    let now = Instant::now();
    let mut last_sent = LAST_SENT.lock();
    notifications.retain(|hint| hint.take_slot(&mut last_sent, now));
    drop(last_sent);
    if notifications.is_empty() {
        return;
    }
    let closing = pool.close_event();
    tokio::spawn(async move {
        guard.discard = true;
        let send = async {
            for hint in notifications {
                let (channel, payload) = hint.key;
                observed(
                    "post-commit notification",
                    sqlx::query!("SELECT pg_notify($1, $2)", channel, payload)
                        .execute(&mut *guard.connection),
                )
                .await?;
            }
            Ok::<(), sqlx::Error>(())
        };
        tokio::select! {
            biased;
            () = closing => {}
            result = tokio::time::timeout_at(now + SEND_BUDGET, send) => match result {
                Ok(Ok(())) => guard.discard = false,
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

pub async fn set_statement_timeout(tx: &mut Tx<'_>, timeout: Duration) -> Result<(), sqlx::Error> {
    observed(
        "set statement timeout",
        sqlx::query!(
            "SELECT set_config('statement_timeout', $1, true)",
            format_pg_duration(timeout)
        )
        .fetch_one(&mut *tx),
    )
    .await?;
    Ok(())
}

type DriverFuture<'e, T> = Pin<Box<dyn Future<Output = Result<T, sqlx::Error>> + Send + 'e>>;
type DriverStream<'e, T> = Pin<Box<dyn Stream<Item = Result<T, sqlx::Error>> + Send + 'e>>;

impl<'c> Executor<'c> for &'c mut Tx<'_> {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E>(self, query: E) -> DriverStream<'e, Either<PgQueryResult, PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        Box::pin(Proving {
            statement: self.conn.fetch_many(query),
            proof_to_restore: Some(&mut self.proven_alive),
        })
    }

    fn fetch_optional<'e, 'q: 'e, E>(self, query: E) -> DriverFuture<'e, Option<PgRow>>
    where
        'c: 'e,
        E: 'q + Execute<'q, Postgres>,
    {
        self.proven_alive = false;
        let statement = self.conn.fetch_optional(query);
        let proven_alive = &mut self.proven_alive;
        Box::pin(async move {
            let row = statement.await?;
            *proven_alive = true;
            Ok(row)
        })
    }

    fn prepare_with<'e>(
        self,
        sql: SqlStr,
        parameters: &'e [PgTypeInfo],
    ) -> DriverFuture<'e, PgStatement>
    where
        'c: 'e,
    {
        self.proven_alive = false;
        self.conn.prepare_with(sql, parameters)
    }

    fn describe<'e>(self, sql: SqlStr) -> DriverFuture<'e, sqlx::Describe<Postgres>>
    where
        'c: 'e,
    {
        self.proven_alive = false;
        self.conn.describe(sql)
    }
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

pub async fn in_tx<T, E, F>(pool: &PgPool, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    in_tx_with(pool, TxOptions::default(), f).await
}

pub async fn in_tx_with<T, E, F>(pool: &PgPool, options: TxOptions, f: F) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    transaction(pool, options, None, f).await
}

pub async fn in_tx_with_acquire_deadline<T, E, F>(
    pool: &PgPool,
    deadline: tokio::time::Instant,
    options: TxOptions,
    f: F,
) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    transaction(pool, options, Some(deadline), f).await
}

async fn transaction<T, E, F>(
    pool: &PgPool,
    options: TxOptions,
    deadline: Option<tokio::time::Instant>,
    f: F,
) -> Result<T, E>
where
    F: AsyncFnOnce(&mut Tx<'_>) -> Result<T, E>,
    E: From<TxError>,
{
    let span = observe::transaction_span(pool);
    let mut observed = TransactionObservation::start(span.clone());
    async {
        let acquired = Box::pin(async {
            match deadline {
                Some(deadline) => acquire_until(pool, "transaction", deadline).await,
                None => acquire(pool, "transaction").await,
            }
        })
        .await;
        observed.waited();
        let connection = acquired.map_err(|err| observed.fail(TxError::Acquire(err)))?;
        let mut guard = DiscardOnDrop {
            connection,
            discard: true,
        };
        let mut tx = match options.begin_statement() {
            None => guard.connection.begin().await,
            Some(statement) => guard.connection.begin_with(statement).await,
        }
        .map_err(|err| observed.fail(TxError::Begin(err)))?;
        guard.discard = false;

        let mut handle = Tx {
            conn: &mut tx,
            proven_alive: true,
            notifications: Vec::new(),
        };
        let value = match f(&mut handle).await {
            Ok(value) => value,
            Err(err) => {
                observed.end(TransactionOutcome::RolledBack);
                return Err(err);
            }
        };
        let notifications = handle.notifications;
        if !(options.read_only || handle.proven_alive) {
            (&mut *tx)
                .execute("SELECT 1")
                .await
                .map_err(|err| observed.fail(TxError::CommitFailed(err)))?;
        }
        observed.begin_commit();
        tx.commit()
            .await
            .map_err(|err| observed.fail(classify_commit(err)))?;
        observed.end(TransactionOutcome::Committed);
        dispatch_notifications(pool, guard, notifications);
        Ok(value)
    }
    .instrument(span)
    .await
}

fn classify_commit(err: sqlx::Error) -> TxError {
    let rejected = sqlstate(&err)
        .is_some_and(|code| code.starts_with("23") || (code.starts_with("40") && code != "40003"));
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
        let mut last_sent = BTreeMap::new();
        let first = Notification {
            key: ("jobs", "first"),
            min_interval: interval,
        };
        assert!(first.take_slot(&mut last_sent, now));
        assert!(!first.take_slot(&mut last_sent, now + interval / 2));
        for key in [("jobs", "second"), ("another", "first")] {
            let other = Notification {
                key,
                min_interval: interval,
            };
            assert!(other.take_slot(&mut last_sent, now + interval / 2));
        }
        assert!(first.take_slot(&mut last_sent, now + interval));
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
}
