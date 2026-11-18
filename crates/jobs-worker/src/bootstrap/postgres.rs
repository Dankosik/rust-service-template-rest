use std::num::NonZeroU32;

use health::Probe;
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe, SessionSettingsSource};
use secrecy::ExposeSecret;
use service_config::{PostgresConfig, PostgresSessionBudgets, WorkerConfig};

use super::{IDENTITY_SUFFIX, METRICS_MAINTENANCE_INTERVAL, WorkerError};
use crate::background::Background;
use crate::resources::{PoolResource, Resources};

const POSTGRES_IDENTIFIER_LIMIT: usize = 63;

pub(super) fn pool_config(
    config: &WorkerConfig,
    ordinary_jobs: bool,
) -> Result<Option<&PostgresConfig>, WorkerError> {
    #[allow(unused_variables, reason = "retained outbox needs a publisher")]
    let publisher = false;
    // template:begin outbox:worker-bootstrap-outbox-pool
    let publisher = true;
    // template:end outbox:worker-bootstrap-outbox-pool
    if !ordinary_jobs && !publisher {
        return Ok(None);
    }
    let postgres = config
        .postgres
        .as_ref()
        .ok_or(WorkerError::PostgresDisabled)?;
    super::jobs::validate_pool_capacity(
        ordinary_jobs.then_some(config.jobs.max_workers),
        publisher,
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
    let pool = open_pool(config, postgres, background, resources).await?;
    migrate::verify_history(&pool).await?;
    resources.pool = Some(PoolResource::Admitted(pool));
    Ok(())
}

async fn open_pool(
    config: &WorkerConfig,
    postgres: &PostgresConfig,
    background: &Background,
    resources: &mut Resources,
) -> Result<PgPool, WorkerError> {
    let dsn = Dsn::admit_for_environment(
        postgres.dsn.expose_secret(),
        postgres.password_file.as_deref(),
        config.app.is_local_development(),
    )?;
    let application_name = application_name(&config.observability.otel.service_name);
    let options = pool_options(
        postgres.session_budgets,
        postgres.max_connections,
        &application_name,
    );
    let pool =
        infra_postgres::prepare_pool_with_notifications(&dsn, &options, postgres.notifications);
    resources.pool = Some(PoolResource::Starting(pool.clone()));
    infra_postgres::admit_pool(&pool, &options).await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = postgres.max_connections.get(),
        "postgres_pool_opened"
    );
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
    Ok(pool)
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
    match resources.pool.as_ref() {
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
    use super::application_name;
    use crate::bootstrap::{WorkerError, worker_identity};

    #[test]
    fn refusal_texts_name_the_failed_check() {
        assert_eq!(
            WorkerError::PostgresDisabled.to_string(),
            "postgres.enabled must be true to run the jobs worker"
        );
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
