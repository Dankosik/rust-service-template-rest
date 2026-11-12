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
use crate::error::{EnvelopeError, HandlerError};
use crate::prepared::PreparedEvent;
use crate::wire::{InboundEnvelope, valid_subject};
use bytes::Bytes;
use time::OffsetDateTime;

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send>>;
/// Starts the typed handler with an event's ID, time and payload, or returns
/// `None` when the payload is not the handler's type.
type ErasedHandler = Arc<
    dyn Fn(String, OffsetDateTime, Bytes, OperationContext) -> Option<HandlerFuture> + Send + Sync,
>;

/// Why a delivery ended without a handler's success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DispatchError {
    /// No handler is registered for this event type and schema version on
    /// this subject. The handler never ran.
    Unhandled,
    /// The payload is not the JSON the handler's type reads. The handler
    /// never ran.
    Undecodable,
    Handler(HandlerError),
}

/// Event type and schema version. Every registered key comes from an
/// [`EventPayload`] constant, so building one to look up a route does not
/// allocate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RouteKey<'a> {
    pub(crate) event_type: &'a str,
    pub(crate) schema_version: u16,
}

/// The route key of a payload type; rejects an invalid event type and
/// version zero at compile time.
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

/// Composition-owned subject routing for one typed event.
#[derive(Clone)]
pub struct Route {
    pub(crate) key: RouteKey<'static>,
    pub(crate) subject: String,
    pub(crate) schema: Option<PayloadSchema>,
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

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
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
    #[error("route missing for {event_type} v{schema_version}")]
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

/// Typed routes and handlers registered before consumer admission.
#[derive(Clone)]
pub struct Registry {
    routes: HashMap<RouteKey<'static>, RouteEntry>,
}

/// What one route key carries: the route itself and its handler once registered.
#[derive(Clone)]
struct RouteEntry {
    route: Route,
    handler: Option<ErasedHandler>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subjects: HashMap<_, _> = self
            .routes
            .iter()
            .map(|(key, entry)| (key, &entry.route.subject))
            .collect();
        formatter
            .debug_struct("Registry")
            .field("routes", &subjects)
            .field(
                "documented_count",
                &self
                    .routes
                    .values()
                    .filter(|entry| entry.route.schema.is_some())
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
    /// Creates a registry with all composition-owned routes.
    ///
    /// # Errors
    /// Rejects duplicate routes and invalid subjects.
    pub fn try_new(routes: impl IntoIterator<Item = Route>) -> Result<Self, RegistryError> {
        let mut mapped = HashMap::default();
        for route in routes {
            let key @ RouteKey {
                event_type,
                schema_version,
            } = route.key;
            // The subject itself stays out of the error: an invalid one can
            // hold a line break.
            if !valid_subject(&route.subject) {
                return Err(RegistryError::InvalidSubject {
                    event_type: event_type.to_owned(),
                    schema_version,
                });
            }
            let entry = RouteEntry {
                route,
                handler: None,
            };
            if mapped.insert(key, entry).is_some() {
                return Err(RegistryError::DuplicateRoute {
                    event_type: event_type.to_owned(),
                    schema_version,
                });
            }
        }
        Ok(Self { routes: mapped })
    }

    /// Registers a typed handler. It receives the event and an
    /// [`OperationContext`] carrying the fixed delivery deadline and
    /// cancellation; broker metadata stays outside feature code.
    ///
    /// # Errors
    /// Rejects missing routes and a second handler for the same event version.
    pub fn register<T, F, Fut>(&mut self, handler: F) -> Result<(), RegistryError>
    where
        T: EventPayload + DeserializeOwned + 'static,
        F: Fn(Event<T>, OperationContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        let Some(entry) = self.routes.get_mut(&route_key::<T>()) else {
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

    /// Prepares one routed typed event without requiring a live broker.
    ///
    /// # Errors
    /// Rejects an unregistered route or an event outside the payload/wire bound.
    pub fn prepare<T: EventPayload + Serialize>(
        &self,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<PreparedEvent, EnvelopeError> {
        let subject = self.subject_for::<T>().ok_or(EnvelopeError::MissingRoute {
            event_type: T::EVENT_TYPE,
            schema_version: T::SCHEMA_VERSION,
        })?;
        PreparedEvent::prepare(subject, event, max_payload_bytes)
    }

    /// Whether a handler is registered. Routes alone do not count: a
    /// registry with routes and no handler only publishes.
    #[must_use]
    pub fn has_handlers(&self) -> bool {
        self.routes.values().any(|entry| entry.handler.is_some())
    }

    /// Refuses a consuming worker with no registered typed behavior.
    ///
    /// # Errors
    /// Returns `Empty` when no handler has been registered.
    pub fn require_handlers(&self) -> Result<(), RegistryError> {
        if self.has_handlers() {
            Ok(())
        } else {
            Err(RegistryError::Empty)
        }
    }

    fn subject_for<T: EventPayload>(&self) -> Option<&str> {
        self.routes
            .get(&route_key::<T>())
            .map(|entry| entry.route.subject.as_str())
    }

    /// Every registered route.
    pub(crate) fn routes(&self) -> impl Iterator<Item = &Route> {
        self.routes.values().map(|entry| &entry.route)
    }

    /// Every route with a registered handler. A route without one is one
    /// this process only publishes to.
    pub(crate) fn handled_routes(&self) -> impl Iterator<Item = &Route> {
        self.routes
            .values()
            .filter(|entry| entry.handler.is_some())
            .map(|entry| &entry.route)
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
        // The map is covariant in the key, so the inbound type looks up the
        // `'static` keys without a copy.
        let routes: &HashMap<RouteKey<'_>, RouteEntry> = &self.routes;
        let handler = routes
            .get(&RouteKey {
                event_type: &event_type,
                schema_version,
            })
            .filter(|entry| entry.route.subject == subject)
            .and_then(|entry| entry.handler.as_ref())
            .ok_or(DispatchError::Unhandled)?;
        let run =
            handler(message_id, occurred_at, payload, context).ok_or(DispatchError::Undecodable)?;
        run.await.map_err(DispatchError::Handler)
    }
}

#[cfg(test)]
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
        let mut registry = Registry::try_new([
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
