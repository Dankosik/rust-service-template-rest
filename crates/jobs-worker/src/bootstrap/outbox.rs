use std::num::NonZeroU32;

use infra_jobs::{Engine, KindError};
use infra_messaging::{MessagingError, OutboxProducer};
use infra_postgres::Pool;

use super::WorkerError;
use super::messaging::RetainedMessages;

pub(super) fn with_publisher(
    pool: Option<&Pool>,
    outbox: Option<OutboxProducer>,
    ordinary: Option<Engine>,
) -> Result<Engine, WorkerError> {
    let outbox = outbox.ok_or(MessagingError::Connection)?;
    let kinds = infra_messaging::outbox::kinds(outbox)?;
    match ordinary {
        Some(mut engine) => {
            engine
                .add_group(kinds, NonZeroU32::MIN)
                .map_err(|duplicate| KindError::Duplicate(duplicate.kind))?;
            Ok(engine)
        }
        None => Ok(Engine::new(
            pool.cloned().ok_or(WorkerError::PostgresDisabled)?,
            kinds,
            NonZeroU32::MIN,
        )),
    }
}

impl RetainedMessages {
    pub(super) fn register_postgres_messages(
        &mut self,
        pool: Option<&Pool>,
    ) -> Result<(), WorkerError> {
        if let Some(factory) = self.postgres_messages.take() {
            let pool = pool.cloned().ok_or(WorkerError::PostgresDisabled)?;
            factory(pool, &mut self.registry)?;
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

    #[tokio::test]
    async fn deferred_message_intent_refuses_missing_consumer_config_before_factory_runs() {
        let config =
            service_config::fixtures::worker(&[], crate::BUILD_INFO, crate::shutdown::TAIL)
                .unwrap();
        let mut background = Background::new();
        let result = crate::bootstrap::register_capabilities(
            &config,
            Box::new(|registration| {
                registration.with_postgres_messages(|_, _| panic!("must not run before admission"))
            }),
            &mut background,
        );
        assert!(matches!(result, Err(WorkerError::Config(_))));
    }

    #[tokio::test]
    async fn postgres_message_factory_is_single_and_an_empty_consumer_refuses() {
        let config =
            service_config::fixtures::worker(&[], crate::BUILD_INFO, crate::shutdown::TAIL)
                .unwrap();
        let mut background = Background::new();
        let mut registration = crate::Registration::new(&config, &mut background);
        let (handed, received) = tokio::sync::oneshot::channel();
        registration
            .with_postgres_messages(move |pool, _| {
                handed.send(pool).unwrap();
                Ok(())
            })
            .unwrap();
        assert!(registration.with_postgres_messages(|_, _| Ok(())).is_err());
        let pool = infra_postgres::Pool::connect_lazy_for_test(
            "postgres://test:test@127.0.0.1:1/test?sslmode=verify-full",
        )
        .unwrap();
        let mut messages = RetainedMessages {
            registry: registration.messages,
            postgres_messages: registration.postgres_messages,
        };
        std::assert_matches!(
            messages.register_postgres_messages(Some(&pool)),
            Err(WorkerError::MessagingRegistry(
                infra_messaging::RegistryError::Empty
            ))
        );
        let received = received.await.unwrap();
        let _ = pool.close(std::time::Duration::from_secs(1)).await;
        assert!(
            received.is_closed(),
            "factory uses the retained native pool"
        );
        messages.register_postgres_messages(Some(&pool)).unwrap();
    }
}
