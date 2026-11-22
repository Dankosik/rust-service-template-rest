use std::collections::BTreeMap;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use utoipa::ToSchema;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::Schema;

use crate::registry::{Registry, RouteKey};
use crate::wire::{
    CREATED_AT, EVENT_SCHEMA, EVENT_TYPE, IDENTITY_MAX_BYTES, IDENTITY_SCHEMA_PATTERN, MESSAGE_ID,
    NATS_MSG_ID, schema_label,
};

type Definitions = Vec<(String, RefOr<Schema>)>;
pub(crate) type PayloadSchema = fn() -> (String, Definitions);

pub(crate) fn payload_schema<T: ToSchema>() -> (String, Definitions) {
    let name = T::name().into_owned();
    let mut schemas = Vec::new();
    T::schemas(&mut schemas);
    schemas.push((name.clone(), T::schema()));
    (name, schemas)
}

#[derive(Debug, thiserror::Error)]
pub enum ContractError {
    #[error("route for {event_type} v{schema_version} has no payload schema")]
    Undocumented {
        event_type: &'static str,
        schema_version: u16,
    },
    #[error("schema name {0} is used by two different types")]
    SchemaName(String),
    #[error("payload schema cannot be rendered")]
    Render(#[from] serde_json::Error),
}

impl Registry {
    pub fn asyncapi(&self, title: &str, version: &str) -> Result<Value, ContractError> {
        let mut channel_messages = BTreeMap::<&str, BTreeMap<String, Value>>::new();
        let mut seen_schemas = BTreeMap::<String, Value>::new();
        for (key, route) in self.routes() {
            let RouteKey {
                event_type,
                schema_version,
            } = *key;
            let subject = route.subject.as_str();
            let collect_schema = route.schema.ok_or(ContractError::Undocumented {
                event_type,
                schema_version,
            })?;
            let (payload_schema_name, raw_definitions) = collect_schema();
            let definitions = render_definitions(raw_definitions, &mut seen_schemas)?;
            channel_messages.entry(subject).or_default().insert(
                format!("{event_type}.v{schema_version}"),
                message(
                    event_type,
                    schema_version,
                    &payload_schema_name,
                    &definitions,
                ),
            );
        }
        let channels: BTreeMap<&str, Value> = channel_messages
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
        }))
    }
}

fn render_definitions(
    raw_definitions: Definitions,
    seen_schemas: &mut BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>, ContractError> {
    let mut definitions = BTreeMap::new();
    for (name, raw_schema) in raw_definitions {
        let rendered = serde_json::to_value(raw_schema)?;
        if let Some(original) = seen_schemas.get(&name) {
            if *original != rendered {
                return Err(ContractError::SchemaName(name));
            }
        } else {
            seen_schemas.insert(name.clone(), rendered.clone());
        }
        let mut definition = rendered;
        relocate_schema_refs(&mut definition);
        definitions.insert(name, definition);
    }
    Ok(definitions)
}

const CREATED_AT_DESCRIPTION: &str =
    "RFC 3339 time; a consumer refuses the zero time 0001-01-01T00:00:00Z as malformed.";

fn message(
    event_type: &str,
    schema_version: u16,
    payload_schema_name: &str,
    definitions: &BTreeMap<String, Value>,
) -> Value {
    let identity = json!({
        "type": "string",
        "minLength": 1,
        "maxLength": IDENTITY_MAX_BYTES,
        "pattern": IDENTITY_SCHEMA_PATTERN,
        "description": format!(
            "At most {IDENTITY_MAX_BYTES} UTF-8 bytes; a consumer refuses a longer value as malformed."
        ),
    });
    json!({
        "name": event_type,
        "description": format!(
            "The NATS publisher sets {NATS_MSG_ID} to the publication ID and reuses it when retrying the same publication."
        ),
        "headers": {
            "type": "object",
            "required": [MESSAGE_ID, EVENT_TYPE, EVENT_SCHEMA, CREATED_AT],
            "properties": {
                MESSAGE_ID: identity,
                EVENT_TYPE: { "const": event_type },
                EVENT_SCHEMA: { "const": schema_label(schema_version) },
                CREATED_AT: {
                    "type": "string",
                    "format": "date-time",
                    "description": CREATED_AT_DESCRIPTION,
                },
            },
        },
        "payload": {
            "schemaFormat": "application/schema+json;version=draft-2020-12",
            "schema": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$ref": payload_schema_reference(payload_schema_name),
                "$defs": definitions,
            },
        },
    })
}

fn payload_schema_reference(name: &str) -> String {
    const FRAGMENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    // RFC 6901 §6
    let token = shared_rules::rfc6901::escape(name);
    format!(
        "#/$defs/{}",
        utf8_percent_encode(&token, FRAGMENT_ENCODE_SET)
    )
}

fn relocate_schema_refs(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if let Some(Value::String(reference)) = object.get_mut("$ref")
        && let Some(name) = reference.strip_prefix("#/components/schemas/")
    {
        *reference = format!("#/$defs/{name}");
    }
    for keyword in [
        "$defs",
        "definitions",
        "properties",
        "patternProperties",
        "dependentSchemas",
    ] {
        if let Some(Value::Object(children)) = object.get_mut(keyword) {
            for child in children.values_mut() {
                relocate_schema_refs(child);
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(Value::Array(children)) = object.get_mut(keyword) {
            for child in children {
                relocate_schema_refs(child);
            }
        }
    }
    for keyword in [
        "items",
        "contains",
        "additionalProperties",
        "unevaluatedProperties",
        "unevaluatedItems",
        "propertyNames",
        "contentSchema",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(child) = object.get_mut(keyword) {
            relocate_schema_refs(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use domain_events::EventPayload;
    use serde_json::json;

    use super::ContractError;
    use crate::{Registry, Route};

    #[derive(utoipa::ToSchema)]
    struct Line {
        #[expect(dead_code, reason = "only the schema is read")]
        sku: String,
    }

    #[derive(utoipa::ToSchema)]
    struct OrderCreated {
        #[expect(dead_code, reason = "only the schema is read")]
        lines: Vec<Line>,
        #[expect(dead_code, reason = "only the schema is read")]
        note: Option<String>,
    }

    impl EventPayload for OrderCreated {
        const EVENT_TYPE: &'static str = "order.created";
        const SCHEMA_VERSION: u16 = 2;
    }

    #[derive(utoipa::ToSchema)]
    struct OrderCancelled {
        #[expect(dead_code, reason = "only the schema is read")]
        reason: String,
    }

    impl EventPayload for OrderCancelled {
        const EVENT_TYPE: &'static str = "order.cancelled";
        const SCHEMA_VERSION: u16 = 1;
    }

    struct CustomSchemaName;

    impl EventPayload for CustomSchemaName {
        const EVENT_TYPE: &'static str = "order.custom";
        const SCHEMA_VERSION: u16 = 1;
    }

    impl utoipa::PartialSchema for CustomSchemaName {
        fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
            <OrderCancelled as utoipa::PartialSchema>::schema()
        }
    }

    impl utoipa::ToSchema for CustomSchemaName {
        fn name() -> std::borrow::Cow<'static, str> {
            "Order%41/Étape~draft-._ +".into()
        }
    }

    #[test]
    fn payload_schema_reference_encodes_the_definition_name_as_a_uri_fragment() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<CustomSchemaName>::documented("orders"))
            .unwrap();
        let document = registry.asyncapi("orders", "1").unwrap();
        let schema =
            &document["channels"]["orders"]["messages"]["order.custom.v1"]["payload"]["schema"];

        assert_eq!(
            schema["$ref"],
            "#/$defs/Order%2541~1%C3%89tape~0draft-._%20%2B"
        );
        assert_eq!(
            schema["$defs"]["Order%41/Étape~draft-._ +"]["type"],
            "object"
        );
    }

    #[test]
    fn document_lists_each_subject_with_its_messages_headers_and_payload_schemas() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<OrderCreated>::documented("orders.events"))
            .unwrap();
        registry
            .declare(&Route::<OrderCancelled>::documented("orders.events"))
            .unwrap();
        let document = registry.asyncapi("orders", "1.4.0").unwrap();

        assert_eq!(document["asyncapi"], "3.0.0");
        assert_eq!(
            document["info"],
            json!({ "title": "orders", "version": "1.4.0" })
        );
        let channel = &document["channels"]["orders.events"];
        assert_eq!(channel["address"], "orders.events");
        let created = &channel["messages"]["order.created.v2"];
        let identity = json!({
            "type": "string",
            "minLength": 1,
            "maxLength": 256,
            "pattern": "^[^\\u0000-\\u001F\\u007F-\\u009F]*$",
            "description": "At most 256 UTF-8 bytes; a consumer refuses a longer value as malformed.",
        });
        assert_eq!(created["name"], "order.created");
        assert_eq!(
            created["headers"],
            json!({
                "type": "object",
                "required": ["Message-Id", "Event-Type", "Event-Schema", "Created-At"],
                "properties": {
                    "Message-Id": identity,
                    "Event-Type": { "const": "order.created" },
                    "Event-Schema": { "const": "v2" },
                    "Created-At": {
                        "type": "string",
                        "format": "date-time",
                        "description": "RFC 3339 time; a consumer refuses the zero time 0001-01-01T00:00:00Z as malformed.",
                    },
                },
            })
        );
        assert_eq!(
            created["payload"],
            json!({
                "schemaFormat": "application/schema+json;version=draft-2020-12",
                "schema": {
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "$ref": "#/$defs/OrderCreated",
                    "$defs": {
                        "Line": {
                            "type": "object",
                            "required": ["sku"],
                            "properties": { "sku": { "type": "string" } },
                        },
                        "OrderCreated": {
                            "type": "object",
                            "required": ["lines"],
                            "properties": {
                                "lines": {
                                    "type": "array",
                                    "items": { "$ref": "#/$defs/Line" },
                                },
                                "note": { "type": ["string", "null"] },
                            },
                        },
                    },
                },
            })
        );
        assert_eq!(
            channel["messages"]["order.cancelled.v1"]["payload"],
            json!({
                "schemaFormat": "application/schema+json;version=draft-2020-12",
                "schema": {
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "$ref": "#/$defs/OrderCancelled",
                    "$defs": {
                        "OrderCancelled": {
                            "type": "object",
                            "required": ["reason"],
                            "properties": { "reason": { "type": "string" } },
                        },
                    },
                },
            })
        );
    }

    #[derive(utoipa::ToSchema)]
    #[expect(dead_code, reason = "only the schema is read")]
    struct TuplePayload {
        pair: (String, bool),
    }

    impl EventPayload for TuplePayload {
        const EVENT_TYPE: &'static str = "tuple.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn tuple_payload_retains_positional_schemas_and_forbids_extra_items() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<TuplePayload>::documented("tuples"))
            .unwrap();
        let document = registry.asyncapi("tuples", "1").unwrap();
        let payload = &document["channels"]["tuples"]["messages"]["tuple.created.v1"]["payload"];
        assert_eq!(
            payload["schemaFormat"],
            "application/schema+json;version=draft-2020-12"
        );
        assert_eq!(
            payload["schema"]["$defs"]["TuplePayload"]["properties"]["pair"],
            json!({
                "type": "array",
                "prefixItems": [{ "type": "string" }, { "type": "boolean" }],
                "items": false,
            })
        );
    }

    #[derive(utoipa::ToSchema)]
    #[expect(dead_code, reason = "only the schema is read")]
    struct Node {
        lines: Vec<Line>,
        #[schema(no_recursion)]
        children: Vec<Node>,
    }

    impl EventPayload for Node {
        const EVENT_TYPE: &'static str = "tree.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn nested_and_recursive_refs_resolve_inside_the_standalone_payload() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<Node>::documented("trees"))
            .unwrap();
        let document = registry.asyncapi("trees", "1").unwrap();
        let schema =
            &document["channels"]["trees"]["messages"]["tree.created.v1"]["payload"]["schema"];
        let root = schema
            .pointer(schema["$ref"].as_str().unwrap().strip_prefix('#').unwrap())
            .unwrap();
        for (property, expected) in [("children", "Node"), ("lines", "Line")] {
            let reference = root["properties"][property]["items"]["$ref"]
                .as_str()
                .unwrap();
            assert_eq!(reference, format!("#/$defs/{expected}"));
            let target = schema
                .pointer(reference.strip_prefix('#').unwrap())
                .unwrap();
            assert_eq!(target["type"], "object");
            assert!(target["properties"].is_object());
        }
    }

    #[derive(utoipa::ToSchema)]
    #[expect(dead_code, reason = "only the schema is read")]
    struct LiteralPayload {
        #[schema(schema_with = literal_metadata_schema)]
        metadata: serde_json::Value,
        #[serde(rename = "$ref")]
        reference: String,
        #[schema(schema_with = external_schema)]
        external: String,
    }

    fn literal_metadata_schema() -> utoipa::openapi::Object {
        utoipa::openapi::ObjectBuilder::new()
            .default(Some(json!({ "$ref": "#/components/schemas/default" })))
            .examples([json!({ "items": { "$ref": "#/components/schemas/example" } })])
            .extensions(Some(
                utoipa::openapi::extensions::ExtensionsBuilder::new()
                    .add(
                        "x-data",
                        json!({ "$ref": "#/components/schemas/extension" }),
                    )
                    .build(),
            ))
            .build()
    }

    fn external_schema() -> utoipa::openapi::Ref {
        utoipa::openapi::Ref::new("https://example.com/payload.json#/$defs/External")
    }

    impl EventPayload for LiteralPayload {
        const EVENT_TYPE: &'static str = "literal.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    #[test]
    fn literal_reference_data_property_names_and_external_refs_are_preserved() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<LiteralPayload>::documented("literals"))
            .unwrap();
        let document = registry.asyncapi("literals", "1").unwrap();
        let properties = &document["channels"]["literals"]["messages"]["literal.created.v1"]["payload"]
            ["schema"]["$defs"]["LiteralPayload"]["properties"];
        assert_eq!(
            properties["metadata"]["default"],
            json!({ "$ref": "#/components/schemas/default" })
        );
        assert_eq!(
            properties["metadata"]["examples"],
            json!([{ "items": { "$ref": "#/components/schemas/example" } }])
        );
        assert_eq!(
            properties["metadata"]["x-data"],
            json!({ "$ref": "#/components/schemas/extension" })
        );
        assert_eq!(properties["$ref"], json!({ "type": "string" }));
        assert_eq!(
            properties["external"]["$ref"],
            "https://example.com/payload.json#/$defs/External"
        );
    }

    #[test]
    fn same_routes_render_the_same_text_in_any_declaration_order() {
        let mut forward = Registry::default();
        forward
            .declare(&Route::<OrderCreated>::documented("orders.created"))
            .unwrap();
        forward
            .declare(&Route::<OrderCancelled>::documented("orders.cancelled"))
            .unwrap();
        let mut backward = Registry::default();
        backward
            .declare(&Route::<OrderCancelled>::documented("orders.cancelled"))
            .unwrap();
        backward
            .declare(&Route::<OrderCreated>::documented("orders.created"))
            .unwrap();
        assert_eq!(
            serde_json::to_string(&forward.asyncapi("orders", "1").unwrap()).unwrap(),
            serde_json::to_string(&backward.asyncapi("orders", "1").unwrap()).unwrap()
        );
    }

    #[test]
    fn a_route_without_a_schema_has_no_document() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<OrderCreated>::documented("orders.created"))
            .unwrap();
        registry
            .declare(&Route::<OrderCancelled>::new("orders.cancelled"))
            .unwrap();
        std::assert_matches!(
            registry.asyncapi("orders", "1"),
            Err(ContractError::Undocumented {
                event_type: "order.cancelled",
                schema_version: 1
            })
        );
    }

    mod other {
        #[derive(utoipa::ToSchema)]
        pub(super) struct Line {
            #[expect(dead_code, reason = "only the schema is read")]
            quantity: u32,
        }

        #[derive(utoipa::ToSchema)]
        pub(super) struct OrderShipped {
            #[expect(dead_code, reason = "only the schema is read")]
            lines: Vec<Line>,
        }

        impl domain_events::EventPayload for OrderShipped {
            const EVENT_TYPE: &'static str = "order.shipped";
            const SCHEMA_VERSION: u16 = 1;
        }
    }

    #[test]
    fn two_different_types_under_one_schema_name_are_refused() {
        let mut registry = Registry::default();
        registry
            .declare(&Route::<OrderCreated>::documented("orders.created"))
            .unwrap();
        registry
            .declare(&Route::<other::OrderShipped>::documented("orders.shipped"))
            .unwrap();
        std::assert_matches!(
            registry.asyncapi("orders", "1"),
            Err(ContractError::SchemaName(name)) if name == "Line"
        );
    }
}
