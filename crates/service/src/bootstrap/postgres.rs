//! PostgreSQL pool admission.

use infra_postgres::{Dsn, PgPool, PoolOptions, SessionBudgets};
use secrecy::ExposeSecret;
use service_config::Config;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::{BootstrapError, METRICS_MAINTENANCE_INTERVAL};

/// Open the pool when the profile is enabled. An unreachable database fails
/// startup here rather than serving a readiness that never passes.
pub(super) async fn open(
    config: &Config,
    background: &mut JoinSet<()>,
    cancel: &CancellationToken,
) -> Result<Option<PgPool>, BootstrapError> {
    if !config.postgres.enabled {
        return Ok(None);
    }
    let dsn = Dsn::admit_with(
        config.postgres.required_dsn()?.expose_secret(),
        config.postgres.password_file.as_deref(),
    )?;
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.max_connections,
            // Same process identity as traces (`service.name`).
            application_name: &config.observability.otel.service_name,
            default_isolation: infra_postgres::Isolation::ServerDefault,
            session_budgets: match config.postgres.session_budgets {
                service_config::PostgresSessionBudgets::Startup => SessionBudgets::Startup,
                service_config::PostgresSessionBudgets::Server => SessionBudgets::Server,
            },
        },
    )
    .await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = config.postgres.max_connections,
        "postgres_pool_opened"
    );
    background.spawn(infra_postgres::record_metrics_periodically(
        pool.clone(),
        METRICS_MAINTENANCE_INTERVAL,
        cancel.child_token(),
    ));
    // Ends at once unless `postgres.password_file` is set.
    background.spawn(infra_postgres::refresh_password_periodically(
        pool.clone(),
        dsn,
        cancel.child_token(),
    ));
    Ok(Some(pool))
}
