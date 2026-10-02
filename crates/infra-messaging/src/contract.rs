//! The registered routes as an `AsyncAPI` 3.0 document.
//!
//! A route declared with [`Route::documented`](crate::Route::documented)
//! carries its payload type's `utoipa` schema, so the document comes from the
//! same declaration that routes the event and cannot name another subject,
//! type or version.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

use crate::registry::Registry;
use crate::wire::{CREATED_AT, EVENT_SCHEMA, EVENT_TYPE, MESSAGE_ID, NATS_MSG_ID};

/// Appends a payload type's schema, and every schema it references, to the
/// list and returns the payload schema's name.
pub(crate) type PayloadSchema = fn(&mut Vec<(String, RefOr<Schema>)>) -> String;

pub(crate) fn payload_schema<T: ToSchema>(schemas: &mut Vec<(String, RefOr<Schema>)>) -> String {
    let name = T::name().into_owned();
    T::schemas(schemas);
    schemas.push((name.clone(), T::schema()));
    name
}

/// Why the registered routes have no contract document.
#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    /// The route was declared with `Route::new`, which carries no schema.
    #[error("route for {event_type} v{schema_version} has no payload schema")]
    Undocumented {
        event_type: &'static str,
        schema_version: u16,
    },
    /// Two different types render under one schema name; rename one with
    /// `#[schema(as = ...)]`.
    #[error("schema name {0} is used by two different types")]
    SchemaName(String),
    /// A payload schema could not be rendered as JSON.
    #[error("payload schema cannot be rendered")]
    Render(#[source] serde_json::Error),
}

impl Registry {
    /// Renders every route as an `AsyncAPI` 3.0 document: one channel per
    /// subject, one message per event type and schema version with the
    /// envelope headers and the payload schema.
    ///
    /// The document has no operations, because a route does not say whether
    /// this process publishes or consumes it. Keys are sorted, so the same
    /// routes always render the same document.
    ///
    /// # Errors
    /// Rejects a route without a payload schema, two different schemas under
    /// one name, and a schema that cannot be rendered.
    pub fn asyncapi(&self, title: &str, version: &str) -> Result<Value, ContractError> {
        let mut channels = BTreeMap::<&str, BTreeMap<String, Value>>::new();
        let mut schemas = BTreeMap::<String, Value>::new();
        for ((event_type, schema_version), subject, schema) in self.routed() {
            let schema = schema.ok_or(ContractError::Undocumented {
                event_type,
                schema_version,
            })?;
            let mut rendered = Vec::new();
            let payload = schema(&mut rendered);
            for (name, schema) in rendered {
                let schema = serde_json::to_value(schema).map_err(ContractError::Render)?;
                if schemas
                    .insert(name.clone(), schema.clone())
                    .is_some_and(|earlier| earlier != schema)
                {
                    return Err(ContractError::SchemaName(name));
                }
            }
            channels.entry(subject).or_default().insert(
                format!("{event_type}.v{schema_version}"),
                message(event_type, schema_version, &payload),
            );
        }
        let channels: BTreeMap<&str, Value> = channels
            .into_iter()
            .map(|(subject, messages)| {
                (subject, json!({ "address": subject, "messages": messages }))
            })
            .collect();
        Ok(json!({
            "asyncapi": "3.0.0",
            "info": { "title": title, "version": version },
            "defaultContentType": "application/json",
            "channels": channels,
            "components": { "schemas": schemas },
        }))
    }
}

/// One event type and version: the five identity headers every publication
/// carries and a reference to the payload schema.
fn message(event_type: &str, schema_version: u16, payload: &str) -> Value {
    let identity = json!({ "type": "string", "minLength": 1, "maxLength": 256 });
    json!({
        "name": event_type,
        "headers": {
            "type": "object",
            "required": [MESSAGE_ID, EVENT_TYPE, EVENT_SCHEMA, CREATED_AT, NATS_MSG_ID],
            "properties": {
                MESSAGE_ID: identity,
                EVENT_TYPE: { "const": event_type },
                EVENT_SCHEMA: { "const": format!("v{schema_version}") },
                CREATED_AT: { "type": "string", "format": "date-time" },
                NATS_MSG_ID: identity,
            },
        },
        "payload": { "$ref": format!("#/components/schemas/{payload}") },
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixed valid fixtures")]
mod tests {
    use domain_events::EventPayload;
    use serde_json::json;

    use super::ContractError;
    use crate::{Registry, Route};

    #[derive(utoipa::ToSchema)]
    struct Line {
        #[allow(dead_code, reason = "only the schema is read")]
        sku: String,
    }

    #[derive(utoipa::ToSchema)]
    struct OrderCreated {
        #[allow(dead_code, reason = "only the schema is read")]
        lines: Vec<Line>,
        #[allow(dead_code, reason = "only the schema is read")]
        note: Option<String>,
    }

    impl EventPayload for OrderCreated {
        const EVENT_TYPE: &'static str = "order.created";
        const SCHEMA_VERSION: u16 = 2;
    }

    #[derive(utoipa::ToSchema)]
    struct OrderCancelled {
        #[allow(dead_code, reason = "only the schema is read")]
        reason: String,
    }

    impl EventPayload for OrderCancelled {
        const EVENT_TYPE: &'static str = "order.cancelled";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn document_lists_each_subject_with_its_messages_headers_and_payload_schemas() {
        let registry = Registry::new([
            Route::documented::<OrderCreated>("orders.events"),
            Route::documented::<OrderCancelled>("orders.events"),
        ])
        .unwrap();
        let document = registry.asyncapi("orders", "1.4.0").unwrap();

        assert_eq!(document["asyncapi"], "3.0.0");
        assert_eq!(
            document["info"],
            json!({ "title": "orders", "version": "1.4.0" })
        );
        let channel = &document["channels"]["orders.events"];
        assert_eq!(channel["address"], "orders.events");
        assert_eq!(
            channel["messages"]["order.created.v2"],
            json!({
                "name": "order.created",
                "headers": {
                    "type": "object",
                    "required":
                        ["Message-Id", "Event-Type", "Event-Schema", "Created-At", "Nats-Msg-Id"],
                    "properties": {
                        "Message-Id": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "Event-Type": { "const": "order.created" },
                        "Event-Schema": { "const": "v2" },
                        "Created-At": { "type": "string", "format": "date-time" },
                        "Nats-Msg-Id": { "type": "string", "minLength": 1, "maxLength": 256 },
                    },
                },
                "payload": { "$ref": "#/components/schemas/OrderCreated" },
            })
        );
        assert_eq!(
            channel["messages"]["order.cancelled.v1"]["payload"],
            json!({ "$ref": "#/components/schemas/OrderCancelled" })
        );
        assert_eq!(
            document["components"]["schemas"],
            json!({
                "Line": {
                    "type": "object",
                    "required": ["sku"],
                    "properties": { "sku": { "type": "string" } },
                },
                "OrderCancelled": {
                    "type": "object",
                    "required": ["reason"],
                    "properties": { "reason": { "type": "string" } },
                },
                "OrderCreated": {
                    "type": "object",
                    "required": ["lines"],
                    "properties": {
                        "lines": {
                            "type": "array",
                            "items": { "$ref": "#/components/schemas/Line" },
                        },
                        "note": { "type": ["string", "null"] },
                    },
                },
            })
        );
    }

    #[test]
    fn same_routes_render_the_same_text_in_any_declaration_order() {
        let forward = Registry::new([
            Route::documented::<OrderCreated>("orders.created"),
            Route::documented::<OrderCancelled>("orders.cancelled"),
        ])
        .unwrap();
        let backward = Registry::new([
            Route::documented::<OrderCancelled>("orders.cancelled"),
            Route::documented::<OrderCreated>("orders.created"),
        ])
        .unwrap();
        assert_eq!(
            serde_json::to_string(&forward.asyncapi("orders", "1").unwrap()).unwrap(),
            serde_json::to_string(&backward.asyncapi("orders", "1").unwrap()).unwrap()
        );
    }

    #[test]
    fn a_route_without_a_schema_has_no_document() {
        let registry = Registry::new([
            Route::documented::<OrderCreated>("orders.created"),
            Route::new::<OrderCancelled>("orders.cancelled"),
        ])
        .unwrap();
        assert!(matches!(
            registry.asyncapi("orders", "1"),
            Err(ContractError::Undocumented {
                event_type: "order.cancelled",
                schema_version: 1
            })
        ));
    }

    mod other {
        #[derive(utoipa::ToSchema)]
        pub(super) struct Line {
            #[allow(dead_code, reason = "only the schema is read")]
            quantity: u32,
        }

        #[derive(utoipa::ToSchema)]
        pub(super) struct OrderShipped {
            #[allow(dead_code, reason = "only the schema is read")]
            lines: Vec<Line>,
        }

        impl domain_events::EventPayload for OrderShipped {
            const EVENT_TYPE: &'static str = "order.shipped";
            const SCHEMA_VERSION: u16 = 1;
        }
    }

    #[test]
    fn two_different_types_under_one_schema_name_are_refused() {
        let registry = Registry::new([
            Route::documented::<OrderCreated>("orders.created"),
            Route::documented::<other::OrderShipped>("orders.shipped"),
        ])
        .unwrap();
        assert!(matches!(
            registry.asyncapi("orders", "1"),
            Err(ContractError::SchemaName(name)) if name == "Line"
        ));
    }
}
