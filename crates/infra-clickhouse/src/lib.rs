mod admission;
mod credentials;
mod error;
mod insert;
mod metadata;
mod observe;
mod query;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;
use std::time::Duration;

use arc_swap::ArcSwap;
use operation_context::{BoundedContext, BoundedStop};
use secrecy::SecretString;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tokio_util::task::{AbortOnDropHandle, TaskTracker};

#[doc(hidden)]
pub use clickhouse as __clickhouse;
pub use clickhouse::Row;
pub mod serde {
    pub use clickhouse::serde::{time, uuid};
}

use self::admission::Admitted;
use self::credentials::ActiveCredentials;
use self::error::StartErrorKind;
pub use self::error::{InsertError, InsertFailure, QueryError, StartError, UnknownOutcome};
pub use self::insert::{
    AsyncDeduplication, AsyncInsertOptions, BatchInsertOptions, InsertReport, WriteRow,
};
use self::metadata::TableSlotRegistry;
pub use self::observe::HISTOGRAMS;
use self::observe::{ClientMetrics, EndpointMetadata};
pub use self::query::{Param, Query, ReadRow};

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum QuerySettings {
    #[default]
    Client,
    Server,
}

#[derive(Clone, Debug)]
pub struct ClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[derive(derive_more::Debug)]
pub struct ClickHouseOptions {
    pub url: String,
    pub database: String,
    pub username: Option<String>,
    #[debug(skip)]
    pub password: Option<SecretString>,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub client_certificate: Option<ClientCertificate>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub query_settings: QuerySettings,
    pub query_timeout: Duration,
    pub insert_timeout: Duration,
    pub max_concurrency: usize,
    pub max_result_rows: u64,
    pub max_result_bytes: usize,
    pub http_response_buffer_size: usize,
    pub max_memory_usage: Option<u64>,
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
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        self.shared.shutdown.cancel();
    }
}

pub(crate) struct Shared {
    credentials: ArcSwap<ActiveCredentials>,
    table_slots: TableSlotRegistry,
    endpoint: EndpointMetadata,
    metrics: ClientMetrics,
    query_settings: QuerySettings,
    query_timeout: Duration,
    insert_timeout: Duration,
    max_result_rows: u64,
    max_result_bytes: usize,
    http_response_buffer_size: usize,
    max_memory_usage: Option<u64>,
    propagate_trace_context: bool,
    permits: Arc<Semaphore>,
    closing: AtomicBool,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

impl ClickHouse {
    pub async fn start(options: ClickHouseOptions) -> Result<Self, StartError> {
        let admission = AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
            admission::admit(&options)
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
        let shared = Arc::new(Shared {
            credentials: ArcSwap::from_pointee(ActiveCredentials {
                cred_epoch: 1,
                read_client: admitted.client,
                password: admitted.password,
            }),
            table_slots: TableSlotRegistry::new(),
            endpoint: admitted.endpoint,
            metrics,
            query_settings: admitted.query_settings,
            query_timeout: admitted.query_timeout,
            insert_timeout: admitted.insert_timeout,
            max_result_rows: admitted.max_result_rows,
            max_result_bytes: admitted.max_result_bytes,
            http_response_buffer_size: admitted.http_response_buffer_size,
            max_memory_usage: admitted.max_memory_usage,
            propagate_trace_context: admitted.propagate_trace_context,
            permits: Arc::new(Semaphore::new(admitted.max_concurrency)),
            closing: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
            tasks: TaskTracker::new(),
        });
        if let Some(password_file) = admitted.password_file {
            shared.tasks.spawn(credentials::run_password_refresh(
                Arc::clone(&shared),
                password_file,
            ));
        }
        Self {
            inner: Arc::new(ClientHandle { shared }),
        }
    }

    pub fn query<'a>(&'a self, summary: &'static str, sql: &'a str) -> Query<'a> {
        Query::new(self, summary, sql)
    }

    pub async fn close(&self, budget: Duration) -> CloseOutcome {
        let shared = &self.inner.shared;
        shared.closing.store(true, Ordering::Release);
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
