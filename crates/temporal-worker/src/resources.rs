use std::pin::Pin;

use health::ReadinessControl;
use infra_http_transport::Server;
use infra_telemetry::{LoggerGuard, TracerProviderHandle};
use infra_temporal::worker::TemporalWorkerHandle;
use infra_temporal::{CloseOutcome, TemporalStartup};
use tokio::time::Instant;

pub(crate) type CloseHook =
    Box<dyn FnOnce(Instant) -> Pin<Box<dyn Future<Output = CloseOutcome> + Send>> + Send>;

#[derive(Default)]
pub(crate) struct Telemetry {
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
    pub(crate) logger: Option<LoggerGuard>,
}

#[derive(Default)]
pub(crate) struct Work {
    pub(crate) temporal: Option<TemporalWorkerHandle>,
}

#[derive(Default)]
pub(crate) struct Dependencies {
    pub(crate) temporal: Option<TemporalStartup>,
    pub(crate) close_hooks: Vec<CloseHook>,
}

#[derive(Debug, Default)]
pub(crate) struct Listeners {
    pub(crate) health: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
}

impl Listeners {
    pub(crate) const HEALTH: &'static str = "health_listener";
    pub(crate) const DIAGNOSTICS: &'static str = "diagnostics_listener";
}

#[derive(Default)]
pub(crate) struct Resources {
    pub(crate) telemetry: Telemetry,
    pub(crate) work: Work,
    pub(crate) dependencies: Dependencies,
    pub(crate) listeners: Listeners,
    pub(crate) readiness: Option<ReadinessControl>,
}
