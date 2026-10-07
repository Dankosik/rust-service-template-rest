use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use domain_events::{Event, EventPayload};
use operation_context::OperationContext;
use serde::Serialize;
use serde::de::DeserializeOwned;
// Every key is a compile-time constant, so a caller cannot choose colliding
// keys; SipHash would cost two thirds of each lookup.
use foldhash::HashMap;

use crate::contract::{PayloadSchema, payload_schema};
use crate::error::{HandlerError, RegistryError};
use crate::prepared::PreparedEvent;
use crate::wire::{InboundEnvelope, valid_subject};

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send>>;
/// Starts the typed handler, or returns `None` when the payload is not the
/// handler's type.
type ErasedHandler =
    Arc<dyn Fn(InboundEnvelope, OperationContext) -> Option<HandlerFuture> + Send + Sync>;

/// Why a delivery ended without a handler's success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchError {
    /// No handler is registered for this event type and schema version on
    /// this subject. The handler never ran.
    Unhandled,
    /// The payload is not the JSON the handler's type reads. The handler
    /// never ran.
    Undecodable,
    /// The handler ran and returned this classification.
    Handler(HandlerError),
}

/// Event type and schema version. Every key comes from an [`EventPayload`]
/// constant, so building one to look up a route does not allocate.
pub(crate) type RouteKey = (&'static str, u16);

/// The route key of a payload type; rejects version zero at compile time.
const fn route_key<T: EventPayload>() -> RouteKey {
    const {
        assert!(
            T::SCHEMA_VERSION > 0,
            "event schema version must be positive"
        );
    }
    (T::EVENT_TYPE, T::SCHEMA_VERSION)
}

/// Composition-owned subject routing for one typed event.
#[derive(Clone)]
pub struct Route {
    key: RouteKey,
    subject: String,
    schema: Option<PayloadSchema>,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Route")
            .field("key", &self.key)
            .field("subject", &self.subject)
            .field("documented", &self.schema.is_some())
            .finish()
    }
}

impl Route {
    /// Declares the subject for an explicitly named payload type.
    #[must_use]
    pub fn new<T: EventPayload>(subject: impl Into<String>) -> Self {
        Self {
            key: route_key::<T>(),
            subject: subject.into(),
            schema: None,
        }
    }

    /// Declares the subject and keeps the payload type's schema, so
    /// [`Registry::asyncapi`] can describe the event.
    #[must_use]
    pub fn documented<T: EventPayload + utoipa::ToSchema>(subject: impl Into<String>) -> Self {
        Self {
            schema: Some(payload_schema::<T>),
            ..Self::new::<T>(subject)
        }
    }
}

/// Typed routes and handlers registered before consumer admission.
#[derive(Clone)]
pub struct Registry {
    routes: HashMap<RouteKey, String>,
    /// Payload schemas of the routes declared with [`Route::documented`].
    schemas: HashMap<RouteKey, PayloadSchema>,
    handlers: HashMap<RouteKey, ErasedHandler>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Registry")
            .field("routes", &self.routes)
            .field("documented_count", &self.schemas.len())
            .field("handler_count", &self.handlers.len())
            .finish()
    }
}

impl Registry {
    /// Creates a registry with all composition-owned routes.
    ///
    /// # Errors
    /// Rejects duplicate routes or invalid type, schema and subject declarations.
    pub fn new(routes: impl IntoIterator<Item = Route>) -> Result<Self, RegistryError> {
        let mut mapped = HashMap::default();
        let mut schemas = HashMap::default();
        for route in routes {
            if crate::wire::validate_text(route.key.0).is_err() {
                return Err(RegistryError::InvalidRoute("event type is invalid"));
            }
            if !valid_subject(&route.subject) {
                return Err(RegistryError::InvalidRoute("subject is invalid"));
            }
            let (event_type, schema_version) = route.key;
            if let Some(schema) = route.schema {
                schemas.insert(route.key, schema);
            }
            if mapped.insert(route.key, route.subject).is_some() {
                return Err(RegistryError::DuplicateRoute {
                    event_type: event_type.to_owned(),
                    schema_version,
                });
            }
        }
        Ok(Self {
            routes: mapped,
            schemas,
            handlers: HashMap::default(),
        })
    }

    /// Registers a typed handler with its fixed delivery deadline and cancellation.
    /// Broker metadata stays outside feature code.
    ///
    /// # Errors
    /// Rejects missing routes and a second handler for the same event version.
    pub fn register<T, F, Fut>(&mut self, handler: F) -> Result<(), RegistryError>
    where
        T: EventPayload + DeserializeOwned + 'static,
        F: Fn(Event<T>, OperationContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        let key = route_key::<T>();
        if !self.routes.contains_key(&key) {
            return Err(RegistryError::MissingRoute {
                event_type: T::EVENT_TYPE.to_owned(),
                schema_version: T::SCHEMA_VERSION,
            });
        }
        if self.handlers.contains_key(&key) {
            return Err(RegistryError::DuplicateHandler {
                event_type: T::EVENT_TYPE.to_owned(),
                schema_version: T::SCHEMA_VERSION,
            });
        }
        let handler = Arc::new(handler);
        self.handlers.insert(
            key,
            Arc::new(move |envelope, context| {
                let payload = serde_json::from_slice::<T>(&envelope.payload).ok()?;
                let event = Event {
                    id: envelope.message_id,
                    occurred_at: envelope.occurred_at.to_utc(),
                    payload,
                };
                Some(Box::pin(handler(event, context)))
            }),
        );
        Ok(())
    }

    /// Prepares one routed typed event without requiring a live broker.
    ///
    /// # Errors
    /// Rejects an unregistered route or an event outside the payload/wire bound.
    pub fn prepare<T: EventPayload + Serialize>(
        &self,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<PreparedEvent, crate::MessagingError> {
        let subject = self
            .subject_for::<T>()
            .ok_or(crate::MessagingError::Envelope(
                "event route is not registered",
            ))?;
        PreparedEvent::prepare(subject, event, max_payload_bytes)
    }

    /// Whether a handler is registered. Routes alone do not count: a
    /// registry with routes and no handler only publishes.
    #[must_use]
    pub fn has_handlers(&self) -> bool {
        !self.handlers.is_empty()
    }

    /// Refuses a consuming worker with no registered typed behavior.
    ///
    /// # Errors
    /// Returns `Empty` when no handler has been registered.
    pub fn validate_consumer(&self) -> Result<(), RegistryError> {
        if self.has_handlers() {
            Ok(())
        } else {
            Err(RegistryError::Empty)
        }
    }

    #[must_use]
    pub fn has_route<T: EventPayload>(&self) -> bool {
        self.routes.contains_key(&route_key::<T>())
    }

    pub(crate) fn subject_for<T: EventPayload>(&self) -> Option<&str> {
        self.routes.get(&route_key::<T>()).map(String::as_str)
    }

    /// The route key and subject of every registered handler. A route
    /// without a handler is one this process only publishes to.
    pub(crate) fn handled(&self) -> impl Iterator<Item = (RouteKey, &str)> {
        self.handlers
            .keys()
            .filter_map(|key| Some((*key, self.routes.get(key)?.as_str())))
    }

    pub(crate) async fn dispatch(
        &self,
        subject: &str,
        mut envelope: InboundEnvelope,
        context: OperationContext,
    ) -> Result<(), DispatchError> {
        // The maps are covariant in the key, so the inbound type looks up the
        // `'static` keys without a copy. Typed handlers never read it.
        let event_type = std::mem::take(&mut envelope.event_type);
        let key = (event_type.as_str(), envelope.schema_version);
        let routes: &HashMap<(&str, u16), String> = &self.routes;
        if routes.get(&key).is_none_or(|route| route != subject) {
            return Err(DispatchError::Unhandled);
        }
        let handlers: &HashMap<(&str, u16), ErasedHandler> = &self.handlers;
        let handler = handlers.get(&key).ok_or(DispatchError::Unhandled)?;
        let run = handler(envelope, context).ok_or(DispatchError::Undecodable)?;
        run.await.map_err(DispatchError::Handler)
    }

    /// Every route with its subject and, when documented, its payload schema.
    pub(crate) fn routed(
        &self,
    ) -> impl Iterator<Item = ((&'static str, u16), &str, Option<PayloadSchema>)> {
        self.routes
            .iter()
            .map(|(key, subject)| (*key, subject.as_str(), self.schemas.get(key).copied()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed valid fixtures")]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[derive(serde::Deserialize)]
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

    /// Routes both types and handles `Created`, rejecting a zero quantity.
    fn registry() -> Registry {
        let mut registry = Registry::new([
            Route::new::<Created>("orders.created"),
            Route::new::<Shipped>("orders.shipped"),
        ])
        .unwrap();
        registry
            .register::<Created, _, _>(|event, _| async move {
                if event.payload.quantity == 0 {
                    Err(HandlerError::Permanent)
                } else {
                    Ok(())
                }
            })
            .unwrap();
        registry
    }

    async fn dispatch(
        subject: &str,
        event_type: &str,
        schema: &str,
        payload: &'static str,
    ) -> Result<(), DispatchError> {
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(crate::wire::name::MESSAGE_ID, "event-1");
        headers.insert(crate::wire::name::EVENT_TYPE, event_type);
        headers.insert(crate::wire::name::EVENT_SCHEMA, schema);
        headers.insert(crate::wire::name::CREATED_AT, "2026-09-29T10:00:00Z");
        headers.insert(crate::wire::name::NATS_MSG_ID, "event-1");
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
            // A schema version published before its consumer was deployed.
            ("orders.created", "order.created", "v3"),
            // A type no route knows.
            ("orders.created", "order.cancelled", "v1"),
            // A handled type on another subject.
            ("orders.shipped", "order.created", "v2"),
            // A route this process only publishes to.
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
