//! HTTP idempotency boundary composition and activation.

use super::background::Background;
use infra_http::idempotency::{Activation, Composer};
use infra_idempotency_store::Store;
use infra_postgres::PgPool;
use service_config::Config;

use super::BootstrapError;

/// The composer through which idempotent operations join the contract. It
/// has a store only when both the pool and a retention are set; otherwise
/// activation refuses the missing value if an idempotent operation is served.
pub(super) fn prepare(config: &Config, postgres_pool: Option<&PgPool>) -> Composer {
    match (postgres_pool, config.http_idempotency.retention) {
        (Some(pool), Some(retention)) => Composer::new(Store::new(pool.clone(), retention)),
        _ => Composer::inert(),
    }
}

/// Start the composed boundary when it serves at least one. An inactive
/// boundary makes no query, starts no task, and requires no value.
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

/// Why a served idempotent operation has no store: the configuration key
/// that is missing, or an unavailable store when every key is set.
fn unconfigured(config: &Config) -> BootstrapError {
    match config.http_idempotency.required_retention(&config.postgres) {
        Err(invalid) => invalid.into(),
        Ok(_) => infra_idempotency_store::StartupError::Unavailable.into(),
    }
}

/// Start an active boundary before readiness admission. It needs
/// `postgres.enabled`, a set `http_idempotency.retention`, and the store's
/// schema on a writable session; the cleanup task then joins the background set.
async fn start(
    store: Store,
    operations: std::num::NonZeroUsize,
    config: &Config,
    background: &mut Background,
) -> Result<(), BootstrapError> {
    let retention = config
        .http_idempotency
        .required_retention(&config.postgres)?;
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

    use super::*;

    #[tokio::test]
    async fn an_inactive_boundary_touches_no_store_and_spawns_no_task() {
        // A contract without an idempotent operation: only the family's
        // components, which every retained document carries.
        let composer = Composer::inert();
        let mut background = Background::new(CancellationToken::new());
        // An active boundary would refuse this configuration at its first
        // check: `postgres.enabled` is false and no retention is set.
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
        let mut config = Config::default();
        config.postgres.enabled = true;
        let refused = unconfigured(&config);
        std::assert_matches!(&refused, BootstrapError::Config(invalid) if invalid.key == "http_idempotency.retention",
            "{refused:?}"
        );
    }

    #[test]
    fn an_unconfigured_boundary_with_every_key_set_reports_the_store_unavailable() {
        let mut config = Config::default();
        config.postgres.enabled = true;
        config.http_idempotency.retention = Some(Duration::from_secs(3600));
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
