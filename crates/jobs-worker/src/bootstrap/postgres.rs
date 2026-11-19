use std::num::NonZeroU32;
use std::time::Duration;

use health::Probe;
use infra_postgres::{Dsn, DsnError, PgPool, PoolOptions, PostgresProbe, SessionSettingsSource};
use secrecy::ExposeSecret;
use service_config::{PostgresConfig, PostgresSessionBudgets, WorkerConfig};

use super::{IDENTITY_SUFFIX, METRICS_MAINTENANCE_INTERVAL, WorkerError};
use crate::background::Background;
use crate::resources::{PoolResource, Resources};

const POSTGRES_IDENTIFIER_LIMIT: usize = 63;
const CREDENTIAL_PREPARATION_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn required_pool_config(
    config: &WorkerConfig,
    has_ordinary_jobs: bool,
) -> Result<Option<&PostgresConfig>, WorkerError> {
    #[allow(unused_variables, reason = "retained outbox needs a publisher")]
    let has_publisher = false;
    // template:begin outbox:worker-bootstrap-outbox-pool
    let has_publisher = true;
    // template:end outbox:worker-bootstrap-outbox-pool
    if !has_ordinary_jobs && !has_publisher {
        return Ok(None);
    }
    let postgres = config
        .postgres
        .as_ref()
        .ok_or(WorkerError::PostgresDisabled)?;
    super::jobs::validate_pool_capacity(
        has_ordinary_jobs.then_some(config.jobs.max_workers),
        has_publisher,
        postgres.max_connections,
    )?;
    Ok(Some(postgres))
}

pub(super) async fn admit_pool(
    config: &WorkerConfig,
    postgres: &PostgresConfig,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    let dsn = postgres.dsn.clone();
    let password_file = postgres.password_file.clone();
    let local_development = config.app.is_local_development();
    let pool = open_pool(config, postgres, background, resources, move || {
        Dsn::admit_for_environment(
            dsn.expose_secret(),
            password_file.as_deref(),
            local_development,
        )
    })
    .await?;
    migrate::verify_history(&pool).await?;
    resources.dependencies.pool = Some(PoolResource::Admitted(pool));
    Ok(())
}

async fn open_pool(
    config: &WorkerConfig,
    postgres: &PostgresConfig,
    background: &Background,
    resources: &mut Resources,
    prepare: impl FnOnce() -> Result<Dsn, DsnError> + Send + 'static,
) -> Result<PgPool, WorkerError> {
    let dsn = prepare_dsn(prepare).await?;
    let application_name = application_name(&config.observability.otel.service_name);
    let options = pool_options(
        postgres.session_budgets,
        postgres.max_connections,
        &application_name,
    );
    let pool =
        infra_postgres::prepare_pool_with_notifications(&dsn, &options, postgres.notifications);
    resources.dependencies.pool = Some(PoolResource::Starting(pool.clone()));
    infra_postgres::admit_pool(&pool, &options).await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = postgres.max_connections.get(),
        "postgres_pool_opened"
    );
    spawn_pool_tasks(&pool, &dsn, background);
    Ok(pool)
}

async fn prepare_dsn(
    prepare: impl FnOnce() -> Result<Dsn, DsnError> + Send + 'static,
) -> Result<Dsn, WorkerError> {
    let deadline = tokio::time::Instant::now() + CREDENTIAL_PREPARATION_TIMEOUT;
    let preparation = tokio::task::spawn_blocking(prepare);
    match tokio::time::timeout_at(deadline, preparation).await {
        Ok(result) if tokio::time::Instant::now() < deadline => {
            Ok(result.map_err(|_| WorkerError::PostgresPreparationJoin)??)
        }
        _ => Err(WorkerError::PostgresPreparationTimeout),
    }
}

fn spawn_pool_tasks(pool: &PgPool, dsn: &Dsn, background: &Background) {
    background.spawn("postgres_pool_metrics", |cancel| {
        infra_postgres::record_metrics_periodically(
            pool.clone(),
            "postgres",
            METRICS_MAINTENANCE_INTERVAL,
            cancel,
        )
    });
    if let Some(password_file) = dsn.password_file() {
        let password_file = password_file.to_owned();
        background.spawn("postgres_password_refresh", |cancel| {
            infra_postgres::refresh_password_periodically(pool.clone(), password_file, cancel)
        });
    }
}

pub(crate) fn pool_options(
    session_budgets: PostgresSessionBudgets,
    max_connections: NonZeroU32,
    application_name: &str,
) -> PoolOptions<'_> {
    PoolOptions {
        max_connections,
        application_name,
        default_isolation: infra_postgres::Isolation::ReadCommitted,
        session_settings_source: match session_budgets {
            PostgresSessionBudgets::Startup => SessionSettingsSource::Startup,
            PostgresSessionBudgets::Server => SessionSettingsSource::Server,
        },
        wait_order: infra_postgres::WaitOrder::Arrival,
    }
}

fn application_name(service_name: &str) -> String {
    let end = service_name.floor_char_boundary(POSTGRES_IDENTIFIER_LIMIT - IDENTITY_SUFFIX.len());
    let prefix = service_name.get(..end).unwrap_or(service_name);
    format!("{prefix}{IDENTITY_SUFFIX}")
}

pub(super) fn admitted_pool(resources: &Resources) -> Result<PgPool, WorkerError> {
    match resources.dependencies.pool.as_ref() {
        Some(PoolResource::Admitted(pool)) => Ok(pool.clone()),
        Some(PoolResource::Starting(_)) | None => Err(WorkerError::PostgresDisabled),
    }
}

pub(super) fn probe(resources: &Resources) -> Option<Box<dyn Probe>> {
    let pool = admitted_pool(resources).ok()?;
    Some(Box::new(PostgresProbe::writable_primary(pool)))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use infra_postgres::Dsn;
    use service_config::WorkerConfig;

    use super::{application_name, open_pool, probe};
    use crate::background::Background;
    use crate::bootstrap::{WorkerError, worker_identity};
    use crate::resources::Resources;

    const DSN: &str = "postgres://worker:fixture@127.0.0.1:5432/example?sslmode=verify-full";
    const WAIT: Duration = Duration::from_secs(5);

    fn config() -> WorkerConfig {
        service_config::fixtures::worker(
            &[
                ("APP__POSTGRES__ENABLED", "true"),
                ("APP__POSTGRES__DSN", DSN),
            ],
            crate::BUILD_INFO,
            crate::shutdown::TAIL,
        )
        .unwrap()
    }

    #[test]
    fn stopped_or_expired_preparation_cannot_admit_a_late_pool() {
        let config = config();
        let postgres = config.postgres.as_ref().unwrap();
        for (expired, result_ready) in [(false, false), (true, false), (true, true)] {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .max_blocking_threads(1)
                .build()
                .unwrap();
            let (began, started) = mpsc::channel();
            let (release, wait) = mpsc::channel();
            let mut resources = Resources::default();
            let background = Background::new();
            let dsn = Dsn::admit(DSN).unwrap();
            runtime.block_on(async {
                let admission =
                    open_pool(&config, postgres, &background, &mut resources, move || {
                        began.send(()).unwrap();
                        wait.recv_timeout(WAIT)
                            .expect("release after stop or expiry");
                        Ok(dsn)
                    });
                tokio::pin!(admission);
                assert!(futures_util::poll!(&mut admission).is_pending());
                started.recv_timeout(WAIT).expect("preparation started");
                if expired {
                    tokio::time::advance(Duration::from_secs(4)).await;
                    assert!(futures_util::poll!(&mut admission).is_pending());
                    tokio::time::advance(Duration::from_millis(1001)).await;
                    if result_ready {
                        release.send(()).unwrap();
                        tokio::task::spawn_blocking(|| ()).await.unwrap();
                    }
                    std::assert_matches!(
                        admission.await,
                        Err(WorkerError::PostgresPreparationTimeout)
                    );
                } else {
                    tokio::select! {
                        biased;
                        () = std::future::ready(()) => {}
                        _ = &mut admission => panic!("startup finished before stop"),
                    }
                }
            });
            assert!(resources.dependencies.pool.is_none());
            if !result_ready {
                release.send(()).unwrap();
            }
            drop(runtime);
            assert!(resources.dependencies.pool.is_none());
            assert!(probe(&resources).is_none());
            assert!(background.tracker.is_empty());
        }
    }

    #[tokio::test]
    async fn failed_preparation_task_withholds_its_panic_payload() {
        let config = config();
        let postgres = config.postgres.as_ref().unwrap();
        let mut resources = Resources::default();
        let background = Background::new();
        let error = open_pool(&config, postgres, &background, &mut resources, || {
            panic!("synthetic preparation payload")
        })
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "postgres credential preparation task failed"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert!(resources.dependencies.pool.is_none());
    }

    #[test]
    fn identity_keeps_the_suffix_inside_the_postgres_limit() {
        assert_eq!(worker_identity("service"), "service-jobs-worker");
        let cases = [
            ("service".to_owned(), "service-jobs-worker".to_owned()),
            ("a".repeat(64), format!("{}-jobs-worker", "a".repeat(51))),
            ("a".repeat(51), format!("{}-jobs-worker", "a".repeat(51))),
            (
                format!("{}é", "a".repeat(50)),
                format!("{}-jobs-worker", "a".repeat(50)),
            ),
            (
                format!("{}é", "a".repeat(49)),
                format!("{}é-jobs-worker", "a".repeat(49)),
            ),
            ("あ".repeat(20), format!("{}-jobs-worker", "あ".repeat(17))),
        ];
        for (input, expected) in cases {
            let name = application_name(&input);
            assert_eq!(name, expected, "{input}");
            assert!(name.ends_with("-jobs-worker"), "{name}");
            assert!(name.len() <= 63, "{}", name.len());
        }
    }
}
