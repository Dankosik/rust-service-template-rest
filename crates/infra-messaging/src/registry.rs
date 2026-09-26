use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use domain_events::{Event, EventPayload};
use tokio_util::sync::CancellationToken;

use crate::error::{HandlerError, RegistryError};
use crate::prepared::PreparedEvent;
use crate::wire::{InboundEnvelope, valid_subject};

type HandlerFuture = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send>>;
type ErasedHandler = Arc<dyn Fn(InboundEnvelope, CancellationToken) -> HandlerFuture + Send + Sync>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RouteKey {
    event_type: String,
    schema_version: u16,
}

/// Composition-owned subject routing for one typed event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    key: RouteKey,
    subject: String,
}

impl Route {
    /// Declares the subject for an explicitly named payload type.
    #[must_use]
    pub fn new<T: EventPayload>(subject: impl Into<String>) -> Self {
        Self {
            key: RouteKey {
                event_type: T::EVENT_TYPE.to_owned(),
                schema_version: T::SCHEMA_VERSION,
            },
            subject: subject.into(),
        }
    }
}

/// Typed routes and handlers registered before consumer admission.
#[derive(Clone)]
pub struct Registry {
    routes: HashMap<RouteKey, String>,
    handlers: HashMap<RouteKey, ErasedHandler>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Registry")
            .field("routes", &self.routes)
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
        let mut mapped = HashMap::new();
        for route in routes {
            if route.key.schema_version == 0 {
                return Err(RegistryError::InvalidRoute(
                    "schema version must be positive",
                ));
            }
            if route.key.event_type.is_empty()
                || route.key.event_type.len() > 256
                || route.key.event_type.chars().any(char::is_control)
            {
                return Err(RegistryError::InvalidRoute("event type is invalid"));
            }
            if !valid_subject(&route.subject) {
                return Err(RegistryError::InvalidRoute("subject is invalid"));
            }
            let key = route.key.clone();
            if mapped.insert(key.clone(), route.subject).is_some() {
                return Err(RegistryError::DuplicateRoute {
                    event_type: key.event_type,
                    schema_version: key.schema_version,
                });
            }
        }
        Ok(Self {
            routes: mapped,
            handlers: HashMap::new(),
        })
    }

    /// Registers a typed handler without exposing broker metadata to feature code.
    ///
    /// # Errors
    /// Rejects missing routes and a second handler for the same event version.
    pub fn register<T, F, Fut>(&mut self, handler: F) -> Result<(), RegistryError>
    where
        T: EventPayload,
        F: Fn(Event<T>, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        let key = RouteKey {
            event_type: T::EVENT_TYPE.to_owned(),
            schema_version: T::SCHEMA_VERSION,
        };
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
            Arc::new(move |envelope, cancel| {
                let Ok(payload) = serde_json::from_slice::<T>(&envelope.payload) else {
                    return Box::pin(async { Err(HandlerError::Permanent) });
                };
                let Ok(event) = Event::new(envelope.message_id, envelope.occurred_at, payload)
                else {
                    return Box::pin(async { Err(HandlerError::Permanent) });
                };
                Box::pin(handler(event, cancel))
            }),
        );
        Ok(())
    }

    /// Prepares one routed typed event without requiring a live broker.
    ///
    /// # Errors
    /// Rejects an unregistered route or an event outside the payload/wire bound.
    pub fn prepare<T: EventPayload>(
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

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Refuses a consuming worker with no registered typed behavior.
    ///
    /// # Errors
    /// Returns `Empty` when no handler has been registered.
    pub fn validate_consumer(&self) -> Result<(), RegistryError> {
        if self.is_empty() {
            Err(RegistryError::Empty)
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn has_route<T: EventPayload>(&self) -> bool {
        self.routes.contains_key(&RouteKey {
            event_type: T::EVENT_TYPE.to_owned(),
            schema_version: T::SCHEMA_VERSION,
        })
    }

    pub(crate) fn subject_for<T: EventPayload>(&self) -> Option<&str> {
        self.routes
            .get(&RouteKey {
                event_type: T::EVENT_TYPE.to_owned(),
                schema_version: T::SCHEMA_VERSION,
            })
            .map(String::as_str)
    }

    pub(crate) fn subjects(&self) -> impl Iterator<Item = &str> {
        self.routes.values().map(String::as_str)
    }

    pub(crate) async fn dispatch(
        &self,
        subject: &str,
        envelope: InboundEnvelope,
        cancel: CancellationToken,
    ) -> Result<(), HandlerError> {
        let key = RouteKey {
            event_type: envelope.event_type.clone(),
            schema_version: envelope.schema_version,
        };
        if self.routes.get(&key).is_none_or(|route| route != subject) {
            return Err(HandlerError::Permanent);
        }
        let Some(handler) = self.handlers.get(&key) else {
            return Err(HandlerError::Permanent);
        };
        handler(envelope, cancel).await
    }
}
