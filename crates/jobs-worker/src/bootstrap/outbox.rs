//! The outbox publication engine and database-backed message handlers.

use std::num::NonZeroU32;

use infra_jobs::Engine;
use infra_messaging::MessagingError;
use service_config::Config;

use super::{RetainedCapabilities, WorkerError};
use crate::shutdown::Resources;

pub(super) fn admit_pool_inputs(
    config: &Config,
    capabilities: &RetainedCapabilities,
) -> Result<(), WorkerError> {
    config
        .jobs
        .validate_pool_capacity_with_outbox(&config.postgres, capabilities.jobs.is_some())?;
    config.messaging.validate_producer(&config.app.env)?;
    Ok(())
}

pub(super) async fn publisher(
    resources: &Resources,
    ordinary: Option<&Engine>,
) -> Result<Engine, WorkerError> {
    let messaging = resources
        .messaging
        .as_ref()
        .ok_or(MessagingError::Connection)?;
    let registry = infra_messaging::outbox::registry(messaging.producer())?;
    if ordinary.is_some_and(|ordinary| {
        registry
            .names()
            .any(|reserved| ordinary.kinds().any(|name| name == reserved))
    }) {
        return Err(WorkerError::PublisherKindConflict);
    }
    // One listener and one retention loop serve both engines of this process.
    let publisher = match ordinary {
        Some(ordinary) => ordinary.beside(registry, NonZeroU32::MIN),
        None => Engine::new(
            resources
                .pool
                .as_ref()
                .ok_or(WorkerError::PostgresDisabled)?
                .clone(),
            registry,
            NonZeroU32::MIN,
        ),
    };
    publisher.check_startup().await?;
    Ok(publisher)
}

impl RetainedCapabilities {
    pub(super) fn activate_postgres_messages(
        &mut self,
        resources: &Resources,
    ) -> Result<(), WorkerError> {
        if let Some(factory) = self.postgres_messages.take() {
            let pool = resources
                .pool
                .as_ref()
                .ok_or(WorkerError::PostgresDisabled)?;
            let registry = self.messages.as_mut().ok_or(WorkerError::NoRegistrations)?;
            factory(pool.clone(), registry).map_err(WorkerError::Registration)?;
            registry.validate_consumer()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use service_config::Config;

    use crate::bootstrap::{RetainedCapabilities, WorkerError};
    use crate::shutdown::{Background, Resources};

    #[tokio::test]
    async fn deferred_message_intent_refuses_missing_consumer_config_before_factory_runs() {
        let config = Config::default();
        let background = Background::new();
        let result = crate::bootstrap::register_capabilities(
            &config,
            Box::new(|registration| {
                registration.with_postgres_messages(|_, _| panic!("must not run before admission"))
            }),
            &background,
        );
        assert!(matches!(result, Err(WorkerError::Config(_))));
    }

    #[tokio::test]
    async fn postgres_message_factory_is_single_and_an_empty_consumer_refuses() {
        use infra_messaging::Registry;

        let config = Config::default();
        let background = Background::new();
        let mut registration = crate::Registration {
            jobs: infra_jobs::KindsBuilder::new(),
            messages: Registry::try_new([]).unwrap(),
            postgres_messages: None,
            config: &config,
            background: &background,
        };
        let (handed, received) = tokio::sync::oneshot::channel();
        registration
            .with_postgres_messages(move |pool, _| {
                handed.send(pool).unwrap();
                Ok(())
            })
            .unwrap();
        assert!(registration.with_postgres_messages(|_, _| Ok(())).is_err());
        let dsn =
            infra_postgres::Dsn::admit("postgres://test:test@127.0.0.1:1/test?sslmode=disable")
                .unwrap();
        let pool = infra_postgres::prepare_pool(
            &dsn,
            &infra_postgres::PoolOptions {
                max_connections: std::num::NonZeroU32::MIN,
                application_name: "factory-composition-test",
                default_isolation: infra_postgres::Isolation::ReadCommitted,
                session_budgets: infra_postgres::SessionBudgets::Startup,
            },
        );
        let resources = Resources {
            pool: Some(pool.clone()),
            ..Resources::default()
        };
        let mut capabilities = RetainedCapabilities {
            jobs: None,
            messages: Some(registration.messages),
            postgres_messages: registration.postgres_messages,
        };
        assert!(matches!(
            capabilities.activate_postgres_messages(&resources),
            Err(WorkerError::MessagingRegistry(
                infra_messaging::RegistryError::Empty
            ))
        ));
        let received = received.await.unwrap();
        pool.close().await;
        assert!(
            received.is_closed(),
            "factory uses the retained native pool"
        );
        // The consumed FnOnce is not run again.
        capabilities.activate_postgres_messages(&resources).unwrap();
    }
}
