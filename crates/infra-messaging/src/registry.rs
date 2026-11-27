use std::any::TypeId;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use domain_events::{Event, EventPayload};
use foldhash::HashMap;
use operation_context::OperationContext;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::contract::{PayloadSchema, payload_schema};
use crate::error::HandlerError;
use crate::wire::{InboundEnvelopeRef, valid_subject};

type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send + 'a>>;

pub trait EventHandler<T>: Send + Sync + 'static {
    fn handle(
        &self,
        event: Event<T>,
        context: OperationContext,
    ) -> impl Future<Output = Result<(), HandlerError>> + Send;
}

impl<T, F, Fut> EventHandler<T> for F
where
    F: Fn(Event<T>, OperationContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
{
    fn handle(
        &self,
        event: Event<T>,
        context: OperationContext,
    ) -> impl Future<Output = Result<(), HandlerError>> + Send {
        self(event, context)
    }
}

trait ErasedEventHandler: Send + Sync {
    fn call(
        &self,
        id: &str,
        occurred_at: time::UtcDateTime,
        payload_json: &[u8],
        context: OperationContext,
    ) -> Result<HandlerFuture<'_>, serde_json::Error>;
}

struct TypedEventHandler<T, H> {
    handler: H,
    payload: PhantomData<fn(T)>,
}

impl<T, H> ErasedEventHandler for TypedEventHandler<T, H>
where
    T: EventPayload + DeserializeOwned,
    H: EventHandler<T>,
{
    fn call(
        &self,
        id: &str,
        occurred_at: time::UtcDateTime,
        payload_json: &[u8],
        context: OperationContext,
    ) -> Result<HandlerFuture<'_>, serde_json::Error> {
        let payload = serde_json::from_slice::<T>(payload_json)?;
        let event = Event {
            id: id.to_owned(),
            occurred_at,
            payload,
        };
        Ok(Box::pin(self.handler.handle(event, context)))
    }
}

#[derive(Clone)]
pub(crate) struct BoundHandler(Arc<dyn ErasedEventHandler>);

impl std::fmt::Debug for BoundHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BoundHandler")
    }
}

impl BoundHandler {
    pub(crate) async fn dispatch(
        &self,
        envelope: InboundEnvelopeRef<'_>,
        context: OperationContext,
    ) -> Result<(), DispatchError> {
        let run = self
            .0
            .call(
                envelope.message_id,
                envelope.occurred_at,
                envelope.payload,
                context,
            )
            .map_err(|_| DispatchError::Undecodable)?;
        run.await.map_err(DispatchError::Handler)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchError {
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

#[derive_where::derive_where(Clone)]
pub struct Route<T> {
    subject: async_nats::Subject,
    schema: Option<PayloadSchema>,
    payload: PhantomData<fn(T) -> T>,
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

#[derive_where::derive_where(Clone)]
pub struct PublicationRoute<T> {
    route: Route<T>,
    max_payload_bytes: usize,
}

impl<T: EventPayload> std::fmt::Debug for PublicationRoute<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicationRoute")
            .field("route", &self.route)
            .field("max_payload_bytes", &self.max_payload_bytes)
            .finish()
    }
}

impl<T: EventPayload + Serialize> PublicationRoute<T> {
    pub fn prepare(&self, event: &Event<T>) -> Result<crate::PreparedEvent, crate::PrepareError> {
        self.prepare_with_message_id(event, || event.id.clone())
    }

    pub(crate) fn prepare_with_message_id(
        &self,
        event: &Event<T>,
        make_message_id: impl FnOnce() -> String,
    ) -> Result<crate::PreparedEvent, crate::PrepareError> {
        crate::PreparedEvent::prepare_with_message_id(
            self.route.subject.clone(),
            event,
            self.max_payload_bytes,
            make_message_id,
        )
    }
}

pub struct RouteEntry<'a, T> {
    registered: &'a mut RegisteredRoute,
    max_publication_bytes: &'a mut usize,
    payload: PhantomData<fn(T) -> T>,
}

impl<T: EventPayload> std::fmt::Debug for RouteEntry<'_, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RouteEntry")
            .field("key", &route_key::<T>())
            .field("subject", &self.registered.subject)
            .field("documented", &self.registered.schema.is_some())
            .finish_non_exhaustive()
    }
}

impl<T: EventPayload + 'static> RouteEntry<'_, T> {
    #[must_use]
    pub fn publish(&mut self, max_payload_bytes: usize) -> PublicationRoute<T> {
        *self.max_publication_bytes = (*self.max_publication_bytes).max(max_payload_bytes);
        PublicationRoute {
            route: Route {
                subject: self.registered.subject.clone(),
                schema: self.registered.schema,
                payload: PhantomData,
            },
            max_payload_bytes,
        }
    }

    pub fn handle(self, handler: impl EventHandler<T>)
    where
        T: DeserializeOwned,
    {
        self.registered.bind_handler(handler);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("route event type {event_type:?} v{schema_version} is invalid")]
    InvalidEventType {
        event_type: &'static str,
        schema_version: u16,
    },
    #[error("route subject is invalid for {event_type} v{schema_version}")]
    InvalidSubject {
        event_type: &'static str,
        schema_version: u16,
    },
    #[error("duplicate route for {event_type} v{schema_version}")]
    DuplicateRoute {
        event_type: &'static str,
        schema_version: u16,
    },
    #[error("typed route is not registered for {event_type} v{schema_version}")]
    MissingRoute {
        event_type: &'static str,
        schema_version: u16,
    },
    #[error("duplicate handler for {event_type} v{schema_version}")]
    DuplicateHandler {
        event_type: &'static str,
        schema_version: u16,
    },
    #[error(
        "declared publication limit {declared} bytes exceeds the publisher limit {available} bytes"
    )]
    PublicationLimit { declared: usize, available: usize },
    #[error("no typed event handlers are registered")]
    Empty,
}

#[derive(Clone, Default)]
pub struct Registry {
    routes: HashMap<RouteKey<'static>, RegisteredRoute>,
    max_publication_bytes: usize,
}

#[derive(Clone)]
pub(crate) struct RegisteredRoute {
    pub(crate) subject: async_nats::Subject,
    pub(crate) schema: Option<PayloadSchema>,
    payload_type: TypeId,
    handler: Option<Arc<dyn ErasedEventHandler>>,
}

impl RegisteredRoute {
    fn matches<T: 'static>(&self, route: &Route<T>) -> bool {
        self.payload_type == TypeId::of::<T>()
            && self.subject == route.subject
            && self.schema.is_some() == route.schema.is_some()
    }

    fn bind_handler<T: EventPayload + DeserializeOwned + 'static>(
        &mut self,
        handler: impl EventHandler<T>,
    ) {
        self.handler = Some(Arc::new(TypedEventHandler {
            handler,
            payload: PhantomData,
        }));
    }

    const fn is_handled(&self) -> bool {
        self.handler.is_some()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subjects = std::fmt::from_fn(|f| {
            f.debug_map()
                .entries(self.routes.iter().map(|(key, route)| (key, &route.subject)))
                .finish()
        });
        formatter
            .debug_struct("Registry")
            .field("routes", &subjects)
            .field("max_publication_bytes", &self.max_publication_bytes)
            .field(
                "documented_count",
                &self
                    .routes
                    .values()
                    .filter(|route| route.schema.is_some())
                    .count(),
            )
            .field(
                "handler_count",
                &self
                    .routes
                    .values()
                    .filter(|route| route.is_handled())
                    .count(),
            )
            .finish()
    }
}

impl Registry {
    pub fn declare<T: EventPayload + 'static>(
        &mut self,
        route: &Route<T>,
    ) -> Result<RouteEntry<'_, T>, RegistryError> {
        let key @ RouteKey {
            event_type,
            schema_version,
        } = route_key::<T>();
        if !valid_subject(&route.subject) {
            return Err(RegistryError::InvalidSubject {
                event_type,
                schema_version,
            });
        }
        if !crate::wire::valid_outbound_identity(event_type) {
            return Err(RegistryError::InvalidEventType {
                event_type,
                schema_version,
            });
        }
        let std::collections::hash_map::Entry::Vacant(vacant) = self.routes.entry(key) else {
            return Err(RegistryError::DuplicateRoute {
                event_type,
                schema_version,
            });
        };
        let registered = vacant.insert(RegisteredRoute {
            subject: route.subject.clone(),
            schema: route.schema,
            payload_type: TypeId::of::<T>(),
            handler: None,
        });
        Ok(RouteEntry {
            registered,
            max_publication_bytes: &mut self.max_publication_bytes,
            payload: PhantomData,
        })
    }

    pub fn register<T>(
        &mut self,
        route: &Route<T>,
        handler: impl EventHandler<T>,
    ) -> Result<(), RegistryError>
    where
        T: EventPayload + DeserializeOwned + 'static,
    {
        let key = route_key::<T>();
        let Some(registered) = self
            .routes
            .get_mut(&key)
            .filter(|registered| registered.matches(route))
        else {
            return Err(RegistryError::MissingRoute {
                event_type: key.event_type,
                schema_version: key.schema_version,
            });
        };
        if registered.is_handled() {
            return Err(RegistryError::DuplicateHandler {
                event_type: key.event_type,
                schema_version: key.schema_version,
            });
        }
        registered.bind_handler(handler);
        Ok(())
    }

    #[must_use]
    pub fn has_handlers(&self) -> bool {
        self.routes.values().any(RegisteredRoute::is_handled)
    }

    pub fn require_handlers(&self) -> Result<(), RegistryError> {
        self.has_handlers().ok_or(RegistryError::Empty)
    }

    pub fn declare_publication<T: EventPayload + 'static>(
        &mut self,
        route: &Route<T>,
        max_payload_bytes: usize,
    ) -> Result<PublicationRoute<T>, RegistryError> {
        Ok(self.declare(route)?.publish(max_payload_bytes))
    }

    pub fn check_publication_limit(&self, available: usize) -> Result<(), RegistryError> {
        if self.max_publication_bytes > available {
            return Err(RegistryError::PublicationLimit {
                declared: self.max_publication_bytes,
                available,
            });
        }
        Ok(())
    }

    pub(crate) fn routes(&self) -> impl Iterator<Item = (&RouteKey<'static>, &RegisteredRoute)> {
        self.routes.iter()
    }

    pub(crate) fn handled_routes(
        &self,
    ) -> impl Iterator<Item = (&RouteKey<'static>, &RegisteredRoute)> {
        self.routes().filter(|(_, route)| route.is_handled())
    }

    pub(crate) fn into_handled_routes(
        self,
    ) -> impl Iterator<Item = (RouteKey<'static>, async_nats::Subject, BoundHandler)> {
        self.routes.into_iter().filter_map(|(key, route)| {
            route
                .handler
                .map(|handler| (key, route.subject, BoundHandler(handler)))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn accept<T>(_: Event<T>, _: OperationContext) -> std::future::Ready<Result<(), HandlerError>> {
        std::future::ready(Ok(()))
    }

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
        let collision = Route::<Created>::documented("orders.created");
        let mut registry = Registry::default();
        let publication = registry.declare_publication(&route, 1024).unwrap();
        std::assert_matches!(
            registry.declare_publication(&collision, 1025),
            Err(RegistryError::DuplicateRoute { .. })
        );
        std::assert_matches!(
            registry.register(&collision, accept),
            Err(RegistryError::MissingRoute { .. })
        );
        assert!(registry.check_publication_limit(1024).is_ok());
        std::assert_matches!(
            registry.check_publication_limit(1023),
            Err(RegistryError::PublicationLimit {
                declared: 1024,
                available: 1023
            })
        );

        let event = Event {
            id: "event-1".to_owned(),
            occurred_at: time::UtcDateTime::UNIX_EPOCH,
            payload: Alias("accepted".to_owned()),
        };
        for undeclared in [
            Route::<Alias>::documented("another.subject"),
            Route::<Alias>::new("orders.created"),
        ] {
            std::assert_matches!(
                registry.register(&undeclared, accept),
                Err(RegistryError::MissingRoute { .. })
            );
        }
        let prepared = publication.prepare(&event).unwrap();
        assert_eq!(prepared.subject(), "orders.created");
        assert_eq!(prepared.payload().as_ref(), br#""accepted""#);
        registry.register(&route, accept).unwrap();
        std::assert_matches!(
            registry.register(&route, accept),
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
    fn admission_refuses_a_type_that_nats_would_trim() {
        #[derive(serde::Serialize)]
        struct Padded;

        impl EventPayload for Padded {
            const EVENT_TYPE: &'static str = "order.shipped\u{a0}";
            const SCHEMA_VERSION: u16 = 1;
        }

        let route = Route::<Padded>::new("orders.shipped");
        std::assert_matches!(
            Registry::default().declare(&route),
            Err(RegistryError::InvalidEventType { .. })
        );
    }

    fn registry() -> Registry {
        let created = Route::<Created>::new("orders.created");
        let mut registry = Registry::default();
        registry.declare(&created).unwrap().handle(
            |event: Event<Created>, _: OperationContext| async move {
                (event.payload.quantity != 0).ok_or(HandlerError::Permanent)
            },
        );
        registry
            .declare(&Route::<Shipped>::new("orders.shipped"))
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

    fn handled_handler(registry: Registry, subject: &str, key: RouteKey<'_>) -> BoundHandler {
        let routes: Vec<_> = registry.into_handled_routes().collect();
        assert_eq!(routes.len(), 1);
        let (route_key, route_subject, handler) = routes.into_iter().next().unwrap();
        assert_eq!(route_key, key);
        assert_eq!(route_subject.as_str(), subject);
        handler
    }

    async fn dispatch_bound(
        handler: &BoundHandler,
        subject: &str,
        event_type: &str,
        schema: &str,
        payload: &str,
    ) -> Result<(), DispatchError> {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(crate::wire::header_name::MESSAGE_ID, "event-1");
        headers.insert(crate::wire::header_name::EVENT_TYPE, event_type);
        headers.insert(crate::wire::header_name::EVENT_SCHEMA, schema);
        headers.insert(crate::wire::header_name::CREATED_AT, "2026-09-29T10:00:00Z");
        headers.insert(crate::wire::header_name::NATS_MSG_ID, "event-1");
        let envelope =
            crate::wire::decode_envelope_ref(subject, &headers, payload.as_bytes()).unwrap();
        handler
            .dispatch(envelope, OperationContext::unbounded())
            .await
    }

    #[tokio::test]
    async fn handled_routes_exclude_publish_only_routes_and_preserve_handler_outcomes() {
        let handler = handled_handler(registry(), "orders.created", route_key::<Created>());
        assert_eq!(
            dispatch_bound(
                &handler,
                "orders.created",
                "order.created",
                "v2",
                r#"{"quantity":1}"#,
            )
            .await,
            Ok(())
        );
        assert_eq!(
            dispatch_bound(
                &handler,
                "orders.created",
                "order.created",
                "v2",
                r#"{"quantity":0}"#,
            )
            .await,
            Err(DispatchError::Handler(HandlerError::Permanent))
        );
    }

    #[tokio::test]
    async fn a_payload_the_handler_type_cannot_read_never_reaches_the_handler() {
        let handler = handled_handler(registry(), "orders.created", route_key::<Created>());
        for payload in [r#"{"quantity":"many"}"#, r#"{"count":1}"#, "not json"] {
            assert_eq!(
                dispatch_bound(&handler, "orders.created", "order.created", "v2", payload).await,
                Err(DispatchError::Undecodable),
                "{payload}"
            );
        }
    }

    #[tokio::test]
    async fn stateful_event_handler_borrows_self_across_await() {
        struct QuantityAccumulator {
            total: Arc<AtomicU32>,
        }

        impl EventHandler<Created> for QuantityAccumulator {
            async fn handle(
                &self,
                event: Event<Created>,
                _context: OperationContext,
            ) -> Result<(), HandlerError> {
                tokio::task::yield_now().await;
                self.total
                    .fetch_add(event.payload.quantity, Ordering::SeqCst);
                Ok(())
            }
        }

        let total = Arc::new(AtomicU32::new(0));
        let created = Route::<Created>::new("orders.created");
        let mut registry = Registry::default();
        let mut entry = registry.declare(&created).unwrap();
        let publication = entry.publish(128);
        entry.handle(QuantityAccumulator {
            total: Arc::clone(&total),
        });
        assert!(registry.check_publication_limit(128).is_ok());
        assert!(registry.check_publication_limit(127).is_err());
        let prepared = publication
            .prepare(&Event {
                id: "event-1".to_owned(),
                occurred_at: time::UtcDateTime::UNIX_EPOCH,
                payload: Created { quantity: 7 },
            })
            .unwrap();

        let headers = crate::wire::encode_prepared(&prepared);
        let envelope =
            crate::wire::decode_envelope_ref(prepared.subject(), &headers, prepared.payload())
                .unwrap();
        let handler = handled_handler(registry, "orders.created", route_key::<Created>());
        assert_eq!(
            handler
                .dispatch(envelope, OperationContext::unbounded())
                .await,
            Ok(())
        );
        assert_eq!(total.load(Ordering::SeqCst), 7);
    }
}
