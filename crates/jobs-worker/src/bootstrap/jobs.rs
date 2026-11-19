use std::num::NonZeroU32;

use infra_jobs::{Engine, KindError, Kinds, KindsBuilder};
use itertools::Itertools as _;
use service_config::{ValidationError, WorkerConfig};

use super::WorkerError;
use crate::background::Background;
use crate::resources::Resources;

pub(super) fn validate(kinds: KindsBuilder) -> Result<Option<Kinds>, WorkerError> {
    match kinds.validate() {
        Ok(kinds) => Ok(Some(kinds)),
        Err(KindError::NoKinds) => Ok(None),
        Err(error) => Err(WorkerError::Kinds(error)),
    }
}

pub(super) fn kind_names(kinds: Option<&Kinds>) -> String {
    kinds.map_or_default(|kinds| kinds.names().join(","))
}

pub(super) fn validate_pool_capacity(
    workers: Option<NonZeroU32>,
    publisher: bool,
    max_connections: NonZeroU32,
) -> Result<(), ValidationError> {
    const MANAGEMENT_CONNECTIONS: u64 = 2;
    const PUBLISHER_CONNECTIONS: u64 = 3;
    let publisher_connections = if publisher { PUBLISHER_CONNECTIONS } else { 0 };
    let required = workers.map_or(0, |workers| {
        u64::from(workers.get()) + MANAGEMENT_CONNECTIONS
    }) + publisher_connections;
    if u64::from(max_connections.get()) < required {
        let formula = if workers.is_some() {
            format!(
                "jobs.max_workers + {}",
                MANAGEMENT_CONNECTIONS + publisher_connections
            )
        } else {
            required.to_string()
        };
        let role = if publisher { "outbox" } else { "jobs" };
        return Err(ValidationError::new(
            "postgres.max_connections",
            format!(
                "must be at least {formula} ({required}) for the {role} worker, got {max_connections}"
            ),
        ));
    }
    Ok(())
}

pub(super) async fn build_engine(
    config: &WorkerConfig,
    kinds: Option<Kinds>,
    resources: &Resources,
) -> Result<Option<Engine>, WorkerError> {
    let engine = match kinds {
        Some(kinds) => Some(Engine::new(
            super::postgres::admitted_pool(resources)?,
            kinds,
            config.jobs().max_workers,
        )),
        None => None,
    };
    // template:begin outbox:worker-bootstrap-outbox-engine
    let engine = Some(super::outbox::with_publisher(resources, engine)?);
    // template:end outbox:worker-bootstrap-outbox-engine
    if let Some(engine) = &engine {
        engine.check_startup().await?;
    }
    Ok(engine)
}

pub(super) fn start_engine(engine: Engine, resources: &mut Resources, background: &Background) {
    resources.work.started_engine = Some(engine.start(&background.tracker, &background.cancel));
    tracing::info!("jobs_claiming_started");
}

pub(super) async fn engine_failure(resources: &Resources) -> WorkerError {
    match &resources.work.started_engine {
        Some(engine) => {
            engine.failed().await;
            WorkerError::EngineStopped
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use infra_jobs::{KindError, StartupError};

    use super::validate_pool_capacity;
    use crate::bootstrap::WorkerError;

    #[test]
    fn pool_capacity_refuses_below_the_minimum_for_registered_engines() {
        for (workers, publisher, minimum, diagnostic) in [
            (
                Some(1),
                false,
                3,
                "jobs.max_workers + 2 (3) for the jobs worker",
            ),
            (
                Some(8),
                false,
                10,
                "jobs.max_workers + 2 (10) for the jobs worker",
            ),
            (
                Some(500),
                false,
                502,
                "jobs.max_workers + 2 (502) for the jobs worker",
            ),
            // template:begin outbox:jobs-outbox-capacity-tests
            (None, true, 3, "3 (3) for the outbox worker"),
            (
                Some(1),
                true,
                6,
                "jobs.max_workers + 5 (6) for the outbox worker",
            ),
            (
                Some(8),
                true,
                13,
                "jobs.max_workers + 5 (13) for the outbox worker",
            ),
            // template:end outbox:jobs-outbox-capacity-tests
        ] {
            let workers = workers.map(|count| NonZeroU32::new(count).unwrap());
            let below = NonZeroU32::new(minimum - 1).unwrap();
            let error = validate_pool_capacity(workers, publisher, below).unwrap_err();
            assert_eq!(error.key, "postgres.max_connections");
            assert_eq!(
                error.message,
                format!("must be at least {diagnostic}, got {below}")
            );
            validate_pool_capacity(workers, publisher, NonZeroU32::new(minimum).unwrap()).unwrap();
        }
    }

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
