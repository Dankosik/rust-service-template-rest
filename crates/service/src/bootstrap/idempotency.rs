use super::background::Background;
use infra_http::idempotency::{Activation, Composer};
use infra_idempotency_store::Store;
use infra_postgres::PgPool;
use service_config::{Config, HttpIdempotencyConfig, ValidationError};

use super::BootstrapError;

pub(super) fn prepare(
    config: &Config,
    postgres_pool: Option<&PgPool>,
) -> Result<Composer, ValidationError> {
    let (Some(pool), Some(retention)) = (postgres_pool, config.http_idempotency.retention) else {
        return Ok(Composer::inert());
    };
    Store::new(pool.clone(), retention)
        .map(Composer::new)
        .map_err(|error| {
            ValidationError::new(HttpIdempotencyConfig::RETENTION_KEY, error.to_string())
        })
}

pub(super) async fn activate(
    composer: Composer,
    config: &Config,
    background: &mut Background,
) -> Result<(), BootstrapError> {
    match composer.finish() {
        Activation::Inactive => Ok(()),
        Activation::Unconfigured { .. } => Err(unconfigured(config)),
        Activation::Active { store, operations } => {
            start(store, operations, config, background).await
        }
    }
}

fn unconfigured(config: &Config) -> BootstrapError {
    match config
        .http_idempotency
        .required_retention(config.postgres.is_some())
    {
        Err(invalid) => invalid.into(),
        Ok(_) => infra_idempotency_store::StartupError::Unavailable.into(),
    }
}

async fn start(
    store: Store,
    operations: std::num::NonZeroUsize,
    config: &Config,
    background: &mut Background,
) -> Result<(), BootstrapError> {
    let retention = config
        .http_idempotency
        .required_retention(config.postgres.is_some())?;
    store.check_startup().await?;
    background.spawn(
        "idempotency_cleanup",
        store.run_cleanup(background.child_token()),
    );
    tracing::info!(
        http_idempotency.operations = operations.get(),
        http_idempotency.retention = ?retention,
        "http_idempotency_active"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;

    use service_config::{PostgresConfig, PostgresSessionBudgets};

    use super::*;

    fn enabled_postgres() -> PostgresConfig {
        PostgresConfig {
            dsn: secrecy::SecretString::from("postgres://localhost/app".to_owned()),
            password_file: None,
            session_budgets: PostgresSessionBudgets::Startup,
            max_connections: std::num::NonZeroU32::MIN,
            migration_deadline: Duration::from_mins(5),
        }
    }

    #[tokio::test]
    async fn an_inactive_boundary_touches_no_store_and_spawns_no_task() {
        let composer = Composer::inert();
        let mut background = Background::new(CancellationToken::new());
        activate(composer, &Config::default(), &mut background)
            .await
            .expect("an inactive boundary requires no value");
        assert!(background.is_empty());
    }

    #[test]
    fn an_unconfigured_boundary_refuses_disabled_postgres_naming_the_key() {
        let refused = unconfigured(&Config::default());
        std::assert_matches!(&refused, BootstrapError::Config(invalid) if invalid.key == "postgres.enabled",
            "{refused:?}"
        );
    }

    #[test]
    fn an_unconfigured_boundary_refuses_an_unset_retention_naming_the_key() {
        let config = Config {
            postgres: Some(enabled_postgres()),
            ..Config::default()
        };
        let refused = unconfigured(&config);
        std::assert_matches!(&refused, BootstrapError::Config(invalid) if invalid.key == "http_idempotency.retention",
            "{refused:?}"
        );
    }

    #[test]
    fn an_unconfigured_boundary_with_every_key_set_reports_the_store_unavailable() {
        let mut config = Config {
            postgres: Some(enabled_postgres()),
            ..Config::default()
        };
        config.http_idempotency.retention = Some(Duration::from_hours(1));
        let refused = unconfigured(&config);
        std::assert_matches!(
            &refused,
            BootstrapError::HttpIdempotencyStartup(
                infra_idempotency_store::StartupError::Unavailable
            ),
            "{refused:?}"
        );
    }
}
