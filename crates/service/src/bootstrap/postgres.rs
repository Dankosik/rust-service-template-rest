//! PostgreSQL pool admission.

use super::background::Background;
use infra_postgres::{Dsn, PgPool, PoolOptions, SessionSettingsSource};
use secrecy::ExposeSecret;
use service_config::Config;

use super::{BootstrapError, METRICS_MAINTENANCE_INTERVAL};

/// Open the pool when the profile is enabled. An unreachable database fails
/// startup here rather than serving a readiness that never passes.
pub(super) async fn open(
    config: &Config,
    background: &mut Background,
    retained: &mut Option<PgPool>,
) -> Result<(), BootstrapError> {
    if !config.postgres.enabled {
        return Ok(());
    }
    let raw = config.postgres.required_dsn()?.clone();
    let password_file = config.postgres.password_file.clone();
    let dsn = super::blocking_startup("postgres", move || {
        Dsn::admit_with(raw.expose_secret(), password_file.as_deref())
    })
    .await?;
    let options = PoolOptions {
        max_connections: config.postgres.max_connections,
        // Same process identity as traces (`service.name`).
        application_name: &config.observability.otel.service_name,
        default_isolation: infra_postgres::Isolation::ServerDefault,
        session_settings_source: match config.postgres.session_budgets {
            service_config::PostgresSessionBudgets::Startup => SessionSettingsSource::Startup,
            service_config::PostgresSessionBudgets::Server => SessionSettingsSource::Server,
        },
    };
    let pool = retained.insert(infra_postgres::prepare_pool(&dsn, &options));
    infra_postgres::admit_pool(pool, &options).await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections,
        "postgres_pool_opened"
    );
    background.spawn(
        "postgres_metrics",
        infra_postgres::record_metrics_periodically(
            pool.clone(),
            "postgres",
            METRICS_MAINTENANCE_INTERVAL,
            background.child_token(),
        ),
    );
    if let Some(password_file) = dsn.password_file() {
        background.spawn(
            "postgres_password_refresh",
            infra_postgres::refresh_password_periodically(
                pool.clone(),
                password_file.to_owned(),
                background.child_token(),
            ),
        );
    }
    Ok(())
}
