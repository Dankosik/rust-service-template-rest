//! Finite workload for the whole-process quota proof. This example is not shipped.
//!
//! Every operation enters the existing Echo service through production gRPC
//! admission. The external driver owns offered load and the provider peers.

use std::convert::Infallible;
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
use infra_grpc::Services;
use infra_object_storage::{
    CredentialSource, ObjectKey, ObjectStorage, ObjectStorageOptions, Provider, PutBody, PutOptions,
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

/// Occupied wall duration, including preemption, never CPU-consumption time.
struct CpuOccupancy {
    epoch: Instant,
    completed: Duration,
    active: Option<Instant>,
}

impl CpuOccupancy {
    fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            completed: Duration::ZERO,
            active: None,
        }
    }

    fn enter(&mut self, now: Instant) {
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

/// The registration lock serializes admission with closing the tracker.
/// It is never held across an await or during the actual computation.
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
        let started = Instant::now();
        // Copy/decode/spawn only after non-waiting admission. Fixed input and
        // cardinality are independent of the caller's message and callbacks.
        let input = CPU_SOURCE.to_vec();
        let counts = Arc::clone(&self.counts);
        let occupancy = Arc::clone(&self.occupancy);
        let work = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _active = ActiveCpu::start(Arc::clone(&counts), occupancy, id, started);
            let mut output = Vec::with_capacity(CPU_BYTES);
            for record in input.as_chunks::<16>().0.iter().take(CPU_ITEMS) {
                let mut value = u128::from_le_bytes(*record);
                for round in 0..CPU_ROUNDS {
                    value = std::hint::black_box(value).rotate_left(7).wrapping_mul(3)
                        ^ u128::from(round);
                }
                output.extend_from_slice(&value.to_le_bytes());
            }
            output
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

    fn occupancy_snapshot(&self) -> (u64, u64) {
        let occupancy = self
            .occupancy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // One timestamp under the same lock pairs elapsed time with completed
        // and currently active work; no computation or await holds this lock.
        let (observed, occupied) = occupancy.snapshot(Instant::now());
        (
            u64::try_from(observed.as_nanos()).unwrap_or(u64::MAX),
            u64::try_from(occupied.as_nanos()).unwrap_or(u64::MAX),
        )
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
        // Close and submission use the same gate. A successor already admitted
        // after a failed closure released its permit remains in this tracker.
        self.close();
        if self.counts.cpu_failed.load(Ordering::SeqCst) != 0 {
            reporter.report();
        }
        let joined = self.join();
        tokio::pin!(joined);
        loop {
            tokio::select! {
                () = &mut joined => break,
                () = self.failure.notified() => reporter.report(),
            }
        }
        if self.counts.cpu_failed.load(Ordering::SeqCst) == 0 {
            Ok(())
        } else {
            // Also catches a failure whose notification races the final join.
            // The root's existing absolute deadline/exit mapping owns failure;
            // reporting never certifies actual CPU work completed.
            reporter.report();
            Err(())
        }
    }
}

struct ActiveCpu {
    counts: Arc<Counts>,
    started: Instant,
    occupancy: Arc<Mutex<CpuOccupancy>>,
}

impl ActiveCpu {
    fn start(
        counts: Arc<Counts>,
        occupancy: Arc<Mutex<CpuOccupancy>>,
        id: u64,
        started: Instant,
    ) -> Self {
        {
            let mut interval = occupancy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            interval.enter(Instant::now());
        }
        counts.active_id.store(id, Ordering::SeqCst);
        let active = counts.cpu_running.fetch_add(1, Ordering::SeqCst) + 1;
        counts.cpu_max_active.fetch_max(active, Ordering::SeqCst);
        Self {
            counts,
            started,
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
            u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::SeqCst,
        );
        self.counts.active_id.store(0, Ordering::SeqCst);
        self.counts.cpu_running.fetch_sub(1, Ordering::SeqCst);
    }
}

struct CpuWaiter {
    counts: Arc<Counts>,
    id: u64,
    finished: bool,
}

impl Drop for CpuWaiter {
    fn drop(&mut self) {
        if !self.finished {
            self.counts.cpu_cancelled.fetch_add(1, Ordering::SeqCst);
            if self.counts.active_id.load(Ordering::SeqCst) == self.id {
                self.counts.cancelled_id.store(self.id, Ordering::SeqCst);
                self.counts
                    .cpu_cancelled_active
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

struct UploadWork {
    counts: Arc<Counts>,
    finished: bool,
}

impl Drop for UploadWork {
    fn drop(&mut self) {
        self.counts.upload_active.fetch_sub(1, Ordering::SeqCst);
        if !self.finished {
            self.counts.upload_cancelled.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[derive(Clone)]
struct Echo {
    cpu: Arc<Cpu>,
    storage: ObjectStorage,
    counts: Arc<Counts>,
}

#[derive(Serialize)]
struct PrimitivePayload<'a> {
    value: &'a str,
}

impl EventPayload for PrimitivePayload<'_> {
    const EVENT_TYPE: &'static str = "runtime.progress";
    const SCHEMA_VERSION: u16 = 1;
}

impl Echo {
    async fn cpu(&self) -> Result<String, Status> {
        let (id, result) = self.cpu.submit()?;
        let mut waiter = CpuWaiter {
            counts: Arc::clone(&self.counts),
            id,
            finished: false,
        };
        let result = result
            .await
            .map_err(|_| Status::internal("CPU observer ended"))?;
        waiter.finished = true;
        std::hint::black_box(result?);
        Ok("ok".to_owned())
    }

    async fn upload(&self) -> Result<String, Status> {
        self.counts.upload_started.fetch_add(1, Ordering::SeqCst);
        let active = self.counts.upload_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.counts
            .upload_max_active
            .fetch_max(active, Ordering::SeqCst);
        let mut work = UploadWork {
            counts: Arc::clone(&self.counts),
            finished: false,
        };
        // The driver caps offers at two; the adopted adapter also has two
        // non-waiting slots. This generator ends after exactly 4097 frames.
        let frames = futures_util::stream::iter((0..=EMPTY_FRAMES).map(|frame| {
            Ok::<_, Infallible>(if frame < EMPTY_FRAMES {
                Bytes::new()
            } else {
                Bytes::from(vec![b'U'; UPLOAD_BYTES])
            })
        }));
        let body = PutBody::stream(UPLOAD_BYTES as u64, Body::from_stream(frames));
        let key = ObjectKey::new("runtime-progress")
            .map_err(|_| Status::internal("fixture key rejected"))?;
        let result = self.storage.put(&key, body, PutOptions::default()).await;
        work.finished = true;
        if let Ok(()) = result {
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
            payload: PrimitivePayload { value: &source },
        };
        let prepared =
            infra_messaging::PreparedEvent::prepare("runtime.progress", &event, PAYLOAD_LIMIT)
                .map_err(|_| Status::internal("bounded preparation failed"))?;
        std::hint::black_box(prepared);
        self.counts.prepared.fetch_add(1, Ordering::SeqCst);
        Ok("ok".to_owned())
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded nanosecond fixture measurement rendered as seconds"
    )]
    fn snapshot(&self) -> String {
        let c = &self.counts;
        let (observed_ns, occupied_ns) = self.cpu.occupancy_snapshot();
        serde_json::json!({
            "cpu_occupancy_observed_ns": observed_ns,
            "cpu_occupancy_occupied_ns": occupied_ns,
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
            Some("upload") => self.upload().await?,
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
                // Deliberately wrong execution boundary, only for the finite
                // negative control. It cannot become a shipped handler.
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

#[allow(
    clippy::expect_used,
    reason = "non-shipped fixture receives its local provider endpoint from the bounded external driver"
)]
fn register(
    services: &mut Services,
    _state: &service::AppState,
    background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error> {
    let counts = Arc::new(Counts::default());
    let cpu = Arc::new(Cpu::new(Arc::clone(&counts)));
    let storage = ObjectStorage::new(ObjectStorageOptions {
        provider: Provider::Local {
            endpoint: std::env::var("RUNTIME_PROGRESS_UPLOAD_ENDPOINT").unwrap_or_default(),
            region: "us-east-1".to_owned(),
        },
        bucket: "runtime-progress".to_owned(),
        credentials: CredentialSource::AccessKey {
            access_key_id: "fixture".to_owned(),
            secret_access_key: "fixture".into(),
        },
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
    services.add(EchoServiceServer::new(Echo {
        cpu,
        storage,
        counts,
    }))
}

fn main() -> ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
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
        occupancy.enter(at(10));
        // The operation has not completed, but its elapsed active portion counts.
        assert_eq!(
            occupancy.snapshot(at(13)),
            (Duration::from_secs(13), Duration::from_secs(3))
        );
        occupancy.exit(at(15));
        assert_eq!(
            occupancy.snapshot(at(20)),
            (Duration::from_secs(20), Duration::from_secs(5))
        );
        occupancy.enter(at(20));
        assert_eq!(
            occupancy.snapshot(at(22)),
            (Duration::from_secs(22), Duration::from_secs(7))
        );
        occupancy.exit(at(23));
        assert_eq!(
            occupancy.snapshot(at(30)),
            (Duration::from_secs(30), Duration::from_secs(8))
        );
    }
}
