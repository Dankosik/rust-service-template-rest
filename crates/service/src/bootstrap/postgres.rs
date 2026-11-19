use super::background::Background;
use infra_postgres::{Dsn, Pool, PoolOptions, SessionSettingsSource, SslPolicy};
use secrecy::ExposeSecret;
use service_config::{Config, PostgresConfig};

use super::{BootstrapError, METRICS_MAINTENANCE_INTERVAL};

pub(super) async fn open<'a>(
    postgres: &PostgresConfig,
    config: &Config,
    background: &mut Background,
    retained: &'a mut Option<Pool>,
) -> Result<&'a Pool, BootstrapError> {
    let raw = postgres.dsn.clone();
    let password_file = postgres.password_file.clone();
    let ssl_policy = if config.app().is_local_development() {
        SslPolicy::AllowLocalDevelopment
    } else {
        SslPolicy::VerifyFull
    };
    let dsn = super::blocking_startup("postgres", move || {
        Dsn::admit_with(raw.expose_secret(), password_file.as_deref(), ssl_policy)
    })
    .await?;
    let options = PoolOptions {
        max_connections: postgres.max_connections,
        application_name: &config.observability().otel.service_name,
        default_isolation: infra_postgres::Isolation::ServerDefault,
        session_settings_source: match postgres.session_budgets {
            service_config::PostgresSessionBudgets::Startup => SessionSettingsSource::Startup,
            service_config::PostgresSessionBudgets::Server => SessionSettingsSource::Server,
        },
        wait_order: infra_postgres::WaitOrder::NewestOnceStanding,
        notifications: postgres.notifications,
    };
    let pool = retained.insert(infra_postgres::prepare_pool(&dsn, &options));
    infra_postgres::admit_pool(pool).await?;
    tracing::info!(
        postgres.host = dsn.host(),
        postgres.port = dsn.port(),
        postgres.database = dsn.database(),
        postgres.sslmode = dsn.ssl_mode_name(),
        postgres.max_connections = postgres.max_connections,
        "postgres_pool_opened"
    );
    background.spawn_with_token("postgres_metrics", |stop| {
        infra_postgres::record_metrics_periodically(
            pool.clone(),
            "postgres",
            METRICS_MAINTENANCE_INTERVAL,
            stop,
        )
    });
    if let Some(password_file) = dsn.password_file() {
        background.spawn_with_token("postgres_password_refresh", |stop| {
            infra_postgres::refresh_password_periodically(
                pool.clone(),
                password_file.to_owned(),
                stop,
            )
        });
    }
    Ok(&*pool)
}
