//! The declaration rules of an idempotent operation and the two-way
//! agreement between the document and the composed routes.
//!
//! [`check_route`] checks one tuple before it is composed; [`agree`]
//! re-checks every idempotent operation on the assembled document, where
//! response references resolve, and adds the document-wide rules. Rule 3,
//! the protected-operation contract, is `protect`'s own acceptance of the
//! tuple and is not repeated here. Operations are read in their generated
//! JSON form, as the committed contract renders them.

use std::collections::BTreeSet;

use serde_json::Value;
use utoipa::IntoResponses as _;
use utoipa::openapi::OpenApi;
use utoipa::openapi::path::{Operation, PathItem, Paths};

use super::openapi::{
    AUTHORIZATION_STATUS, FIXED_PROBLEM_STATUSES, IdempotentOperationProblemResponses, KEY_HEADER,
    KEY_MAX_LENGTH, KEY_MIN_LENGTH, KEY_PATTERN, REPLAYABLE_HEADERS, RESPONSE_COMPONENTS,
};

/// The operation extension that declares an idempotent operation.
const EXTENSION: &str = "x-idempotent";
const IDEMPOTENT_METHODS: [&str; 4] = ["post", "put", "patch", "delete"];
const RESPONSE_REFERENCE: &str = "#/components/responses/";
const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";
const PROBLEM_SCHEMA: &str = "#/components/schemas/Problem";
/// The label of a failure that belongs to no single operation.
const DOCUMENT: &str = "document";

/// The idempotent operations of the document and the routes composed
/// through [`Composer::route`](super::Composer::route) disagree, or an
/// operation breaks a declaration rule. Sanitized: it names the operation
/// and one fixed rule, never request data.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("idempotent operation contract is invalid: {operation}: {}", .rule.text())]
pub struct AgreementError {
    operation: String,
    rule: Rule,
}

impl AgreementError {
    pub(super) fn new(operation: impl Into<String>, rule: Rule) -> Self {
        Self {
            operation: operation.into(),
            rule,
        }
    }

    #[cfg(test)]
    pub(super) const fn rule(&self) -> Rule {
        self.rule
    }
}

/// One declaration or agreement rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rule {
    Shape,
    Undeclared,
    NotTrue,
    Method,
    Protected,
    KeyParameter,
    SuccessResponses,
    ProblemResponses,
    NotComposed,
    UndeclaredKey,
    Components,
    Unreadable,
}

impl Rule {
    const fn text(self) -> &'static str {
        match self {
            Self::Shape => {
                "a composed route must be one path and one operation with an operationId"
            }
            Self::Undeclared => "a route composed as idempotent must declare x-idempotent: true",
            Self::NotTrue => "x-idempotent must be the boolean true",
            Self::Method => "the method must be POST, PUT, PATCH, or DELETE",
            Self::Protected => "the operation must meet the protected-operation contract",
            Self::KeyParameter => {
                "Idempotency-Key must be one required header parameter with the exact schema"
            }
            Self::SuccessResponses => {
                "declare a 2xx response, no 1xx or 3xx response, and only replayable 2xx headers"
            }
            Self::ProblemResponses => {
                "the operation must declare the idempotent operation problem responses"
            }
            Self::NotComposed => {
                "an operation declaring x-idempotent: true must be composed as idempotent"
            }
            Self::UndeclaredKey => {
                "an Idempotency-Key header parameter requires x-idempotent: true"
            }
            Self::Components => "the idempotency response components must be registered",
            Self::Unreadable => "the contract cannot be read",
        }
    }
}

/// Check one `routes!` tuple before it is composed and return its
/// `operationId`. Response references cannot resolve without the document,
/// so [`agree`] checks what they name.
pub(super) fn check_route(paths: &Paths) -> Result<String, AgreementError> {
    let mut routed = paths.paths.values().flat_map(operations);
    let (Some((method, operation)), None) = (routed.next(), routed.next()) else {
        return Err(AgreementError::new(route_label(paths), Rule::Shape));
    };
    let Some(operation_id) = operation.operation_id.clone() else {
        return Err(AgreementError::new(route_label(paths), Rule::Shape));
    };
    let Ok(operation) = serde_json::to_value(operation) else {
        return Err(AgreementError::new(operation_id, Rule::Unreadable));
    };
    let checked = match operation.get(EXTENSION) {
        Some(Value::Bool(true)) => check_operation(method, &operation, None),
        None => Err(Rule::Undeclared),
        Some(_) => Err(Rule::NotTrue),
    };
    match checked {
        Ok(()) => Ok(operation_id),
        Err(rule) => Err(AgreementError::new(operation_id, rule)),
    }
}

/// The document-wide agreement: every operation declaring `x-idempotent`
/// declares `true`, is composed, and meets the rules with its references
/// resolved; every composed operation is declared; no other operation
/// declares the key; and the family's components are registered.
pub(super) fn agree(document: &OpenApi, composed: &BTreeSet<String>) -> Result<(), AgreementError> {
    let Ok(components) = serde_json::to_value(&document.components) else {
        return Err(AgreementError::new(DOCUMENT, Rule::Unreadable));
    };
    let mut declared = BTreeSet::new();
    for (path, item) in &document.paths.paths {
        for (method, operation) in operations(item) {
            let label = operation
                .operation_id
                .clone()
                .unwrap_or_else(|| format!("{} {path}", method.to_ascii_uppercase()));
            let Ok(operation) = serde_json::to_value(operation) else {
                return Err(AgreementError::new(label, Rule::Unreadable));
            };
            let checked = match operation.get(EXTENSION) {
                None if key_parameters(&operation).next().is_some() => Err(Rule::UndeclaredKey),
                None => continue,
                Some(Value::Bool(true)) if composed.contains(&label) => {
                    check_operation(method, &operation, Some(&components))
                }
                Some(Value::Bool(true)) => Err(Rule::NotComposed),
                Some(_) => Err(Rule::NotTrue),
            };
            match checked {
                Ok(()) => {
                    declared.insert(label);
                }
                Err(rule) => return Err(AgreementError::new(label, rule)),
            }
        }
    }
    if let Some(missing) = composed.difference(&declared).next() {
        return Err(AgreementError::new(missing.clone(), Rule::Undeclared));
    }
    let registered = components.get("responses");
    if RESPONSE_COMPONENTS.iter().any(|name| {
        registered
            .and_then(|responses| responses.get(*name))
            .is_none()
    }) {
        return Err(AgreementError::new(DOCUMENT, Rule::Components));
    }
    Ok(())
}

/// Rules 2, 4, 5, and 6 on one operation's JSON form. `components` is the
/// document's when references can resolve; a tuple checked before
/// composition passes `None`, and its references wait for [`agree`].
fn check_operation(
    method: &str,
    operation: &Value,
    components: Option<&Value>,
) -> Result<(), Rule> {
    if !IDEMPOTENT_METHODS.contains(&method) {
        return Err(Rule::Method);
    }
    if !declares_key(operation) {
        return Err(Rule::KeyParameter);
    }
    if !declares_successes(operation, components) {
        return Err(Rule::SuccessResponses);
    }
    if !declares_problems(operation, components) {
        return Err(Rule::ProblemResponses);
    }
    Ok(())
}

fn operations(item: &PathItem) -> impl Iterator<Item = (&'static str, &Operation)> {
    [
        ("get", item.get.as_ref()),
        ("put", item.put.as_ref()),
        ("post", item.post.as_ref()),
        ("delete", item.delete.as_ref()),
        ("options", item.options.as_ref()),
        ("head", item.head.as_ref()),
        ("patch", item.patch.as_ref()),
        ("trace", item.trace.as_ref()),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.map(|operation| (method, operation)))
}

fn route_label(paths: &Paths) -> String {
    paths
        .paths
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "route".to_owned())
}

/// Header parameters named `Idempotency-Key`, compared case-insensitively.
fn key_parameters(operation: &Value) -> impl Iterator<Item = &Value> {
    operation
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|parameter| {
            parameter.get("in").and_then(Value::as_str) == Some("header")
                && parameter
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case(KEY_HEADER))
        })
}

fn declares_key(operation: &Value) -> bool {
    let mut keys = key_parameters(operation);
    let (Some(key), None) = (keys.next(), keys.next()) else {
        return false;
    };
    let schema = &key["schema"];
    key.get("required") == Some(&Value::Bool(true))
        && schema.get("type").and_then(Value::as_str) == Some("string")
        && schema.get("minLength").and_then(Value::as_u64) == Some(KEY_MIN_LENGTH)
        && schema.get("maxLength").and_then(Value::as_u64) == Some(KEY_MAX_LENGTH)
        && schema.get("pattern").and_then(Value::as_str) == Some(KEY_PATTERN)
}

fn declares_successes(operation: &Value, components: Option<&Value>) -> bool {
    let Some(responses) = operation.get("responses").and_then(Value::as_object) else {
        return false;
    };
    let mut success = false;
    for (status, response) in responses {
        match status.as_bytes().first() {
            Some(b'1' | b'3') => return false,
            Some(b'2') => {
                success = true;
                let replayable = match resolve(response, components) {
                    Resolved::Response(response) => declares_only_replayable_headers(response),
                    Resolved::Deferred => true,
                    Resolved::Dangling => false,
                };
                if !replayable {
                    return false;
                }
            }
            _ => {}
        }
    }
    success
}

fn declares_only_replayable_headers(response: &Value) -> bool {
    match response.get("headers") {
        None => true,
        Some(headers) => headers.as_object().is_some_and(|headers| {
            headers.keys().all(|name| {
                REPLAYABLE_HEADERS
                    .iter()
                    .any(|replayable| replayable.eq_ignore_ascii_case(name))
            })
        }),
    }
}

fn declares_problems(operation: &Value, components: Option<&Value>) -> bool {
    let Ok(expected) = serde_json::to_value(IdempotentOperationProblemResponses::responses())
    else {
        return false;
    };
    let responses = &operation["responses"];
    let fixed = FIXED_PROBLEM_STATUSES.iter().all(|status| {
        responses
            .get(*status)
            .is_some_and(|declared| expected.get(*status) == Some(declared))
    });
    fixed
        && responses.get(AUTHORIZATION_STATUS).is_some_and(|declared| {
            match resolve(declared, components) {
                Resolved::Response(response) => is_problem_response(response),
                Resolved::Deferred => true,
                Resolved::Dangling => false,
            }
        })
}

fn is_problem_response(response: &Value) -> bool {
    response
        .get("content")
        .and_then(|content| content.get(PROBLEM_MEDIA_TYPE))
        .and_then(|media| media.get("schema"))
        .and_then(|schema| schema.get("$ref"))
        .and_then(Value::as_str)
        == Some(PROBLEM_SCHEMA)
}

enum Resolved<'a> {
    Response(&'a Value),
    /// A component reference in a tuple checked without its document.
    Deferred,
    /// A reference to no component of the document.
    Dangling,
}

fn resolve<'a>(response: &'a Value, components: Option<&'a Value>) -> Resolved<'a> {
    let Some(reference) = response.get("$ref") else {
        return Resolved::Response(response);
    };
    let Some(components) = components else {
        return Resolved::Deferred;
    };
    reference
        .as_str()
        .and_then(|reference| reference.strip_prefix(RESPONSE_REFERENCE))
        .and_then(|name| components.get("responses")?.get(name))
        .map_or(Resolved::Dangling, Resolved::Response)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use utoipa::OpenApi as _;
    use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};
    use utoipa_axum::routes;

    use super::*;
    use crate::idempotency::IdempotencyKey;
    use crate::idempotency::openapi::IdempotencyComponents;
    use crate::problem::responses::ProblemComponents;

    const WIDGETS: &str = "/_test/widgets";
    const CREATE_WIDGET: &str = "infraHttpTestCreateWidget";

    #[utoipa::path(
        post,
        path = "/_test/widgets",
        operation_id = "infraHttpTestCreateWidget",
        params(IdempotencyKey),
        security(("bearerAuth" = [])),
        extensions(
            ("x-security-decision" = json!({
                "exposure": "protected",
                "rationale": "test-only idempotent operation"
            })),
            ("x-idempotent" = json!(true))
        ),
        responses(
            (
                status = 201,
                description = "created",
                content_type = "text/plain",
                body = String,
                headers(("Location" = String, description = "the created widget"))
            ),
            IdempotentOperationProblemResponses,
        )
    )]
    async fn create_widget() -> &'static str {
        "created"
    }

    #[utoipa::path(
        put,
        path = "/_test/widgets",
        operation_id = "infraHttpTestReplaceWidget",
        params(IdempotencyKey),
        security(("bearerAuth" = [])),
        extensions(("x-idempotent" = json!(true))),
        responses((status = 200, description = "replaced"), IdempotentOperationProblemResponses)
    )]
    async fn replace_widget() -> &'static str {
        "replaced"
    }

    #[utoipa::path(
        post,
        path = "/_test/declared-string",
        operation_id = "infraHttpTestDeclaredString",
        params(IdempotencyKey),
        security(("bearerAuth" = [])),
        extensions(("x-idempotent" = json!("true"))),
        responses((status = 201, description = "created"), IdempotentOperationProblemResponses)
    )]
    async fn declared_string() -> &'static str {
        "created"
    }

    #[utoipa::path(
        post,
        path = "/_test/keyed",
        operation_id = "infraHttpTestKeyed",
        params(IdempotencyKey),
        security(),
        extensions(("x-security-decision" = json!({
            "exposure": "public",
            "rationale": "test-only operation that must not advertise a key it ignores"
        }))),
        responses((status = 204, description = "done"))
    )]
    async fn keyed() -> &'static str {
        "done"
    }

    fn document(routes: UtoipaMethodRouter) -> OpenApi {
        OpenApiRouter::with_openapi(ProblemComponents::openapi())
            .merge(OpenApiRouter::with_openapi(IdempotencyComponents::openapi()))
            .routes(routes)
            .into_openapi()
    }

    fn composed(operations: &[&str]) -> BTreeSet<String> {
        operations
            .iter()
            .map(|operation| (*operation).to_owned())
            .collect()
    }

    fn widget_operation() -> Value {
        let (_, paths, _): UtoipaMethodRouter = routes!(create_widget);
        serde_json::to_value(paths.paths[WIDGETS].post.as_ref().unwrap()).unwrap()
    }

    fn widget_components() -> Value {
        serde_json::to_value(document(routes!(create_widget)).components).unwrap()
    }

    fn widget_with(change: impl FnOnce(&mut Value)) -> Value {
        let mut operation = widget_operation();
        change(&mut operation);
        operation
    }

    #[test]
    fn a_generated_idempotent_operation_meets_every_rule() {
        let (_, paths, _): UtoipaMethodRouter = routes!(create_widget);
        assert_eq!(check_route(&paths), Ok(CREATE_WIDGET.to_owned()));
        assert_eq!(
            agree(
                &document(routes!(create_widget)),
                &composed(&[CREATE_WIDGET])
            ),
            Ok(())
        );
        let components = widget_components();
        assert_eq!(
            check_operation("post", &widget_operation(), Some(&components)),
            Ok(())
        );
    }

    #[test]
    fn only_post_put_patch_and_delete_can_be_idempotent() {
        let operation = widget_operation();
        for method in ["post", "put", "patch", "delete"] {
            assert_eq!(
                check_operation(method, &operation, None),
                Ok(()),
                "{method}"
            );
        }
        for method in ["get", "head", "options", "trace"] {
            assert_eq!(
                check_operation(method, &operation, None),
                Err(Rule::Method),
                "{method}"
            );
        }
    }

    #[test]
    fn the_key_is_one_required_header_with_the_exact_schema() {
        let lowercase = widget_with(|operation| {
            operation["parameters"][0]["name"] = json!("idempotency-key");
        });
        assert_eq!(check_operation("post", &lowercase, None), Ok(()));
        let violations = [
            widget_with(|operation| operation["parameters"] = json!([])),
            widget_with(|operation| operation["parameters"][0]["required"] = json!(false)),
            widget_with(|operation| operation["parameters"][0]["in"] = json!("query")),
            widget_with(|operation| {
                operation["parameters"][0]["schema"]["type"] = json!("integer");
            }),
            widget_with(|operation| operation["parameters"][0]["schema"]["minLength"] = json!(0)),
            widget_with(|operation| operation["parameters"][0]["schema"]["maxLength"] = json!(256)),
            widget_with(|operation| {
                operation["parameters"][0]["schema"]["pattern"] = json!("^[A-Za-z0-9]+$");
            }),
            widget_with(|operation| {
                let mut repeated = operation["parameters"][0].clone();
                repeated["name"] = json!("IDEMPOTENCY-KEY");
                operation["parameters"]
                    .as_array_mut()
                    .unwrap()
                    .push(repeated);
            }),
        ];
        for violation in violations {
            assert_eq!(
                check_operation("post", &violation, None),
                Err(Rule::KeyParameter),
                "{violation}"
            );
        }
    }

    #[test]
    fn successes_are_2xx_only_with_replayable_headers() {
        let components = widget_components();
        let replayable = widget_with(|operation| {
            operation["responses"]["201"]["headers"]["content-language"] = json!({});
        });
        assert_eq!(
            check_operation("post", &replayable, Some(&components)),
            Ok(())
        );
        let referenced = widget_with(|operation| {
            operation["responses"]["201"] = json!({"$ref": "#/components/responses/Widget"});
        });
        assert_eq!(check_operation("post", &referenced, None), Ok(()));
        assert_eq!(
            check_operation("post", &referenced, Some(&components)),
            Err(Rule::SuccessResponses)
        );
        let violations = [
            widget_with(|operation| {
                operation["responses"]
                    .as_object_mut()
                    .unwrap()
                    .remove("201");
            }),
            widget_with(|operation| {
                operation["responses"]["101"] = json!({"description": "switching"});
            }),
            widget_with(|operation| {
                operation["responses"]["303"] = json!({"description": "see other"});
            }),
            widget_with(|operation| operation["responses"]["201"]["headers"]["ETag"] = json!({})),
        ];
        for violation in violations {
            assert_eq!(
                check_operation("post", &violation, Some(&components)),
                Err(Rule::SuccessResponses),
                "{violation}"
            );
        }
    }

    #[test]
    fn problem_responses_are_the_family_with_a_replaceable_403() {
        let components = widget_components();
        let own_authorization = widget_with(|operation| {
            operation["responses"]["403"] = json!({
                "description": "the caller may not create widgets",
                "content": {"application/problem+json": {
                    "schema": {"$ref": "#/components/schemas/Problem"}
                }}
            });
        });
        assert_eq!(
            check_operation("post", &own_authorization, Some(&components)),
            Ok(())
        );
        let dangling = widget_with(|operation| {
            operation["responses"]["403"] = json!({"$ref": "#/components/responses/Missing"});
        });
        assert_eq!(check_operation("post", &dangling, None), Ok(()));
        let violations = [
            dangling,
            widget_with(|operation| {
                operation["responses"]["403"] = json!({
                    "description": "no",
                    "content": {"text/plain": {"schema": {"type": "string"}}}
                });
            }),
            widget_with(|operation| {
                operation["responses"]
                    .as_object_mut()
                    .unwrap()
                    .remove("403");
            }),
            widget_with(|operation| {
                operation["responses"]["409"] = json!({"description": "conflict"});
            }),
            widget_with(|operation| {
                operation["responses"]["503"] =
                    json!({"$ref": "#/components/responses/AuthenticationUnavailable"});
            }),
            widget_with(|operation| {
                operation["responses"]
                    .as_object_mut()
                    .unwrap()
                    .remove("504");
            }),
        ];
        for violation in violations {
            assert_eq!(
                check_operation("post", &violation, Some(&components)),
                Err(Rule::ProblemResponses),
                "{violation}"
            );
        }
    }

    #[test]
    fn a_composed_tuple_is_one_operation_declaring_the_boolean_true() {
        let (_, paths, _): UtoipaMethodRouter = routes!(declared_string);
        assert_eq!(check_route(&paths).unwrap_err().rule(), Rule::NotTrue);
        let (_, paths, _): UtoipaMethodRouter = routes!(keyed);
        assert_eq!(check_route(&paths).unwrap_err().rule(), Rule::Undeclared);
        let (_, paths, _): UtoipaMethodRouter = routes!(create_widget, replace_widget);
        assert_eq!(check_route(&paths).unwrap_err().rule(), Rule::Shape);
    }

    #[test]
    fn agreement_is_two_way() {
        let error = agree(&document(routes!(create_widget)), &BTreeSet::new()).unwrap_err();
        assert_eq!(error, AgreementError::new(CREATE_WIDGET, Rule::NotComposed));

        let error = agree(
            &document(routes!(create_widget)),
            &composed(&[CREATE_WIDGET, "infraHttpTestGhost"]),
        )
        .unwrap_err();
        assert_eq!(
            error,
            AgreementError::new("infraHttpTestGhost", Rule::Undeclared)
        );

        let error = agree(&document(routes!(keyed)), &BTreeSet::new()).unwrap_err();
        assert_eq!(
            error,
            AgreementError::new("infraHttpTestKeyed", Rule::UndeclaredKey)
        );

        let mut not_true = document(routes!(create_widget));
        not_true
            .paths
            .paths
            .get_mut(WIDGETS)
            .and_then(|item| item.post.as_mut())
            .and_then(|operation| operation.extensions.as_mut())
            .unwrap()
            .insert(EXTENSION.to_owned(), json!(1));
        let error = agree(&not_true, &composed(&[CREATE_WIDGET])).unwrap_err();
        assert_eq!(error, AgreementError::new(CREATE_WIDGET, Rule::NotTrue));
    }

    #[test]
    fn the_family_components_must_be_registered() {
        let empty = OpenApiRouter::<()>::with_openapi(ProblemComponents::openapi()).into_openapi();
        let error = agree(&empty, &BTreeSet::new()).unwrap_err();
        assert_eq!(error, AgreementError::new(DOCUMENT, Rule::Components));
        let registered = OpenApiRouter::<()>::with_openapi(ProblemComponents::openapi())
            .merge(OpenApiRouter::with_openapi(IdempotencyComponents::openapi()))
            .into_openapi();
        assert_eq!(agree(&registered, &BTreeSet::new()), Ok(()));
    }

    #[test]
    fn the_error_names_the_operation_and_one_static_rule() {
        assert_eq!(
            AgreementError::new("createWidget", Rule::Method).to_string(),
            "idempotent operation contract is invalid: createWidget: \
             the method must be POST, PUT, PATCH, or DELETE"
        );
    }
}
