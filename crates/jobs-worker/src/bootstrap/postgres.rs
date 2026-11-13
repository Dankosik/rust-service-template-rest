//! The shared PostgreSQL pool: admission, opening, and its readiness probe.

use std::num::NonZeroU32;

use health::Probe;
use infra_postgres::{Dsn, PgPool, PoolOptions, PostgresProbe, SessionSettingsSource};
use secrecy::ExposeSecret;
use service_config::{Config, PostgresConfig, PostgresSessionBudgets};

use super::{IDENTITY_SUFFIX, METRICS_MAINTENANCE_INTERVAL, RetainedCapabilities, WorkerError};
use crate::background::Background;
use crate::resources::Resources;

const POSTGRES_IDENTIFIER_LIMIT: usize = 63;

/// Check the pool capacity, open the pool, and verify the migration history
/// when a retained capability needs the database.
pub(super) async fn admit_pool(
    config: &Config,
    capabilities: &RetainedCapabilities,
    background: &Background,
    resources: &mut Resources,
) -> Result<(), WorkerError> {
    #[allow(
        unused_variables,
        reason = "retained outbox also requires the shared pool"
    )]
    let needs_pool = capabilities.jobs.is_some();
    // template:begin outbox:worker-bootstrap-outbox-pool
    let needs_pool = true;
    // template:end outbox:worker-bootstrap-outbox-pool
    if !needs_pool {
        return Ok(());
    }
    if !config.postgres.enabled {
        return Err(WorkerError::PostgresDisabled);
    }
    #[allow(
        unused_variables,
        reason = "retained outbox supplies the combined admission"
    )]
    let admit_inputs = || {
        config
            .jobs
            .validate_pool_capacity(&config.postgres)
            .map_err(WorkerError::from)
    };
    // template:begin outbox:worker-bootstrap-outbox-pool-inputs
    let admit_inputs = || super::outbox::admit_pool_inputs(config, capabilities);
    // template:end outbox:worker-bootstrap-outbox-pool-inputs
    admit_inputs()?;
    let pool = open_pool(config, background, resources).await?;
    migrate::verify_history(&pool).await?;
    Ok(())
}

async fn open_pool(
    config: &Config,
    background: &Background,
    resources: &mut Resources,
) -> Result<PgPool, WorkerError> {
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    let application_name = application_name(&config.observability.otel.service_name);
    let options = pool_options(
        &config.postgres,
        config.postgres.max_connections,
        &application_name,
    );
    let pool = infra_postgres::prepare_pool(&dsn, &options);
    resources.pool = Some(pool.clone());
    infra_postgres::admit_pool(&pool, &options).await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections.get(),
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

/// The worker and the operator differ only in pool size and session name.
pub(crate) fn pool_options<'a>(
    config: &PostgresConfig,
    max_connections: NonZeroU32,
    application_name: &'a str,
) -> PoolOptions<'a> {
    PoolOptions {
        max_connections,
        application_name,
        default_isolation: infra_postgres::Isolation::ReadCommitted,
        session_settings_source: match config.session_budgets {
            PostgresSessionBudgets::Startup => SessionSettingsSource::Startup,
            PostgresSessionBudgets::Server => SessionSettingsSource::Server,
        },
    }
}

fn application_name(service_name: &str) -> String {
    let end = service_name.floor_char_boundary(POSTGRES_IDENTIFIER_LIMIT - IDENTITY_SUFFIX.len());
    format!("{}{IDENTITY_SUFFIX}", &service_name[..end])
}

/// The pool [`admit_pool`] opened; every capability that needs it runs after
/// admission.
pub(super) fn admitted_pool(resources: &Resources) -> Result<PgPool, WorkerError> {
    resources.pool.clone().ok_or(WorkerError::PostgresDisabled)
}

pub(super) fn probe(resources: &Resources) -> Option<Box<dyn Probe>> {
    let pool = resources.pool.as_ref()?;
    Some(Box::new(PostgresProbe::new(pool.clone())))
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
