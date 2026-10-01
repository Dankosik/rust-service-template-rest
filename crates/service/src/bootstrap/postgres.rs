//! PostgreSQL pool admission.

use infra_postgres::{Dsn, PgPool, PoolOptions};
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
    let dsn = Dsn::admit(config.postgres.required_dsn()?.expose_secret())?;
    let pool = infra_postgres::connect(
        &dsn,
        &PoolOptions {
            max_connections: config.postgres.max_connections,
            // Same process identity as traces (`service.name`).
            application_name: &config.observability.otel.service_name,
            default_isolation: infra_postgres::Isolation::ServerDefault,
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
    Ok(Some(pool))
}
