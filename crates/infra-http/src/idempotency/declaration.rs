//! Generation of an idempotent route's contract and the assembled agreement.
//!
//! [`prepare`] is the only idempotency opt-in: it mutates one documented route
//! carrier before final contract authentication wraps it and it is served.
//! [`agree`] is read-only and checks the resulting document once, after
//! response references can resolve. Final contract authentication owns
//! protected-operation policy acceptance.

use std::collections::BTreeSet;

use serde_json::Value;
use utoipa::openapi::OpenApi;
use utoipa::openapi::path::{Operation, PathItem, Paths};

use super::openapi::{
    KEY_HEADER, RESPONSE_COMPONENTS, key_parameter, replaces_protected_response, response_family,
};

/// The operation extension that declares an idempotent operation.
const EXTENSION: &str = "x-idempotent";
const IDEMPOTENT_METHODS: [&str; 4] = ["post", "put", "patch", "delete"];
const RESPONSE_REFERENCE: &str = "#/components/responses/";
const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";
const PROBLEM_SCHEMA: &str = "#/components/schemas/Problem";
/// The label of a failure that belongs to no single operation.
const DOCUMENT: &str = "document";

/// The route identity retained from the same tuple that receives middleware.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ComposedOperation {
    path: String,
    method: &'static str,
    pub(super) operation_id: String,
}

/// The idempotent operations of the document and the routes composed through
/// [`Composer::route`](super::Composer::route) disagree, or a tuple cannot be
/// prepared. Sanitized: it names an operation and one fixed rule, never
/// request data.
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

/// One preparation or agreement rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rule {
    Shape,
    Undeclared,
    NotTrue,
    Method,
    KeyParameter,
    SuccessResponses,
    ProblemResponses,
    NotComposed,
    UndeclaredKey,
    Components,
    DuplicateOperationId,
    Unreadable,
}

impl Rule {
    const fn text(self) -> &'static str {
        match self {
            Self::Shape => {
                "a composed route must be one path and one operation with an operationId"
            }
            Self::Undeclared => "a composed route must declare generated idempotency metadata",
            Self::NotTrue => "x-idempotent must be the boolean true",
            Self::Method => "the method must be POST, PUT, PATCH, or DELETE",
            Self::KeyParameter => {
                "Idempotency-Key must be absent or one byte-equal generated header parameter"
            }
            Self::SuccessResponses => {
                "declare a 2xx response, no 1xx or 3xx response, and only replayable 2xx headers"
            }
            Self::ProblemResponses => {
                "a generated response slot conflicts with the idempotency response family"
            }
            Self::NotComposed => {
                "an operation declaring x-idempotent: true must be composed as idempotent"
            }
            Self::UndeclaredKey => {
                "an Idempotency-Key header parameter requires x-idempotent: true"
            }
            Self::Components => "the idempotency response components must be registered",
            Self::DuplicateOperationId => "operationId must be unique in the assembled document",
            Self::Unreadable => "the contract cannot be read",
        }
    }
}

/// Generate the idempotency contract for one `routes!` tuple and return its
/// path, method, and `operationId` for the composer to record.
///
/// The tuple has no adopter-facing idempotency annotation. A preexisting
/// `x-idempotent: true`, generated key parameter, or generated response is
/// accepted only when it is the exact generated value, which keeps repeated
/// preparation idempotent without admitting parallel contract ownership.
pub(super) fn prepare(paths: &mut Paths) -> Result<ComposedOperation, AgreementError> {
    let label = route_label(paths);
    if paths.paths.len() != 1 {
        return Err(AgreementError::new(label, Rule::Shape));
    }
    let Some((path, item)) = paths.paths.iter_mut().next() else {
        return Err(AgreementError::new(label, Rule::Shape));
    };
    let Some((method, operation)) = one_operation_mut(item) else {
        return Err(AgreementError::new(label, Rule::Shape));
    };
    let Some(operation_id) = operation.operation_id.clone() else {
        return Err(AgreementError::new(label, Rule::Shape));
    };
    if !IDEMPOTENT_METHODS.contains(&method) {
        return Err(AgreementError::new(operation_id, Rule::Method));
    }
    match operation
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(EXTENSION))
    {
        None | Some(Value::Bool(true)) => {}
        Some(_) => return Err(AgreementError::new(operation_id, Rule::NotTrue)),
    }

    let generated_key = key_parameter();
    let mut existing_keys = operation
        .parameters
        .as_deref()
        .into_iter()
        .flatten()
        .filter(|parameter| {
            parameter.parameter_in == utoipa::openapi::path::ParameterIn::Header
                && parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        });
    let add_key = match (existing_keys.next(), existing_keys.next()) {
        (None, None) => true,
        (Some(existing), None) if existing == &generated_key => false,
        _ => return Err(AgreementError::new(operation_id, Rule::KeyParameter)),
    };

    let family = response_family();
    for (status, generated) in &family {
        match operation.responses.responses.get(status) {
            None => {}
            Some(existing) if existing == generated => {}
            Some(existing) if replaces_protected_response(status, existing) => {}
            Some(_) => return Err(AgreementError::new(operation_id, Rule::ProblemResponses)),
        }
    }

    operation
        .extensions
        .get_or_insert_default()
        .insert(EXTENSION.to_owned(), Value::Bool(true));
    if add_key {
        operation
            .parameters
            .get_or_insert_default()
            .push(generated_key);
    }
    operation.responses.responses.extend(family);
    Ok(ComposedOperation {
        path: path.clone(),
        method,
        operation_id,
    })
}

/// The document-wide agreement: every generated declaration corresponds to a
/// composed tuple and every composed operation is declared; no other operation
/// advertises the key; operation IDs are unique; response references resolve;
/// preserved successes and 403 responses remain compatible; and the family's
/// components are registered.
pub(super) fn agree(
    document: &OpenApi,
    composed: &BTreeSet<ComposedOperation>,
) -> Result<(), AgreementError> {
    let Ok(components) = serde_json::to_value(&document.components) else {
        return Err(AgreementError::new(DOCUMENT, Rule::Unreadable));
    };
    let mut declared = BTreeSet::new();
    let mut operation_ids = BTreeSet::new();
    for (path, item) in &document.paths.paths {
        for (method, operation) in operations(item) {
            let label = operation
                .operation_id
                .clone()
                .unwrap_or_else(|| format!("{} {path}", method.to_ascii_uppercase()));
            if let Some(operation_id) = operation.operation_id.as_deref()
                && !operation_ids.insert(operation_id)
            {
                return Err(AgreementError::new(label, Rule::DuplicateOperationId));
            }
            let declaration = operation
                .extensions
                .as_ref()
                .and_then(|extensions| extensions.get(EXTENSION));
            match declaration {
                None if declares_key(operation) => {
                    return Err(AgreementError::new(label, Rule::UndeclaredKey));
                }
                None => {}
                Some(Value::Bool(true)) => {
                    let Some(operation_id) = operation.operation_id.as_deref() else {
                        return Err(AgreementError::new(label, Rule::Shape));
                    };
                    let identity = ComposedOperation {
                        path: path.clone(),
                        method,
                        operation_id: operation_id.to_owned(),
                    };
                    if !composed.contains(&identity) {
                        return Err(AgreementError::new(label, Rule::NotComposed));
                    }
                    let Ok(operation) = serde_json::to_value(operation) else {
                        return Err(AgreementError::new(label, Rule::Unreadable));
                    };
                    check_responses(&operation, &components)
                        .map_err(|rule| AgreementError::new(label, rule))?;
                    declared.insert(identity);
                }
                Some(_) => return Err(AgreementError::new(label, Rule::NotTrue)),
            }
        }
    }
    if let Some(missing) = composed.difference(&declared).next() {
        return Err(AgreementError::new(&missing.operation_id, Rule::Undeclared));
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

fn one_operation_mut(item: &mut PathItem) -> Option<(&'static str, &mut Operation)> {
    let mut operations = [
        ("get", item.get.as_mut()),
        ("put", item.put.as_mut()),
        ("post", item.post.as_mut()),
        ("delete", item.delete.as_mut()),
        ("options", item.options.as_mut()),
        ("head", item.head.as_mut()),
        ("patch", item.patch.as_mut()),
        ("trace", item.trace.as_mut()),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.map(|operation| (method, operation)));
    let operation = operations.next()?;
    operations.next().is_none().then_some(operation)
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
fn declares_key(operation: &Operation) -> bool {
    operation
        .parameters
        .as_deref()
        .into_iter()
        .flatten()
        .any(|parameter| {
            parameter.parameter_in == utoipa::openapi::path::ParameterIn::Header
                && parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        })
}

fn check_responses(operation: &Value, components: &Value) -> Result<(), Rule> {
    let Some(responses) = operation.get("responses").and_then(Value::as_object) else {
        return Err(Rule::SuccessResponses);
    };
    let mut success = false;
    let mut problem_403 = false;
    for (status, response) in responses {
        let Resolved::Response(response) = resolve(response, components) else {
            return Err(if status.starts_with('2') {
                Rule::SuccessResponses
            } else {
                Rule::ProblemResponses
            });
        };
        match status.as_bytes().first() {
            Some(b'1' | b'3') => return Err(Rule::SuccessResponses),
            Some(b'2') => {
                success = true;
                if !declares_only_replayable_headers(response) {
                    return Err(Rule::SuccessResponses);
                }
            }
            _ => {}
        }
        if status == "403" {
            problem_403 = is_problem_response(response);
        }
    }
    if !success {
        return Err(Rule::SuccessResponses);
    }
    if !problem_403 {
        return Err(Rule::ProblemResponses);
    }
    Ok(())
}

fn declares_only_replayable_headers(response: &Value) -> bool {
    const REPLAYABLE_HEADERS: [&str; 7] = [
        "Content-Type",
        "Content-Encoding",
        "Content-Language",
        "Content-Disposition",
        "Location",
        "ETag",
        "Last-Modified",
    ];
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
    /// A reference to no component of the assembled document.
    Dangling,
}

fn resolve<'a>(mut response: &'a Value, components: &'a Value) -> Resolved<'a> {
    let registered = components.get("responses").and_then(Value::as_object);
    // More hops than registered components means a reference cycle.
    for _ in 0..=registered.map_or(0, serde_json::Map::len) {
        let Some(reference) = response.get("$ref") else {
            return Resolved::Response(response);
        };
        let Some(target) = reference
            .as_str()
            .and_then(|reference| reference.strip_prefix(RESPONSE_REFERENCE))
            .and_then(|name| registered?.get(name))
        else {
            return Resolved::Dangling;
        };
        response = target;
    }
    Resolved::Dangling
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use utoipa::OpenApi as _;
    use utoipa::openapi::response::Response;
    use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};
    use utoipa_axum::routes;

    use super::*;
    use crate::idempotency::openapi::{IdempotencyComponents, key_parameter, response_family};
    use crate::problem::responses::{ProblemComponents, ProtectedOperationProblemResponses};

    const WIDGETS: &str = "/_test/widgets";
    const CREATE_WIDGET: &str = "infraHttpTestCreateWidget";

    #[utoipa::path(
        post,
        path = "/_test/widgets",
        operation_id = "infraHttpTestCreateWidget",
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only operation with composer-generated idempotency metadata"
        }))),
        responses(
            (
                status = 201,
                description = "created",
                content_type = "text/plain",
                body = String,
                headers(("Location" = String, description = "the created widget"))
            ),
            ProtectedOperationProblemResponses,
        )
    )]
    async fn create_widget() -> &'static str {
        "created"
    }

    #[utoipa::path(
        get,
        path = "/_test/widgets",
        operation_id = "infraHttpTestGetWidget",
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only operation with an unsupported idempotency method"
        }))),
        responses((status = 200, description = "found"), ProtectedOperationProblemResponses)
    )]
    async fn get_widget() -> &'static str {
        "found"
    }

    fn prepared_routes() -> (UtoipaMethodRouter, ComposedOperation) {
        #[expect(
            clippy::disallowed_methods,
            reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
        )]
        let mut routes: UtoipaMethodRouter = routes!(create_widget);
        let identity = prepare(&mut routes.1).expect("normal protected route prepares");
        (routes, identity)
    }

    fn document(routes: UtoipaMethodRouter) -> OpenApi {
        OpenApiRouter::with_openapi(ProblemComponents::openapi())
            .merge(OpenApiRouter::with_openapi(IdempotencyComponents::openapi()))
            .routes(routes)
            .into_openapi()
    }

    fn prepared_document() -> (OpenApi, BTreeSet<ComposedOperation>) {
        let (routes, identity) = prepared_routes();
        (document(routes), BTreeSet::from([identity]))
    }

    #[test]
    fn preparation_generates_metadata_key_and_family_from_a_normal_protected_tuple() {
        let (document, composed) = prepared_document();
        let operation = serde_json::to_value(
            document.paths.paths[WIDGETS]
                .post
                .as_ref()
                .expect("prepared post operation"),
        )
        .expect("operation serializes");
        assert_eq!(operation[EXTENSION], true);
        assert_eq!(operation["parameters"].as_array().map(Vec::len), Some(1));
        assert_eq!(operation["parameters"][0]["name"], KEY_HEADER);
        assert!(
            operation["parameters"][0]["schema"]
                .get("pattern")
                .is_none()
        );
        assert!(
            operation["parameters"][0]["schema"]
                .get("maxLength")
                .is_none()
        );
        for status in ["400", "409", "413", "422", "500", "503"] {
            assert!(operation["responses"].get(status).is_some(), "{status}");
        }
        assert_eq!(agree(&document, &composed), Ok(()));
    }

    #[test]
    fn preparation_accepts_only_matching_generated_values_and_rejects_conflicts() {
        type ConflictCase = (fn(&mut Operation), Rule);

        #[expect(
            clippy::disallowed_methods,
            reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
        )]
        let routes: UtoipaMethodRouter = routes!(create_widget);
        let (_, mut idempotent, _) = routes;
        let operation = idempotent
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("post operation");
        operation
            .extensions
            .get_or_insert_default()
            .insert(EXTENSION.to_owned(), json!(true));
        operation.parameters = Some(vec![key_parameter()]);
        operation.responses.responses.extend(response_family());
        assert_eq!(
            prepare(&mut idempotent),
            Ok(ComposedOperation {
                path: WIDGETS.to_owned(),
                method: "post",
                operation_id: CREATE_WIDGET.to_owned(),
            })
        );

        let cases: [ConflictCase; 3] = [
            (
                |operation| {
                    operation
                        .extensions
                        .get_or_insert_default()
                        .insert(EXTENSION.to_owned(), json!(false));
                },
                Rule::NotTrue,
            ),
            (
                |operation| {
                    let mut parameter = key_parameter();
                    parameter.description = Some("different key contract".to_owned());
                    operation.parameters = Some(vec![parameter]);
                },
                Rule::KeyParameter,
            ),
            (
                |operation| {
                    operation
                        .responses
                        .responses
                        .insert("409".to_owned(), Response::new("business conflict").into());
                },
                Rule::ProblemResponses,
            ),
        ];
        for (change, rule) in cases {
            #[expect(
                clippy::disallowed_methods,
                reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
            )]
            let routes: UtoipaMethodRouter = routes!(create_widget);
            let (_, mut paths, _) = routes;
            change(
                paths
                    .paths
                    .get_mut(WIDGETS)
                    .expect("widget path")
                    .post
                    .as_mut()
                    .expect("post operation"),
            );
            assert_eq!(prepare(&mut paths).unwrap_err().rule(), rule);
        }
    }

    #[test]
    fn preparation_requires_one_supported_operation_with_an_operation_id() {
        #[expect(
            clippy::disallowed_methods,
            reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
        )]
        let routes: UtoipaMethodRouter = routes!(get_widget);
        let (_, mut unsupported, _) = routes;
        assert_eq!(prepare(&mut unsupported).unwrap_err().rule(), Rule::Method);

        #[expect(
            clippy::disallowed_methods,
            reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
        )]
        let routes: UtoipaMethodRouter = routes!(create_widget, get_widget);
        let (_, mut multiple, _) = routes;
        assert_eq!(prepare(&mut multiple).unwrap_err().rule(), Rule::Shape);

        #[expect(
            clippy::disallowed_methods,
            reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
        )]
        let routes: UtoipaMethodRouter = routes!(create_widget);
        let (_, mut unnamed, _) = routes;
        unnamed
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("post operation")
            .operation_id = None;
        assert_eq!(prepare(&mut unnamed).unwrap_err().rule(), Rule::Shape);
    }

    #[test]
    fn assembled_agreement_rejects_only_unresolved_or_incompatible_document_facts() {
        let (document, composed) = prepared_document();
        assert_eq!(agree(&document, &composed), Ok(()));

        let mut custom_403 = document.clone();
        custom_403
            .paths
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("post operation")
            .responses
            .responses
            .insert(
                "403".to_owned(),
                Response::new("not a Problem response").into(),
            );
        assert_eq!(
            agree(&custom_403, &composed).unwrap_err().rule(),
            Rule::ProblemResponses
        );

        let mut unreplayable_success = document.clone();
        let response = unreplayable_success
            .paths
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("post operation")
            .responses
            .responses
            .get_mut("201")
            .expect("created response");
        let utoipa::openapi::RefOr::T(response) = response else {
            panic!("inline created response")
        };
        response.headers.insert(
            "Set-Cookie".to_owned(),
            utoipa::openapi::header::Header::default(),
        );
        assert_eq!(
            agree(&unreplayable_success, &composed).unwrap_err().rule(),
            Rule::SuccessResponses
        );

        let mut uncomposed = document;
        uncomposed
            .paths
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("post operation")
            .operation_id = Some("infraHttpTestUncomposed".to_owned());
        assert_eq!(
            agree(&uncomposed, &composed).unwrap_err().rule(),
            Rule::NotComposed
        );
    }

    #[test]
    fn assembled_agreement_rejects_path_or_method_changes_with_the_same_operation_id() {
        let (document, composed) = prepared_document();
        assert_eq!(agree(&document, &composed), Ok(()));

        let mut moved = document.clone();
        let route = moved.paths.paths.remove(WIDGETS).expect("widget path");
        moved.paths.paths.insert("/_test/moved".to_owned(), route);
        assert_eq!(
            agree(&moved, &composed).unwrap_err().rule(),
            Rule::NotComposed
        );

        let mut changed_method = document;
        let route = changed_method
            .paths
            .paths
            .get_mut(WIDGETS)
            .expect("widget path");
        route.put = route.post.take();
        assert_eq!(
            agree(&changed_method, &composed).unwrap_err().rule(),
            Rule::NotComposed
        );
    }

    #[test]
    fn assembled_agreement_resolves_shared_and_preserved_response_references() {
        use utoipa::openapi::Ref;

        let (document, composed) = prepared_document();
        for component in ["RequestEntityTooLarge", "InternalServerError"] {
            let mut dangling = document.clone();
            dangling
                .components
                .as_mut()
                .expect("components")
                .responses
                .remove(component);
            assert_eq!(
                agree(&dangling, &composed).unwrap_err().rule(),
                Rule::ProblemResponses,
                "{component}"
            );
        }

        let mut custom = document;
        let responses = &mut custom
            .paths
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("post operation")
            .responses
            .responses;
        let forbidden = responses
            .remove("403")
            .expect("protected forbidden response");
        responses.insert(
            "403".to_owned(),
            Ref::from_response_name("CustomForbidden").into(),
        );
        custom
            .components
            .as_mut()
            .expect("components")
            .responses
            .insert("CustomForbidden".to_owned(), forbidden);
        assert_eq!(agree(&custom, &composed), Ok(()));

        for reference in [
            "#/components/responses/Missing",
            "#/components/schemas/Problem",
            "#/components/responses/CustomForbidden",
        ] {
            custom
                .components
                .as_mut()
                .expect("components")
                .responses
                .insert("CustomForbidden".to_owned(), Ref::new(reference).into());
            assert_eq!(
                agree(&custom, &composed).unwrap_err().rule(),
                Rule::ProblemResponses,
                "{reference}"
            );
        }
    }
}
