use std::any::TypeId;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use domain_events::{Event, EventPayload};
use foldhash::HashMap;
use operation_context::OperationContext;
use serde::de::DeserializeOwned;

use crate::contract::{PayloadSchema, payload_schema};
use crate::error::HandlerError;
use crate::wire::{InboundEnvelope, valid_subject};
use bytes::Bytes;
use time::OffsetDateTime;

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send>>;
type ErasedHandler = Arc<
    dyn Fn(String, OffsetDateTime, Bytes, OperationContext) -> Option<HandlerFuture> + Send + Sync,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchError {
    Unhandled,
    Undecodable,
    Handler(HandlerError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RouteKey<'a> {
    pub(crate) event_type: &'a str,
    pub(crate) schema_version: u16,
}

pub(crate) const fn route_key<T: EventPayload>() -> RouteKey<'static> {
    const {
        assert!(
            T::SCHEMA_VERSION > 0,
            "event schema version must be positive"
        );
        assert!(
            crate::wire::valid_identity(T::EVENT_TYPE),
            "event type must be 1 to IDENTITY_MAX_BYTES bytes without control characters"
        );
    }
    RouteKey {
        event_type: T::EVENT_TYPE,
        schema_version: T::SCHEMA_VERSION,
    }
}

pub struct Route<T> {
    subject: async_nats::Subject,
    schema: Option<PayloadSchema>,
    payload: PhantomData<fn(T) -> T>,
}

impl<T> Clone for Route<T> {
    fn clone(&self) -> Self {
        Self {
            subject: self.subject.clone(),
            schema: self.schema,
            payload: PhantomData,
        }
    }
}

impl<T: EventPayload> std::fmt::Debug for Route<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Route")
            .field("key", &route_key::<T>())
            .field("subject", &self.subject)
            .field("documented", &self.schema.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: EventPayload> Route<T> {
    #[must_use]
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: async_nats::Subject::from(subject.into()),
            schema: None,
            payload: PhantomData,
        }
    }

    #[must_use]
    pub fn documented(subject: impl Into<String>) -> Self
    where
        T: utoipa::ToSchema,
    {
        Self {
            schema: Some(payload_schema::<T>),
            ..Self::new(subject)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("route event type {event_type:?} v{schema_version} is invalid")]
    InvalidEventType {
        event_type: String,
        schema_version: u16,
    },
    #[error("route subject is invalid for {event_type} v{schema_version}")]
    InvalidSubject {
        event_type: String,
        schema_version: u16,
    },
    #[error("duplicate route for {event_type} v{schema_version}")]
    DuplicateRoute {
        event_type: String,
        schema_version: u16,
    },
    #[error("typed route is not registered for {event_type} v{schema_version}")]
    MissingRoute {
        event_type: String,
        schema_version: u16,
    },
    #[error("duplicate handler for {event_type} v{schema_version}")]
    DuplicateHandler {
        event_type: String,
        schema_version: u16,
    },
    #[error("no typed event handlers are registered")]
    Empty,
}

#[derive(Clone, Default)]
pub struct Registry {
    routes: HashMap<RouteKey<'static>, RegisteredRoute>,
}

#[derive(Clone)]
pub(crate) struct RegisteredRoute {
    pub(crate) key: RouteKey<'static>,
    pub(crate) subject: async_nats::Subject,
    pub(crate) schema: Option<PayloadSchema>,
    payload: TypeId,
    handler: Option<ErasedHandler>,
}

impl RegisteredRoute {
    fn matches<T: 'static>(&self, route: &Route<T>) -> bool {
        self.payload == TypeId::of::<T>()
            && self.subject == route.subject
            && self.schema.is_some() == route.schema.is_some()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subjects = std::fmt::from_fn(|f| {
            f.debug_map()
                .entries(self.routes.iter().map(|(key, entry)| (key, &entry.subject)))
                .finish()
        });
        formatter
            .debug_struct("Registry")
            .field("routes", &subjects)
            .field(
                "documented_count",
                &self
                    .routes
                    .values()
                    .filter(|entry| entry.schema.is_some())
                    .count(),
            )
            .field(
                "handler_count",
                &self
                    .routes
                    .values()
                    .filter(|entry| entry.handler.is_some())
                    .count(),
            )
            .finish()
    }
}

impl Registry {
    pub fn declare<T: EventPayload + 'static>(
        &mut self,
        route: &Route<T>,
    ) -> Result<(), RegistryError> {
        let key @ RouteKey {
            event_type,
            schema_version,
        } = route_key::<T>();
        if !valid_subject(&route.subject) {
            return Err(RegistryError::InvalidSubject {
                event_type: event_type.to_owned(),
                schema_version,
            });
        }
        if crate::wire::validate_outbound_identity(event_type).is_err() {
            return Err(RegistryError::InvalidEventType {
                event_type: event_type.to_owned(),
                schema_version,
            });
        }
        let std::collections::hash_map::Entry::Vacant(entry) = self.routes.entry(key) else {
            return Err(RegistryError::DuplicateRoute {
                event_type: event_type.to_owned(),
                schema_version,
            });
        };
        entry.insert(RegisteredRoute {
            key,
            subject: route.subject.clone(),
            schema: route.schema,
            payload: TypeId::of::<T>(),
            handler: None,
        });
        Ok(())
    }

    pub fn register<T, F, Fut>(&mut self, route: &Route<T>, handler: F) -> Result<(), RegistryError>
    where
        T: EventPayload + DeserializeOwned + 'static,
        F: Fn(Event<T>, OperationContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        let Some(entry) = self
            .routes
            .get_mut(&route_key::<T>())
            .filter(|entry| entry.matches(route))
        else {
            return Err(RegistryError::MissingRoute {
                event_type: T::EVENT_TYPE.to_owned(),
                schema_version: T::SCHEMA_VERSION,
            });
        };
        if entry.handler.is_some() {
            return Err(RegistryError::DuplicateHandler {
                event_type: T::EVENT_TYPE.to_owned(),
                schema_version: T::SCHEMA_VERSION,
            });
        }
        entry.handler = Some(Arc::new(
            move |id, occurred_at: OffsetDateTime, payload: Bytes, context| {
                let payload = serde_json::from_slice::<T>(&payload).ok()?;
                let event = Event {
                    id,
                    occurred_at: occurred_at.to_utc(),
                    payload,
                };
                Some(Box::pin(handler(event, context)) as HandlerFuture)
            },
        ));
        Ok(())
    }

    #[must_use]
    pub fn has_handlers(&self) -> bool {
        self.routes.values().any(|entry| entry.handler.is_some())
    }

    pub fn require_handlers(&self) -> Result<(), RegistryError> {
        self.has_handlers().ok_or(RegistryError::Empty)
    }

    pub(crate) fn subject_for<T: EventPayload + 'static>(
        &self,
        route: &Route<T>,
    ) -> Option<&async_nats::Subject> {
        self.routes
            .get(&route_key::<T>())
            .filter(|entry| entry.matches(route))
            .map(|entry| &entry.subject)
    }

    pub(crate) fn routes(&self) -> impl Iterator<Item = &RegisteredRoute> {
        self.routes.values()
    }

    pub(crate) fn handled_routes(&self) -> impl Iterator<Item = &RegisteredRoute> {
        self.routes.values().filter(|entry| entry.handler.is_some())
    }

    pub(crate) async fn dispatch(
        &self,
        subject: &str,
        envelope: InboundEnvelope,
        context: OperationContext,
    ) -> Result<(), DispatchError> {
        let InboundEnvelope {
            message_id,
            event_type,
            schema_version,
            occurred_at,
            payload,
        } = envelope;
        let routes: &HashMap<RouteKey<'_>, RegisteredRoute> = &self.routes;
        let handler = routes
            .get(&RouteKey {
                event_type: &event_type,
                schema_version,
            })
            .filter(|entry| entry.subject.as_str() == subject)
            .and_then(|entry| entry.handler.as_ref())
            .ok_or(DispatchError::Unhandled)?;
        let run =
            handler(message_id, occurred_at, payload, context).ok_or(DispatchError::Undecodable)?;
        run.await.map_err(DispatchError::Handler)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EnvelopeError;
    use crate::prepared::PreparedEvent;

    #[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
    struct Created {
        quantity: u32,
    }

    impl EventPayload for Created {
        const EVENT_TYPE: &'static str = "order.created";
        const SCHEMA_VERSION: u16 = 2;
    }

    struct Shipped;

    impl EventPayload for Shipped {
        const EVENT_TYPE: &'static str = "order.shipped";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn a_colliding_declaration_cannot_replace_publish_or_handle_the_admitted_route() {
        #[derive(serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
        struct Alias(String);

        impl EventPayload for Alias {
            const EVENT_TYPE: &'static str = Created::EVENT_TYPE;
            const SCHEMA_VERSION: u16 = Created::SCHEMA_VERSION;
        }

        let route = Route::<Alias>::documented("orders.created");
        let publication_route = route.clone();
        let collision = Route::<Created>::documented("orders.created");
        let mut registry = Registry::default();
        registry.declare(&route).unwrap();
        std::assert_matches!(
            registry.declare(&collision),
            Err(RegistryError::DuplicateRoute { .. })
        );
        let aliased_event = Event {
            id: "event-1".to_owned(),
            occurred_at: time::UtcDateTime::UNIX_EPOCH,
            payload: Created { quantity: 2 },
        };
        std::assert_matches!(
            registry.register(&collision, |_, _| async { Ok(()) }),
            Err(RegistryError::MissingRoute { .. })
        );
        std::assert_matches!(
            registry.prepare(&collision, &aliased_event, 1024),
            Err(EnvelopeError::MissingRoute { .. })
        );

        let event = Event {
            id: aliased_event.id,
            occurred_at: aliased_event.occurred_at,
            payload: Alias("accepted".to_owned()),
        };
        for undeclared in [
            Route::<Alias>::documented("another.subject"),
            Route::<Alias>::new("orders.created"),
        ] {
            std::assert_matches!(
                registry.prepare(&undeclared, &event, 1024),
                Err(EnvelopeError::MissingRoute { .. })
            );
            std::assert_matches!(
                registry.register(&undeclared, |_, _| async { Ok(()) }),
                Err(RegistryError::MissingRoute { .. })
            );
        }
        let prepared = registry.prepare(&publication_route, &event, 1024).unwrap();
        assert_eq!(prepared.subject(), "orders.created");
        assert_eq!(prepared.payload().as_ref(), br#""accepted""#);
        registry.register(&route, |_, _| async { Ok(()) }).unwrap();
        std::assert_matches!(
            registry.register(&route, |_, _| async { Ok(()) }),
            Err(RegistryError::DuplicateHandler { .. })
        );
        let document = registry.asyncapi("orders", "1").unwrap();
        assert_eq!(
            document["channels"]["orders.created"]["messages"]["order.created.v2"]["payload"]["schema"]
                ["$ref"],
            "#/$defs/Alias"
        );
    }

    #[test]
    fn routed_preparation_checks_route_before_non_finite_payload() {
        struct Measured(std::cell::Cell<usize>);

        impl EventPayload for Measured {
            const EVENT_TYPE: &'static str = "order.measured";
            const SCHEMA_VERSION: u16 = 1;
        }

        impl serde::Serialize for Measured {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.set(self.0.get() + 1);
                serializer.serialize_f64(f64::NAN)
            }
        }

        let event = Event {
            id: "event-1".to_owned(),
            occurred_at: time::OffsetDateTime::UNIX_EPOCH.to_utc(),
            payload: Measured(std::cell::Cell::new(0)),
        };
        let route = Route::<Measured>::new("orders.measured");
        let mut registry = Registry::default();
        std::assert_matches!(
            registry.prepare(&route, &event, 1024),
            Err(EnvelopeError::MissingRoute { .. })
        );
        assert_eq!(event.payload.0.get(), 0);
        registry.declare(&route).expect("publication route");
        std::assert_matches!(
            registry.prepare(&route, &event, 1024),
            Err(EnvelopeError::Serialization)
        );
        assert_eq!(event.payload.0.get(), 1);
    }

    #[test]
    fn admission_refuses_a_type_that_nats_would_trim() {
        #[derive(serde::Serialize)]
        struct Padded(f64);

        impl EventPayload for Padded {
            const EVENT_TYPE: &'static str = "order.shipped\u{a0}";
            const SCHEMA_VERSION: u16 = 1;
        }

        let route = Route::<Padded>::new("orders.shipped");
        std::assert_matches!(
            Registry::default().declare(&route),
            Err(RegistryError::InvalidEventType { .. })
        );
        for (value, limit, expected) in [
            (f64::NAN, 0, EnvelopeError::Serialization),
            (1.0, 0, EnvelopeError::PayloadTooLarge),
            (1.0, 1024, EnvelopeError::Identity),
        ] {
            let event = Event {
                id: "event-1".to_owned(),
                occurred_at: time::OffsetDateTime::UNIX_EPOCH.to_utc(),
                payload: Padded(value),
            };
            assert_eq!(
                PreparedEvent::prepare_for_subject("orders.shipped", &event, limit),
                Err(expected)
            );
        }
    }

    fn registry() -> Registry {
        let created = Route::<Created>::new("orders.created");
        let mut registry = Registry::default();
        registry.declare(&created).unwrap();
        registry
            .declare(&Route::<Shipped>::new("orders.shipped"))
            .unwrap();
        registry
            .register(&created, |event, _| async move {
                if event.payload.quantity == 0 {
                    Err(HandlerError::Permanent)
                } else {
                    Ok(())
                }
            })
            .unwrap();
        registry
    }

    #[test]
    fn registry_debug_shows_route_subjects_and_counts() {
        let registry = registry();
        let compact = format!("{registry:?}");
        let pretty = format!("{registry:#?}");
        assert!(compact.contains("routes: {RouteKey {"));
        assert!(pretty.contains("routes: {\n        RouteKey {\n"));
        for output in [&compact, &pretty] {
            for value in [
                "order.created",
                "orders.created",
                "order.shipped",
                "orders.shipped",
                "documented_count: 0",
                "handler_count: 1",
            ] {
                assert!(output.contains(value), "{output}");
            }
            assert!(!output.contains("schema:"), "{output}");
            assert!(!output.contains("handler:"), "{output}");
        }
    }

    async fn dispatch(
        subject: &str,
        event_type: &str,
        schema: &str,
        payload: &'static str,
    ) -> Result<(), DispatchError> {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(crate::wire::header_name::MESSAGE_ID, "event-1");
        headers.insert(crate::wire::header_name::EVENT_TYPE, event_type);
        headers.insert(crate::wire::header_name::EVENT_SCHEMA, schema);
        headers.insert(crate::wire::header_name::CREATED_AT, "2026-09-29T10:00:00Z");
        headers.insert(crate::wire::header_name::NATS_MSG_ID, "event-1");
        let envelope =
            crate::wire::decode_envelope(subject, &headers, Bytes::from_static(payload.as_bytes()))
                .unwrap();
        registry()
            .dispatch(subject, envelope, OperationContext::unbounded())
            .await
    }

    #[tokio::test]
    async fn a_delivery_no_handler_claims_is_told_apart_from_a_handler_rejection() {
        let payload = r#"{"quantity":1}"#;
        assert_eq!(
            dispatch("orders.created", "order.created", "v2", payload).await,
            Ok(())
        );
        for (subject, event_type, schema) in [
            ("orders.created", "order.created", "v3"),
            ("orders.created", "order.cancelled", "v1"),
            ("orders.shipped", "order.created", "v2"),
            ("orders.shipped", "order.shipped", "v1"),
        ] {
            assert_eq!(
                dispatch(subject, event_type, schema, payload).await,
                Err(DispatchError::Unhandled),
                "{subject} {event_type} {schema}"
            );
        }
        assert_eq!(
            dispatch("orders.created", "order.created", "v2", r#"{"quantity":0}"#).await,
            Err(DispatchError::Handler(HandlerError::Permanent))
        );
    }

    #[tokio::test]
    async fn a_payload_the_handler_type_cannot_read_never_reaches_the_handler() {
        for payload in [r#"{"quantity":"many"}"#, r#"{"count":1}"#, "not json"] {
            assert_eq!(
                dispatch("orders.created", "order.created", "v2", payload).await,
                Err(DispatchError::Undecodable),
                "{payload}"
            );
        }
    }
}
