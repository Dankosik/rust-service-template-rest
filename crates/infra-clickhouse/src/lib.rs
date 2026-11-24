mod admission;
mod credentials;
mod error;
mod insert;
mod metadata;
mod observe;
mod query;

#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use arc_swap::ArcSwap;
use operation_context::{BoundedContext, BoundedStop};
use secrecy::SecretString;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::{CancellationToken, DropGuard as CancelDropGuard};
use tokio_util::task::{AbortOnDropHandle, TaskTracker};

pub use clickhouse::Row;
pub use clickhouse::types::{Decimal32, Decimal64, Decimal128};

#[doc(hidden)]
pub mod _priv {
    pub use clickhouse::_priv::RowKind;
}

pub mod serde {
    pub use clickhouse::serde::{ipv4, time, uuid, uuid_vec};
}

use self::admission::Admitted;
use self::credentials::ActiveCredentials;
use self::error::StartErrorKind;
pub use self::error::{InsertError, InsertFailure, QueryError, StartError, UnknownOutcome};
pub use self::insert::{InsertOptions, InsertOutcomes, InsertReport, WriteRow};
use self::metadata::TableSlotRegistry;
pub use self::observe::HISTOGRAMS;
use self::observe::{ClientMetrics, EndpointMetadata};
pub use self::query::{Param, Query, ReadRow};

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

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
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
    credentials: ArcSwap<ActiveCredentials>,
    password_refresh_trigger: Option<Notify>,
    table_slots: TableSlotRegistry,
    endpoint: EndpointMetadata,
    metrics: ClientMetrics,
    policy: RuntimePolicy,
    read_permits: Semaphore,
    write_permits: Semaphore,
    shutdown: CancellationToken,
    tasks: TaskTracker,
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

    pub(crate) fn apply_client_budget<T>(
        &self,
        mut target: T,
        remaining: Duration,
        mut with_setting: impl FnMut(T, &'static str, &str) -> T,
    ) -> T {
        let QuerySettings::Client { max_memory_usage } = self.policy.query_settings else {
            return target;
        };
        let timeout_secs = format_execution_timeout_secs(remaining);
        target = with_setting(target, "max_execution_time", &timeout_secs);
        target = with_setting(target, "timeout_overflow_mode", "throw");
        if let Some(mem) = max_memory_usage {
            let mut buf = std::fmt::NumBuffer::new();
            target = with_setting(target, "max_memory_usage", mem.get().format_into(&mut buf));
        }
        target
    }
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
        let password_refresh_trigger = admitted.password_file.as_ref().map(|_| Notify::new());
        let shutdown = CancellationToken::new();
        let shutdown_guard = shutdown.clone().drop_guard();
        let shared = Arc::new(Shared {
            credentials: ArcSwap::from_pointee(ActiveCredentials {
                cred_epoch: 1,
                read_client: admitted.client,
                password: admitted.password,
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
        if let Some(password_file) = admitted.password_file {
            shared.tasks.spawn(credentials::run_password_refresh(
                Arc::clone(&shared),
                password_file,
            ));
        }
        Self {
            inner: Arc::new(ClientHandle {
                shared,
                _shutdown_guard: shutdown_guard,
            }),
        }
    }

    pub fn query<'a>(&'a self, summary: &'static str, sql: impl Into<Cow<'a, str>>) -> Query<'a> {
        Query::new(self, summary, sql.into())
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
