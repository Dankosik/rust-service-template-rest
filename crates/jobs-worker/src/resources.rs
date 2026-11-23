use health::ReadinessControl;
use infra_http_transport::Server;
// template:begin jobs:worker-resources-jobs-import
use infra_jobs::Started;
// template:end jobs:worker-resources-jobs-import
// template:begin messaging:worker-resources-messaging-import
use infra_messaging::{ConsumerHandle, MessagingStartup};
// template:end messaging:worker-resources-messaging-import
// template:begin jobs:worker-resources-postgres-import
use infra_postgres::PoolStartup;
// template:end jobs:worker-resources-postgres-import
use infra_telemetry::{LoggerGuard, TracerProviderHandle};

#[derive(Default)]
pub(crate) struct Telemetry {
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
    pub(crate) logger: Option<LoggerGuard>,
}

#[derive(Default)]
pub(crate) struct Work {
    // template:begin jobs:worker-shutdown-resources-started
    pub(crate) started_engine: Option<Started>,
    // template:end jobs:worker-shutdown-resources-started
    // template:begin messaging:worker-shutdown-resources-messaging-consumer
    pub(crate) consumer: Option<ConsumerHandle>,
    // template:end messaging:worker-shutdown-resources-messaging-consumer
}

#[derive(Default)]
pub(crate) struct Dependencies {
    // template:begin jobs:worker-shutdown-resources-pool
    pub(crate) pool: Option<PoolStartup>,
    // template:end jobs:worker-shutdown-resources-pool
    // template:begin messaging:worker-shutdown-resources-messaging-broker
    pub(crate) broker: Option<MessagingStartup>,
    // template:end messaging:worker-shutdown-resources-messaging-broker
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
