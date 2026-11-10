//! Job kinds and the engines that claim them.

use futures_util::FutureExt;
use infra_jobs::{Engine, KindError, Kinds, KindsBuilder};
use service_config::Config;

use super::{Ended, RetainedCapabilities, WorkerError, pending_end};
use crate::shutdown::{Background, Resources, Signals};

pub(super) fn validate(kinds: KindsBuilder) -> Result<Option<Kinds>, WorkerError> {
    match kinds.validate() {
        Ok(kinds) => Ok(Some(kinds)),
        Err(KindError::NoKinds) => Ok(None),
        Err(error) => Err(WorkerError::Kinds(error)),
    }
}

pub(super) fn kind_names(registry: Option<&Kinds>) -> String {
    registry
        .map(|registry| registry.names().collect::<Vec<_>>().join(","))
        .unwrap_or_default()
}

pub(super) async fn build_engines(
    config: &Config,
    capabilities: &mut RetainedCapabilities,
    resources: &Resources,
) -> Result<Vec<Engine>, WorkerError> {
    let mut engines = Vec::new();
    if let Some(registry) = capabilities.jobs.take() {
        let engine = Engine::new(
            resources
                .pool
                .as_ref()
                .ok_or(WorkerError::PostgresDisabled)?
                .clone(),
            registry,
            config.jobs.max_workers,
        );
        engine.check_startup().await?;
        engines.push(engine);
    }
    // template:begin outbox:worker-bootstrap-outbox-engine
    let publisher = super::outbox::publisher(resources, engines.first()).await?;
    engines.push(publisher);
    // template:end outbox:worker-bootstrap-outbox-engine
    Ok(engines)
}

pub(super) fn start_engines(
    engines: Vec<Engine>,
    resources: &mut Resources,
    background: &Background,
    signals: &mut Signals,
) -> Option<Ended> {
    for engine in engines {
        if let Some(ended) = pending_end(resources, background, signals) {
            return Some(ended);
        }
        resources
            .started
            .push(engine.start(&background.tracker, &background.cancel));
    }
    tracing::info!(engines = resources.started.len(), "jobs_claiming_started");
    None
}

pub(super) fn engine_failure(resources: &Resources) -> Option<WorkerError> {
    resources
        .started
        .iter()
        .any(|engine| engine.failed().now_or_never().is_some())
        .then_some(WorkerError::EngineStopped)
}

pub(super) async fn engine_stopped(resources: &Resources) {
    if resources.started.is_empty() {
        std::future::pending::<()>().await;
    } else {
        futures_util::future::select_all(
            resources
                .started
                .iter()
                .map(|engine| Box::pin(engine.failed())),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use infra_jobs::{KindError, StartupError};

    use crate::bootstrap::WorkerError;

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::Kinds(KindError::Duplicate("welcome")).to_string(),
            "job kinds are invalid: job kind \"welcome\" is registered twice"
        );
        assert_eq!(
            WorkerError::JobsStartup(StartupError::Unavailable).to_string(),
            "jobs startup check: the jobs store is unavailable"
        );
        assert_eq!(
            WorkerError::EngineStopped.to_string(),
            "the job engine stopped without a stop signal"
        );
    }
}
