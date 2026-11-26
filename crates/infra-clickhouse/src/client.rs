use std::borrow::Cow;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use arc_swap::ArcSwap;
use operation_context::{BoundedContext, BoundedStop, OperationContext};
use secrecy::SecretString;
use tokio::sync::{Notify, Semaphore, SemaphorePermit};
use tokio_util::sync::{CancellationToken, DropGuard as CancelDropGuard};
use tokio_util::task::task_tracker::TaskTrackerToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};

use crate::admission::{self, Admitted};
use crate::credentials::{self, ActiveCredentials, CredEpoch};
use crate::error::{
    InsertError, ObservedFailure, QueryError, StartError, StartErrorKind, is_auth_failure,
};
use crate::insert::{self, InsertOptions, InsertReport, WriteRow};
use crate::metadata::TableSlotRegistry;
use crate::observe::{
    self, ClientMetrics, EndpointMetadata, InsertGuard, InsertKind, Operation, ReadGuard, ReadKind,
    ReadStats,
};
use crate::query::Query;

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuerySettings {
    Client {
        max_memory_usage: Option<NonZeroU64>,
    },
    Server,
}

impl Default for QuerySettings {
    fn default() -> Self {
        Self::Client {
            max_memory_usage: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[derive(Clone, derive_more::Debug)]
pub enum ClickHouseAuth {
    Password {
        username: String,
        #[debug(skip)]
        password: SecretString,
    },
    PasswordFile {
        username: String,
        path: PathBuf,
    },
    Unauthenticated {
        username: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportSecurity {
    Tls {
        root_ca_path: Option<PathBuf>,
        client_certificate: Option<ClientCertificate>,
    },
    Plaintext,
}

impl Default for TransportSecurity {
    fn default() -> Self {
        Self::Tls {
            root_ca_path: None,
            client_certificate: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClickHouseOptions {
    pub url: String,
    pub database: String,
    pub auth: ClickHouseAuth,
    pub transport: TransportSecurity,
    pub query_settings: QuerySettings,
    pub query_timeout: Duration,
    pub insert_timeout: Duration,
    pub max_read_concurrency: NonZeroUsize,
    pub max_write_concurrency: NonZeroUsize,
    pub max_result_rows: NonZeroU64,
    pub max_result_bytes: NonZeroUsize,
    pub http_response_buffer_size: NonZeroUsize,
    pub propagate_trace_context: bool,
}

#[must_use = "inspect close outcome to log or handle incomplete drain"]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    Complete,
    TimedOut,
}

#[derive(Clone, derive_more::Debug)]
#[debug("ClickHouse {{ .. }}")]
pub struct ClickHouse {
    inner: Arc<ClientHandle>,
}

struct ClientHandle {
    shared: Arc<Shared>,
    _shutdown_guard: CancelDropGuard,
}

pub(crate) struct RuntimePolicy {
    pub(crate) query_settings: QuerySettings,
    pub(crate) query_timeout: Duration,
    pub(crate) insert_timeout: Duration,
    pub(crate) max_result_rows: NonZeroU64,
    pub(crate) max_result_bytes: NonZeroUsize,
    pub(crate) http_response_buffer_size: NonZeroUsize,
    pub(crate) propagate_trace_context: bool,
}

pub(crate) struct Shared {
    pub(crate) credentials: ArcSwap<ActiveCredentials>,
    password_refresh_trigger: Option<Arc<Notify>>,
    pub(crate) table_slots: TableSlotRegistry,
    pub(crate) endpoint: EndpointMetadata,
    pub(crate) metrics: ClientMetrics,
    pub(crate) policy: RuntimePolicy,
    read_permits: Semaphore,
    write_permits: Semaphore,
    pub(crate) shutdown: CancellationToken,
    pub(crate) tasks: TaskTracker,
}

pub(crate) enum AdmitFailure {
    Closing,
    Busy,
}

pub(crate) struct AdmittedRead<'a> {
    guard: ReadGuard<'a>,
    _task_token: TaskTrackerToken,
    _permit: SemaphorePermit<'a>,
    shared: &'a Shared,
}

impl<'a> AdmittedRead<'a> {
    pub(crate) fn guard(&self) -> &ReadGuard<'a> {
        &self.guard
    }

    pub(crate) fn guard_mut(&mut self) -> &mut ReadGuard<'a> {
        &mut self.guard
    }

    pub(crate) fn finish<T>(
        self,
        result: Result<(T, ReadStats), ObservedFailure<QueryError>>,
    ) -> Result<T, QueryError> {
        if let Err(failure) = &result
            && is_auth_failure(failure.detail)
        {
            self.shared.notify_auth_failure();
        }
        self.guard.finish(result)
    }
}

pub(crate) struct AdmittedInsert<'a> {
    guard: InsertGuard<'a>,
    _task_token: TaskTrackerToken,
    _permit: SemaphorePermit<'a>,
    shared: &'a Shared,
}

impl<'a> AdmittedInsert<'a> {
    pub(crate) fn guard(&self) -> &InsertGuard<'a> {
        &self.guard
    }

    pub(crate) fn guard_mut(&mut self) -> &mut InsertGuard<'a> {
        &mut self.guard
    }

    pub(crate) fn finish(
        self,
        result: Result<InsertReport, ObservedFailure<InsertError>>,
    ) -> Result<InsertReport, InsertError> {
        if let Err(failure) = &result
            && is_auth_failure(failure.detail)
        {
            self.shared.notify_auth_failure();
        }
        self.guard.finish(result)
    }
}

impl Shared {
    pub(crate) fn is_closing(&self) -> bool {
        self.shutdown.is_cancelled() || self.tasks.is_closed()
    }

    pub(crate) fn notify_auth_failure(&self) {
        if let Some(trigger) = &self.password_refresh_trigger {
            trigger.notify_one();
        }
    }

    fn acquire_slot(
        &self,
        operation: Operation,
    ) -> Result<(SemaphorePermit<'_>, TaskTrackerToken), AdmitFailure> {
        let permits = match operation {
            Operation::Read(_) => &self.read_permits,
            Operation::Insert(_) => &self.write_permits,
        };
        let Ok(permit) = permits.try_acquire() else {
            self.metrics.record_admission_refused(operation);
            return Err(AdmitFailure::Busy);
        };
        let task_token = self.tasks.token();
        if self.is_closing() {
            return Err(AdmitFailure::Closing);
        }
        Ok((permit, task_token))
    }

    pub(crate) fn admit_read(
        &self,
        kind: ReadKind,
        summary: &'static str,
    ) -> Result<AdmittedRead<'_>, AdmitFailure> {
        let (permit, task_token) = self.acquire_slot(Operation::Read(kind))?;
        let guard = ReadGuard::start(&self.metrics, kind, summary, &self.endpoint);
        Ok(AdmittedRead {
            guard,
            _task_token: task_token,
            _permit: permit,
            shared: self,
        })
    }

    pub(crate) fn admit_insert(
        &self,
        mode: InsertKind,
        summary: &'static str,
    ) -> Result<AdmittedInsert<'_>, AdmitFailure> {
        let (permit, task_token) = self.acquire_slot(Operation::Insert(mode))?;
        let guard = InsertGuard::start(&self.metrics, mode, summary, &self.endpoint);
        Ok(AdmittedInsert {
            guard,
            _task_token: task_token,
            _permit: permit,
            shared: self,
        })
    }
}

pub(crate) fn apply_client_budget_settings<T>(
    mut target: T,
    max_memory_usage: Option<NonZeroU64>,
    timeout_secs: &str,
    mut with_setting: impl FnMut(T, &'static str, &str) -> T,
) -> T {
    target = with_setting(target, "max_execution_time", timeout_secs);
    target = with_setting(target, "timeout_overflow_mode", "throw");
    if let Some(mem) = max_memory_usage {
        let mut buf = std::fmt::NumBuffer::new();
        target = with_setting(target, "max_memory_usage", mem.get().format_into(&mut buf));
    }
    target
}

pub(crate) fn format_execution_timeout_secs(remaining: Duration) -> String {
    let millis = remaining.as_millis().max(1);
    format!("{}.{:03}", millis / 1_000, millis % 1_000)
}

impl ClickHouse {
    pub async fn start(options: ClickHouseOptions) -> Result<Self, StartError> {
        let admission = AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
            admission::admit(options)
        }));
        let admitted = tokio::time::timeout(ADMISSION_TIMEOUT, admission)
            .await
            .map_err(|_| StartError(StartErrorKind::AdmissionTimedOut))?
            .map_err(|_| StartError(StartErrorKind::ClientBuild))?
            .map_err(StartError)?;
        Ok(Self::from_admitted(admitted))
    }

    fn from_admitted(admitted: Admitted) -> Self {
        observe::describe();
        let metrics = ClientMetrics::register();
        let (initial_password, password_refresh) = match admitted.password_refresh {
            Some(seed) => (
                Some(seed.initial_password),
                Some((seed.path, Arc::new(Notify::new()))),
            ),
            None => (None, None),
        };
        let password_refresh_trigger = password_refresh
            .as_ref()
            .map(|(_, trigger)| Arc::clone(trigger));
        let shutdown = CancellationToken::new();
        let shutdown_guard = shutdown.clone().drop_guard();
        let shared = Arc::new(Shared {
            credentials: ArcSwap::from_pointee(ActiveCredentials {
                cred_epoch: CredEpoch::INITIAL,
                read_client: admitted.client,
                password: initial_password,
            }),
            password_refresh_trigger,
            table_slots: TableSlotRegistry::new(),
            endpoint: admitted.endpoint,
            metrics,
            policy: admitted.policy,
            read_permits: Semaphore::new(admitted.max_read_concurrency.get()),
            write_permits: Semaphore::new(admitted.max_write_concurrency.get()),
            shutdown,
            tasks: TaskTracker::new(),
        });
        if let Some((password_file, trigger)) = password_refresh {
            shared.tasks.spawn(credentials::run_password_refresh(
                Arc::clone(&shared),
                password_file,
                trigger,
            ));
        }
        Self {
            inner: Arc::new(ClientHandle {
                shared,
                _shutdown_guard: shutdown_guard,
            }),
        }
    }

    pub(crate) fn shared(&self) -> &Shared {
        &self.inner.shared
    }

    pub fn query<'a>(&'a self, summary: &'static str, sql: impl Into<Cow<'a, str>>) -> Query<'a> {
        Query::new(self, summary, sql.into())
    }

    pub async fn insert<'r, R: WriteRow<'r>>(
        &self,
        ctx: &OperationContext,
        summary: &'static str,
        table: &str,
        rows: &[R],
        options: InsertOptions<'_>,
    ) -> Result<InsertReport, InsertError> {
        insert::execute(self.shared(), ctx, summary, table, rows, options).await
    }

    pub async fn close(&self, budget: Duration) -> CloseOutcome {
        let shared = &self.inner.shared;
        shared.shutdown.cancel();
        shared.tasks.close();
        if tokio::time::timeout(budget, shared.tasks.wait())
            .await
            .is_ok()
        {
            CloseOutcome::Complete
        } else {
            CloseOutcome::TimedOut
        }
    }
}

pub(crate) async fn within_bounded<T>(
    bounded: &BoundedContext,
    future: impl Future<Output = T>,
) -> Result<T, BoundedStop> {
    let mut future = pin!(future);
    let mut stopped = pin!(bounded.wait_stopped());
    std::future::poll_fn(|cx| {
        if let Some(reason) = bounded.stopped() {
            return Poll::Ready(Err(reason));
        }
        if let Poll::Ready(reason) = stopped.as_mut().poll(cx) {
            return Poll::Ready(Err(reason));
        }
        future.as_mut().poll(cx).map(Ok)
    })
    .await
}
