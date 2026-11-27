use std::convert::Infallible;
use std::mem::DropGuard;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use domain_events::{Event, EventPayload};
use futures_util::Stream;
use grpc_contracts::example::v1::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_server::{EchoService, EchoServiceServer},
};
use infra_grpc::{RequestExt as _, Services};
use infra_object_storage::{
    AccessKey, ObjectKey, ObjectStorage, ObjectStorageOptions, Provider, PutBody, PutOptions,
};
use serde::Serialize;
use tokio::sync::{Notify, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tonic::{Request, Response, Status};

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const CPU_BYTES: usize = 65_536;
const CPU_ITEMS: usize = 4096;
const CPU_ROUNDS: u32 = 4096;
const UPLOAD_BYTES: usize = 131_072;
const EMPTY_FRAMES: usize = 4096;
const PAYLOAD_LIMIT: usize = 262_144;
static CPU_SOURCE: [u8; CPU_BYTES] = [7; CPU_BYTES];
static LOG_VALUE: [u8; 4096] = [b'L'; 4096];

type CpuReply = oneshot::Receiver<Result<Vec<u8>, Status>>;

struct CpuOccupancy {
    epoch: Instant,
    completed: Duration,
    active: Option<Instant>,
    started: u64,
    start_delay_total: Duration,
    start_delay_max: Duration,
}

impl CpuOccupancy {
    fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            completed: Duration::ZERO,
            active: None,
            started: 0,
            start_delay_total: Duration::ZERO,
            start_delay_max: Duration::ZERO,
        }
    }

    fn enter(&mut self, now: Instant, admitted: Instant) {
        let delay = now.duration_since(admitted);
        self.started += 1;
        self.start_delay_total += delay;
        self.start_delay_max = self.start_delay_max.max(delay);
        self.active = Some(now);
    }

    fn exit(&mut self, now: Instant) {
        if let Some(started) = self.active.take() {
            self.completed += now.duration_since(started);
        }
    }

    fn snapshot(&self, now: Instant) -> (Duration, Duration) {
        let partial = self
            .active
            .map_or(Duration::ZERO, |started| now.duration_since(started));
        (now.duration_since(self.epoch), self.completed + partial)
    }
}

struct CpuObservation {
    observed_ns: u64,
    occupied_ns: u64,
    started: u64,
    start_delay_total_ns: u64,
    start_delay_max_ns: u64,
}

#[derive(Default)]
struct Counts {
    cpu_admitted: AtomicU64,
    cpu_refused: AtomicU64,
    cpu_completed: AtomicU64,
    cpu_failed: AtomicU64,
    cpu_cancelled: AtomicU64,
    cpu_cancelled_active: AtomicU64,
    cpu_refused_after_cancel: AtomicU64,
    cpu_max_active: AtomicU64,
    cpu_running: AtomicU64,
    cpu_max_ns: AtomicU64,
    active_id: AtomicU64,
    cancelled_id: AtomicU64,
    upload_active: AtomicU64,
    upload_started: AtomicU64,
    upload_max_active: AtomicU64,
    upload_completed: AtomicU64,
    upload_failed: AtomicU64,
    upload_cancelled: AtomicU64,
    prepared: AtomicU64,
    log_attempted: AtomicU64,
}

struct Cpu {
    admission: Arc<Semaphore>,
    registration: Mutex<()>,
    tasks: TaskTracker,
    counts: Arc<Counts>,
    failure: Arc<Notify>,
    occupancy: Arc<Mutex<CpuOccupancy>>,
}

impl Cpu {
    fn new(counts: Arc<Counts>) -> Self {
        Self {
            admission: Arc::new(Semaphore::new(1)),
            registration: Mutex::new(()),
            tasks: TaskTracker::new(),
            counts,
            failure: Arc::new(Notify::new()),
            occupancy: Arc::new(Mutex::new(CpuOccupancy::new(Instant::now()))),
        }
    }

    fn submit(&self) -> Result<(u64, CpuReply), Status> {
        let _registration = self
            .registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| {
                self.counts.cpu_refused.fetch_add(1, Ordering::SeqCst);
                let active = self.counts.active_id.load(Ordering::SeqCst);
                if active != 0 && self.counts.cancelled_id.load(Ordering::SeqCst) == active {
                    self.counts
                        .cpu_refused_after_cancel
                        .fetch_add(1, Ordering::SeqCst);
                }
                Status::resource_exhausted("finite CPU work is occupied or closed")
            })?;
        let id = self.counts.cpu_admitted.fetch_add(1, Ordering::SeqCst) + 1;
        let admitted_at = Instant::now();
        let input = CPU_SOURCE.to_vec();
        let counts = Arc::clone(&self.counts);
        let occupancy = Arc::clone(&self.occupancy);
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _active = ActiveCpu::start(counts, occupancy, id, admitted_at);
            transform_cpu_records(&input)
        });
        let (tx, rx) = oneshot::channel();
        let counts = Arc::clone(&self.counts);
        let failure = Arc::clone(&self.failure);
        let _observer = self.tasks.spawn(async move {
            let result = if let Ok(output) = work.await {
                counts.cpu_completed.fetch_add(1, Ordering::SeqCst);
                Ok(output)
            } else {
                counts.cpu_failed.fetch_add(1, Ordering::SeqCst);
                failure.notify_one();
                Err(Status::internal("finite CPU work failed"))
            };
            let _ = tx.send(result);
        });
        Ok((id, rx))
    }

    fn close(&self) {
        let _registration = self
            .registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.admission.close();
        self.tasks.close();
    }

    async fn join(&self) {
        self.tasks.wait().await;
    }

    fn occupancy_snapshot(&self) -> CpuObservation {
        let occupancy = self
            .occupancy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (observed, occupied) = occupancy.snapshot(Instant::now());
        CpuObservation {
            observed_ns: u64::try_from(observed.as_nanos()).unwrap_or(u64::MAX),
            occupied_ns: u64::try_from(occupied.as_nanos()).unwrap_or(u64::MAX),
            started: occupancy.started,
            start_delay_total_ns: u64::try_from(occupancy.start_delay_total.as_nanos())
                .unwrap_or(u64::MAX),
            start_delay_max_ns: u64::try_from(occupancy.start_delay_max.as_nanos())
                .unwrap_or(u64::MAX),
        }
    }

    async fn manage(
        &self,
        cancel: CancellationToken,
        reporter: service::BackgroundFailureReporter,
    ) -> Result<(), ()> {
        tokio::select! {
            () = cancel.cancelled() => {},
            () = self.failure.notified() => {},
        }
        self.close();
        if self.counts.cpu_failed.load(Ordering::SeqCst) != 0 {
            reporter.report();
        }
        let joined = self.join();
        let mut joined = std::pin::pin!(joined);
        loop {
            tokio::select! {
                () = &mut joined => break,
                () = self.failure.notified() => reporter.report(),
            }
        }
        if self.counts.cpu_failed.load(Ordering::SeqCst) == 0 {
            Ok(())
        } else {
            reporter.report();
            Err(())
        }
    }
}

fn transform_cpu_records(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(CPU_BYTES);
    for record in input.as_chunks::<16>().0.iter().take(CPU_ITEMS) {
        let mut value = u128::from_le_bytes(*record);
        for round in 0..CPU_ROUNDS {
            value = std::hint::black_box(value).rotate_left(7).wrapping_mul(3) ^ u128::from(round);
        }
        output.extend_from_slice(&value.to_le_bytes());
    }
    output
}

struct ActiveCpu {
    counts: Arc<Counts>,
    admitted_at: Instant,
    occupancy: Arc<Mutex<CpuOccupancy>>,
}

impl ActiveCpu {
    fn start(
        counts: Arc<Counts>,
        occupancy: Arc<Mutex<CpuOccupancy>>,
        id: u64,
        admitted_at: Instant,
    ) -> Self {
        {
            let mut interval = occupancy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            interval.enter(Instant::now(), admitted_at);
        }
        counts.active_id.store(id, Ordering::SeqCst);
        let active = counts.cpu_running.fetch_add(1, Ordering::SeqCst) + 1;
        counts.cpu_max_active.fetch_max(active, Ordering::SeqCst);
        Self {
            counts,
            admitted_at,
            occupancy,
        }
    }
}

impl Drop for ActiveCpu {
    fn drop(&mut self) {
        {
            let mut interval = self
                .occupancy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            interval.exit(Instant::now());
        }
        self.counts.cpu_max_ns.fetch_max(
            u64::try_from(self.admitted_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
        self.counts.active_id.store(0, Ordering::SeqCst);
        self.counts.cpu_running.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
struct Echo {
    cpu: Arc<Cpu>,
    storage: ObjectStorage,
    counts: Arc<Counts>,
}

#[derive(Serialize)]
struct PrimitivePayload {
    value: String,
}

static PUBLICATION: std::sync::LazyLock<
    Result<infra_messaging::PublicationRoute<PrimitivePayload>, infra_messaging::RegistryError>,
> = std::sync::LazyLock::new(|| {
    infra_messaging::Registry::default().declare_publication(
        &infra_messaging::Route::new("runtime.progress"),
        PAYLOAD_LIMIT,
    )
});

impl EventPayload for PrimitivePayload {
    const EVENT_TYPE: &'static str = "runtime.progress";
    const SCHEMA_VERSION: u16 = 1;
}

impl Echo {
    async fn cpu(&self) -> Result<String, Status> {
        let (id, result) = self.cpu.submit()?;
        let waiter = DropGuard::new(&self.counts, move |counts| {
            counts.cpu_cancelled.fetch_add(1, Ordering::SeqCst);
            if counts.active_id.load(Ordering::SeqCst) == id {
                counts.cancelled_id.store(id, Ordering::SeqCst);
                counts.cpu_cancelled_active.fetch_add(1, Ordering::SeqCst);
            }
        });
        let result = result
            .await
            .map_err(|_| Status::internal("CPU observer ended"))?;
        DropGuard::dismiss(waiter);
        std::hint::black_box(result?);
        Ok("ok".to_owned())
    }

    async fn upload(&self, request: &Request<UnaryRequest>) -> Result<String, Status> {
        let context = request.operation()?;
        self.counts.upload_started.fetch_add(1, Ordering::SeqCst);
        let active = self.counts.upload_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.counts
            .upload_max_active
            .fetch_max(active, Ordering::SeqCst);
        let counts = &self.counts;
        let mut work = DropGuard::new(false, move |finished| {
            counts.upload_active.fetch_sub(1, Ordering::SeqCst);
            if !finished {
                counts.upload_cancelled.fetch_add(1, Ordering::SeqCst);
            }
        });
        let frames = futures_util::stream::iter((0..=EMPTY_FRAMES).map(|frame| {
            Ok::<_, Infallible>(if frame < EMPTY_FRAMES {
                Bytes::new()
            } else {
                Bytes::from(vec![b'U'; UPLOAD_BYTES])
            })
        }));
        let body = PutBody::stream(UPLOAD_BYTES as u64, Body::from_stream(frames));
        let key = ObjectKey::try_new("runtime-progress")
            .map_err(|_| Status::internal("fixture key rejected"))?;
        let result = self
            .storage
            .put(context, &key, body, PutOptions::overwrite())
            .await;
        *work = true;
        if result == Ok(()) {
            self.counts.upload_completed.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_owned())
        } else {
            self.counts.upload_failed.fetch_add(1, Ordering::SeqCst);
            Err(Status::unavailable("fixture upload failed"))
        }
    }

    fn prepare(&self) -> Result<String, Status> {
        let source = "p".repeat(131_072);
        let event = Event {
            id: "runtime-progress".to_owned(),
            occurred_at: time::UtcDateTime::now(),
            payload: PrimitivePayload { value: source },
        };
        let prepared = PUBLICATION
            .as_ref()
            .map_err(|_| Status::internal("invalid publication route"))?
            .prepare(&event)
            .map_err(|_| Status::internal("bounded preparation failed"))?;
        std::hint::black_box(prepared);
        self.counts.prepared.fetch_add(1, Ordering::SeqCst);
        Ok("ok".to_owned())
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "bounded nanosecond fixture measurement rendered as seconds"
    )]
    fn snapshot(&self) -> String {
        let c = &self.counts;
        let cpu = self.cpu.occupancy_snapshot();
        serde_json::json!({
            "cpu_occupancy_observed_ns": cpu.observed_ns,
            "cpu_occupancy_occupied_ns": cpu.occupied_ns,
            "cpu_start_delay_count": cpu.started,
            "cpu_start_delay_total_ns": cpu.start_delay_total_ns,
            "cpu_start_delay_max_ns": cpu.start_delay_max_ns,
            "cpu_admitted": c.cpu_admitted.load(Ordering::SeqCst),
            "cpu_refused": c.cpu_refused.load(Ordering::SeqCst),
            "cpu_active": c.cpu_running.load(Ordering::SeqCst),
            "cpu_occupied": 1 - self.cpu.admission.available_permits(),
            "cpu_completed": c.cpu_completed.load(Ordering::SeqCst),
            "cpu_failed": c.cpu_failed.load(Ordering::SeqCst),
            "cpu_cancelled": c.cpu_cancelled.load(Ordering::SeqCst),
            "cpu_cancelled_active": c.cpu_cancelled_active.load(Ordering::SeqCst),
            "cpu_refused_after_cancel": c.cpu_refused_after_cancel.load(Ordering::SeqCst),
            "cpu_max_active": c.cpu_max_active.load(Ordering::SeqCst),
            "cpu_max_seconds": c.cpu_max_ns.load(Ordering::SeqCst) as f64 / 1e9,
            "upload_active": c.upload_active.load(Ordering::SeqCst),
            "upload_started": c.upload_started.load(Ordering::SeqCst),
            "upload_max_active": c.upload_max_active.load(Ordering::SeqCst),
            "upload_completed": c.upload_completed.load(Ordering::SeqCst),
            "upload_failed": c.upload_failed.load(Ordering::SeqCst),
            "upload_cancelled": c.upload_cancelled.load(Ordering::SeqCst),
            "prepared": c.prepared.load(Ordering::SeqCst),
            "log_attempted": c.log_attempted.load(Ordering::SeqCst),
            "runtime_workers": tokio::runtime::Handle::current().metrics().num_workers(),
            "cpu_permits": 1,
        })
        .to_string()
    }
}

#[tonic::async_trait]
impl EchoService for Echo {
    async fn unary(
        &self,
        request: Request<UnaryRequest>,
    ) -> Result<Response<UnaryResponse>, Status> {
        let operation = request
            .metadata()
            .get("x-runtime-work")
            .map(|value| value.to_str())
            .transpose()
            .map_err(|_| Status::invalid_argument("invalid fixture operation"))?;
        let message = match operation {
            None => {
                let message = request.into_inner().message;
                if message.len() != 1024 {
                    return Err(Status::invalid_argument("expected 1 KiB"));
                }
                message
            }
            Some("cpu") => self.cpu().await?,
            Some("upload") => self.upload(&request).await?,
            Some("prepare") => self.prepare()?,
            Some("log") => {
                self.counts.log_attempted.fetch_add(1, Ordering::SeqCst);
                let payload = std::str::from_utf8(&LOG_VALUE)
                    .map_err(|_| Status::internal("fixture log bytes"))?;
                tracing::info!(payload, "runtime_progress_pressure");
                "ok".to_owned()
            }
            Some("snapshot") => self.snapshot(),
            Some("freeze") => {
                let until = Instant::now() + Duration::from_secs(9);
                let mut value = 1_u64;
                for _ in 0..100_000_000_000_u64 {
                    value = std::hint::black_box(value).wrapping_mul(3).rotate_left(7);
                    if Instant::now() >= until {
                        break;
                    }
                }
                std::hint::black_box(value);
                "ok".to_owned()
            }
            Some(_) => return Err(Status::invalid_argument("unknown fixture operation")),
        };
        Ok(Response::new(UnaryResponse { message }))
    }

    async fn client_stream(
        &self,
        _: Request<tonic::Streaming<ClientStreamRequest>>,
    ) -> Result<Response<ClientStreamResponse>, Status> {
        Err(Status::unimplemented("unary fixture"))
    }

    type ServerStreamStream =
        Pin<Box<dyn Stream<Item = Result<ServerStreamResponse, Status>> + Send>>;
    async fn server_stream(
        &self,
        _: Request<ServerStreamRequest>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        Err(Status::unimplemented("unary fixture"))
    }

    type BidiStreamStream = Pin<Box<dyn Stream<Item = Result<BidiStreamResponse, Status>> + Send>>;
    async fn bidi_stream(
        &self,
        _: Request<tonic::Streaming<BidiStreamRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        Err(Status::unimplemented("unary fixture"))
    }
}

#[expect(
    clippy::expect_used,
    reason = "non-shipped fixture receives its local provider endpoint from the bounded external driver"
)]
fn register(
    services: &mut Services,
    _state: &service::AppState,
    background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), service::RegistrationError> {
    let counts = Arc::new(Counts::default());
    let cpu = Arc::new(Cpu::new(Arc::clone(&counts)));
    let storage = ObjectStorage::try_new(ObjectStorageOptions {
        provider: Provider::Local {
            endpoint: std::env::var("RUNTIME_PROGRESS_UPLOAD_ENDPOINT").unwrap_or_default(),
            region: Some("us-east-1".to_owned()),
            credentials: AccessKey {
                access_key_id: "fixture".to_owned(),
                secret_access_key: "fixture".into(),
            },
        },
        bucket: "runtime-progress".to_owned(),
        max_object_bytes: UPLOAD_BYTES as u64,
        max_concurrency: 2,
        operation_timeout: Duration::from_secs(8),
    })
    .expect("valid local quota fixture upload endpoint");
    let manager = Arc::clone(&cpu);
    background.spawn("runtime_progress_cpu", move |cancel, reporter| async move {
        manager.manage(cancel, reporter).await
    });
    services.describe(grpc_contracts::FILE_DESCRIPTOR_SET)?;
    #[allow(
        unused_mut,
        unused_variables,
        reason = "the bearer profile registers scope requirements on the handle"
    )]
    let mut echo = services.add(EchoServiceServer::new(Echo {
        cpu,
        storage,
        counts,
    }))?;
    // template:begin grpc-authn:runtime-progress-scope-requirements
    for method in ["Unary", "ClientStream", "ServerStream", "BidiStream"] {
        echo.require_scopes(method, &[])?;
    }
    // template:end grpc-authn:runtime-progress-scope-requirements
    Ok(())
}

fn main() -> Result<ExitCode, &'static infra_messaging::RegistryError> {
    PUBLICATION.as_ref()?;
    Ok(service::run_with_grpc(std::env::args_os(), register))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupancy_includes_active_partial_work_but_excludes_queue_and_idle_time() {
        let epoch = Instant::now();
        let mut occupancy = CpuOccupancy::new(epoch);
        let at = |seconds| epoch + Duration::from_secs(seconds);
        assert_eq!(
            occupancy.snapshot(at(10)),
            (Duration::from_secs(10), Duration::ZERO)
        );
        occupancy.enter(at(10), at(8));
        assert_eq!(
            occupancy.snapshot(at(13)),
            (Duration::from_secs(13), Duration::from_secs(3))
        );
        occupancy.exit(at(15));
        assert_eq!(
            occupancy.snapshot(at(20)),
            (Duration::from_secs(20), Duration::from_secs(5))
        );
        occupancy.enter(at(20), at(17));
        assert_eq!(
            occupancy.snapshot(at(22)),
            (Duration::from_secs(22), Duration::from_secs(7))
        );
        occupancy.exit(at(23));
        assert_eq!(
            occupancy.snapshot(at(30)),
            (Duration::from_secs(30), Duration::from_secs(8))
        );
        assert_eq!(occupancy.started, 2);
        assert_eq!(occupancy.start_delay_total, Duration::from_secs(5));
        assert_eq!(occupancy.start_delay_max, Duration::from_secs(3));
    }
}
