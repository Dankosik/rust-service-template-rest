//! PostgreSQL pool admission.

use super::shutdown::Background;
use infra_postgres::{Dsn, PgPool, PoolOptions, SessionBudgets};
use secrecy::ExposeSecret;
use service_config::Config;
use tokio_util::sync::CancellationToken;

use super::{BootstrapError, METRICS_MAINTENANCE_INTERVAL};

/// Open the pool when the profile is enabled. An unreachable database fails
/// startup here rather than serving a readiness that never passes.
pub(super) async fn open(
    config: &Config,
    background: &mut Background,
    cancel: &CancellationToken,
    retained: &mut Option<PgPool>,
) -> Result<(), BootstrapError> {
    if !config.postgres.enabled {
        return Ok(());
    }
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    let options = PoolOptions {
        max_connections: config.postgres.max_connections,
        // Same process identity as traces (`service.name`).
        application_name: &config.observability.otel.service_name,
        default_isolation: infra_postgres::Isolation::ServerDefault,
        session_budgets: match config.postgres.session_budgets {
            service_config::PostgresSessionBudgets::Startup => SessionBudgets::Startup,
            service_config::PostgresSessionBudgets::Server => SessionBudgets::Server,
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
            METRICS_MAINTENANCE_INTERVAL,
            cancel.child_token(),
        ),
    );
    // The rotation task ends at once without a password file, and an ended
    // background task is a service failure.
    if dsn.password_file().is_some() {
        background.spawn(
            "postgres_password_refresh",
            infra_postgres::refresh_password_periodically(pool.clone(), dsn, cancel.child_token()),
        );
    }
    Ok(())
}
