use super::background::Background;
use infra_idempotency_store::Store;
use infra_postgres::PgPool;
use service_config::{Config, HttpIdempotencyConfig, ValidationError};

use super::BootstrapError;

#[derive(Debug)]
pub(super) struct MountedStore {
    pub(super) store: Store,
    operations: std::num::NonZeroUsize,
    retention: std::time::Duration,
}

pub(super) fn prepare(
    mounted: usize,
    config: &Config,
    postgres_pool: Option<&PgPool>,
) -> Result<Option<MountedStore>, BootstrapError> {
    let Some(operations) = std::num::NonZeroUsize::new(mounted) else {
        return Ok(None);
    };
    let retention = config
        .http_idempotency
        .required_retention(config.postgres.is_some())?;
    let pool = postgres_pool.ok_or(infra_idempotency_store::StartupError::Unavailable)?;
    let store = Store::new(pool.clone(), retention).map_err(|error| {
        ValidationError::new(HttpIdempotencyConfig::RETENTION_KEY, error.to_string())
    })?;
    Ok(Some(MountedStore {
        store,
        operations,
        retention,
    }))
}

impl MountedStore {
    pub(super) async fn activate(self, background: &mut Background) -> Result<(), BootstrapError> {
        self.store.check_startup().await?;
        background.spawn_with_token("idempotency_cleanup", |stop| self.store.run_cleanup(stop));
        tracing::info!(
            http_idempotency.operations = self.operations.get(),
            http_idempotency.retention = ?self.retention,
            "http_idempotency_active"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bootstrap::test_config;

    #[test]
    fn an_empty_mount_requires_no_store() {
        assert!(prepare(0, &test_config(&[]), None).unwrap().is_none());
    }

    #[test]
    fn an_unconfigured_boundary_refuses_disabled_postgres_naming_the_key() {
        let refused = prepare(1, &test_config(&[]), None).unwrap_err();
        std::assert_matches!(&refused, BootstrapError::Config(invalid) if invalid.key == "postgres.enabled",
            "{refused:?}"
        );
    }

    #[test]
    fn an_unconfigured_boundary_refuses_an_unset_retention_naming_the_key() {
        let config = test_config(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
        ]);
        let refused = prepare(1, &config, None).unwrap_err();
        std::assert_matches!(&refused, BootstrapError::Config(invalid) if invalid.key == "http_idempotency.retention",
            "{refused:?}"
        );
    }

    #[test]
    fn an_unconfigured_boundary_with_every_key_set_reports_the_store_unavailable() {
        let config = test_config(&[
            ("APP__POSTGRES__ENABLED", "true"),
            ("APP__POSTGRES__DSN", "postgres://localhost/app"),
            ("APP__HTTP_IDEMPOTENCY__RETENTION", "1h"),
        ]);
        let refused = prepare(1, &config, None).unwrap_err();
        std::assert_matches!(
            &refused,
            BootstrapError::HttpIdempotencyStartup(
                infra_idempotency_store::StartupError::Unavailable
            ),
            "{refused:?}"
        );
    }
}
