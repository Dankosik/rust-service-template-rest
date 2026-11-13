use health::Readiness;
use infra_http::Server;
// template:begin jobs:worker-resources-jobs-import
use infra_jobs::Started;
// template:end jobs:worker-resources-jobs-import
// template:begin messaging:worker-resources-messaging-import
use infra_messaging::{ConsumerHandle, Messaging, MessagingStartup};
// template:end messaging:worker-resources-messaging-import
// template:begin jobs:worker-resources-postgres-import
use infra_postgres::PgPool;
// template:end jobs:worker-resources-postgres-import
use infra_telemetry::{LoggerGuard, TracerProviderHandle};

#[derive(Debug, Default)]
pub(crate) struct Listeners {
    pub(crate) health: Option<Server>,
    pub(crate) diagnostics: Option<Server>,
}

impl Listeners {
    /// Names each listener's watch task, its stop failure, and its close stage.
    pub(crate) const HEALTH: &'static str = "health_listener";
    pub(crate) const DIAGNOSTICS: &'static str = "diagnostics_listener";
}

/// Retained as each resource opens so shutdown can close a partial startup.
#[derive(Default)]
pub(crate) struct Resources {
    pub(crate) tracer_provider: Option<TracerProviderHandle>,
    pub(crate) logger: Option<LoggerGuard>,
    // template:begin jobs:worker-shutdown-resources-started
    pub(crate) started_engines: Vec<Started>,
    // template:end jobs:worker-shutdown-resources-started
    // template:begin messaging:worker-shutdown-resources-messaging
    pub(crate) consumer: Option<ConsumerHandle>,
    pub(crate) broker: Option<BrokerResource>,
    // template:end messaging:worker-shutdown-resources-messaging
    // template:begin jobs:worker-shutdown-resources-pool
    pub(crate) pool: Option<PoolResource>,
    // template:end jobs:worker-shutdown-resources-pool
    pub(crate) listeners: Listeners,
    pub(crate) readiness: Option<Readiness>,
}

// template:begin jobs:worker-resources-postgres-pool
pub(crate) enum PoolResource {
    Starting(PgPool),
    Admitted(PgPool),
}
// template:end jobs:worker-resources-postgres-pool

// template:begin messaging:worker-resources-broker
pub(crate) enum BrokerResource {
    Starting(Box<MessagingStartup>),
    Admitted(Messaging),
}

impl Resources {
    pub(crate) fn admitted_messaging(&self) -> Option<&Messaging> {
        match self.broker.as_ref() {
            Some(BrokerResource::Admitted(messaging)) => Some(messaging),
            Some(BrokerResource::Starting(_)) | None => None,
        }
    }
}
// template:end messaging:worker-resources-broker
