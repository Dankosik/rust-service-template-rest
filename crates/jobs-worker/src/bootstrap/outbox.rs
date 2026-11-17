use std::num::NonZeroU32;

use infra_jobs::Engine;
use infra_messaging::MessagingError;

use super::WorkerError;
use super::messaging::RetainedMessages;
use crate::resources::Resources;

pub(super) async fn publisher(
    resources: &Resources,
    ordinary: Option<&Engine>,
) -> Result<Engine, WorkerError> {
    let messaging = resources
        .admitted_messaging()
        .ok_or(MessagingError::Connection)?;
    let kinds = infra_messaging::outbox::kinds(messaging.producer())?;
    if ordinary.is_some_and(|ordinary| {
        kinds
            .names()
            .any(|reserved| ordinary.kinds().any(|name| name == reserved))
    }) {
        return Err(WorkerError::PublisherKindConflict);
    }
    let publisher = match ordinary {
        Some(ordinary) => ordinary.beside(kinds, NonZeroU32::MIN),
        None => Engine::new(
            super::postgres::admitted_pool(resources)?,
            kinds,
            NonZeroU32::MIN,
        ),
    };
    publisher.check_startup().await?;
    Ok(publisher)
}

impl RetainedMessages {
    pub(super) fn activate_postgres_messages(
        &mut self,
        resources: &Resources,
    ) -> Result<(), WorkerError> {
        if let Some(factory) = self.postgres_messages.take() {
            let pool = super::postgres::admitted_pool(resources)?;
            factory(pool, &mut self.registry).map_err(WorkerError::Registration)?;
            self.registry.require_handlers()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use crate::background::Background;
    use crate::bootstrap::WorkerError;
    use crate::bootstrap::messaging::RetainedMessages;
    use crate::resources::{PoolResource, Resources};

    #[tokio::test]
    async fn deferred_message_intent_refuses_missing_consumer_config_before_factory_runs() {
        let config =
            service_config::fixtures::worker(&[], crate::BUILD_INFO, crate::shutdown::TAIL)
                .unwrap();
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
        let config =
            service_config::fixtures::worker(&[], crate::BUILD_INFO, crate::shutdown::TAIL)
                .unwrap();
        let background = Background::new();
        let mut registration = crate::Registration::new(&config, &background);
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
                session_settings_source: infra_postgres::SessionSettingsSource::Startup,
                wait_order: infra_postgres::WaitOrder::Arrival,
            },
        );
        let resources = Resources {
            pool: Some(PoolResource::Admitted(pool.clone())),
            ..Resources::default()
        };
        let mut messages = RetainedMessages {
            registry: registration.messages,
            postgres_messages: registration.postgres_messages,
        };
        std::assert_matches!(
            messages.activate_postgres_messages(&resources),
            Err(WorkerError::MessagingRegistry(
                infra_messaging::RegistryError::Empty
            ))
        );
        let received = received.await.unwrap();
        pool.close().await;
        assert!(
            received.is_closed(),
            "factory uses the retained native pool"
        );
        messages.activate_postgres_messages(&resources).unwrap();
    }
}
