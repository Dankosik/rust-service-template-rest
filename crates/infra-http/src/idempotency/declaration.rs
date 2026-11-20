use std::collections::BTreeMap;

use axum::http::HeaderName;
use axum::http::header::CONTENT_TYPE;
use axum::routing::MethodFilter;
use itertools::Itertools as _;
use utoipa::openapi::path::{Operation, Parameter, ParameterIn, Paths};
use utoipa::openapi::response::Response;
use utoipa::openapi::{Content, RefOr, Schema};

use crate::contract::{ComponentKind, path_operations_mut, same_definition};

use super::openapi::{
    can_replace_with_idempotency_response, key_parameter, replay_header, response_family,
};
use super::stored;
use super::{KEY_HEADER, REPLAYED_HEADER};

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("idempotent route composition is invalid: {subject}: {rule}")]
pub struct CompositionError {
    subject: String,
    rule: Rule,
}

impl CompositionError {
    fn new(subject: &str, rule: Rule) -> Self {
        Self {
            subject: subject.to_owned(),
            rule,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
enum Rule {
    #[error("a composed route must be one path and one operation with a nonempty operationId")]
    Shape,
    #[error("the method must be POST, PUT, PATCH, or DELETE")]
    Method,
    #[error(
        "publish the operation's retry policy at an absolute HTTP(S) URI using externalDocs.url or the route's docs argument"
    )]
    RetryDocumentation,
    #[error("Idempotency-Key must be absent or one byte-equal generated header parameter")]
    KeyParameter,
    #[error(
        "request parameters must be inline and header parameters must have valid HTTP field names"
    )]
    FingerprintParameter,
    #[error("cookie parameter {0} is outside the idempotent request fingerprint")]
    CookieParameter(String),
    #[error("an idempotent operation must not declare informational or redirect response {status}")]
    InformationalOrRedirectResponse { status: String },
    #[error("an idempotent operation must declare a 2xx response")]
    NoSuccess,
    #[error("response {status} conflicts with the command's success contract")]
    SuccessConflict { status: String },
    #[error("response {status} may declare only replayable headers: {name} cannot be replayed")]
    UnreplayableHeader { status: String, name: String },
    #[error("response {status} must be inline so its replay metadata can be checked")]
    ReferencedSuccess { status: String },
    #[error("response {status} conflicts with the idempotency response family")]
    ProblemResponseConflict { status: String },
    #[error(
        "response {status}: Idempotent-Replayed is reserved for exact generated replay metadata"
    )]
    ReplayHeader { status: String },
}

#[derive(Debug)]
pub(super) struct PreparedOperation {
    pub(super) id: String,
    pub(super) method: MethodFilter,
    pub(super) fingerprint_headers: Vec<HeaderName>,
    pub(super) rules_link: axum::http::HeaderValue,
}

pub(super) fn prepare(
    document: &mut utoipa::openapi::OpenApi,
    successes: BTreeMap<String, RefOr<Response>>,
    success_schemas: Vec<(String, RefOr<Schema>)>,
) -> Result<PreparedOperation, crate::FinalizeError> {
    let paths = &mut document.paths;
    let route_docs = document.external_docs.take();
    let subject = route_subject(paths);
    let shape = || CompositionError::new(&subject, Rule::Shape);
    let Ok(item) = paths.paths.values_mut().exactly_one() else {
        return Err(shape().into());
    };
    if item.ref_path.is_some() {
        return Err(shape().into());
    }
    let mut fingerprint_headers = Vec::new();
    collect_fingerprint_headers(
        item.parameters.as_deref().unwrap_or_default(),
        &mut fingerprint_headers,
    )
    .map_err(|rule| CompositionError::new(&subject, rule))?;
    let has_additional = !item.additional_operations.is_empty();
    let (method, operation) = path_operations_mut(item)
        .exactly_one()
        .map_err(|_| shape())?;
    let operation_id = operation
        .operation_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(shape)?;
    let at_operation = |rule| CompositionError::new(&operation_id, rule);
    let method = match (!has_additional, method) {
        (true, "POST") => MethodFilter::POST,
        (true, "PUT") => MethodFilter::PUT,
        (true, "PATCH") => MethodFilter::PATCH,
        (true, "DELETE") => MethodFilter::DELETE,
        _ => return Err(at_operation(Rule::Method).into()),
    };

    prepare_key_parameter(operation).map_err(at_operation)?;
    collect_fingerprint_headers(
        operation.parameters.as_deref().unwrap_or_default(),
        &mut fingerprint_headers,
    )
    .map_err(at_operation)?;
    fingerprint_headers.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
    fingerprint_headers.dedup();
    prepare_problem_responses(operation).map_err(at_operation)?;
    let schemas = &mut document.components.get_or_insert_default().schemas;
    crate::contract::merge_map(ComponentKind::Schemas, schemas, success_schemas)?;
    prepare_success_responses(operation, successes, schemas).map_err(at_operation)?;
    if operation.external_docs.is_none() {
        operation.external_docs = route_docs;
    }
    let rules_link = operation
        .external_docs
        .as_ref()
        .and_then(super::openapi::key_rules_link)
        .ok_or_else(|| at_operation(Rule::RetryDocumentation))?;
    Ok(PreparedOperation {
        id: operation_id,
        method,
        fingerprint_headers,
        rules_link,
    })
}

fn collect_fingerprint_headers(
    parameters: &[RefOr<Parameter>],
    headers: &mut Vec<HeaderName>,
) -> Result<(), Rule> {
    for parameter in parameters {
        let RefOr::T(parameter) = parameter else {
            return Err(Rule::FingerprintParameter);
        };
        if parameter.parameter_in == ParameterIn::Cookie {
            return Err(Rule::CookieParameter(parameter.name.clone()));
        }
        if parameter.parameter_in == ParameterIn::Header
            && !parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        {
            let name = HeaderName::from_bytes(parameter.name.as_bytes())
                .map_err(|_| Rule::FingerprintParameter)?;
            if name != CONTENT_TYPE {
                headers.push(name);
            }
        }
    }
    Ok(())
}

fn prepare_key_parameter(operation: &mut Operation) -> Result<(), Rule> {
    let generated_key = key_parameter();
    let existing_keys = operation
        .parameters
        .as_deref()
        .into_iter()
        .flatten()
        .filter_map(|parameter| match parameter {
            RefOr::T(parameter) => Some(parameter),
            RefOr::Ref(_) => None,
        })
        .filter(|parameter| {
            parameter.parameter_in == ParameterIn::Header
                && parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        });
    match existing_keys.at_most_one() {
        Ok(None) => operation
            .parameters
            .get_or_insert_default()
            .push(generated_key.into()),
        Ok(Some(existing)) if existing == &generated_key => {}
        _ => return Err(Rule::KeyParameter),
    }
    Ok(())
}

fn prepare_problem_responses(operation: &mut Operation) -> Result<(), Rule> {
    for (status, generated) in response_family() {
        if let Some(existing) = operation.responses.responses.get(&status)
            && existing != &generated
            && !can_replace_with_idempotency_response(&status, existing)
        {
            return Err(Rule::ProblemResponseConflict { status });
        }
        operation.responses.responses.insert(status, generated);
    }
    Ok(())
}

fn prepare_success_responses(
    operation: &mut Operation,
    successes: BTreeMap<String, RefOr<Response>>,
    schemas: &BTreeMap<String, RefOr<Schema>>,
) -> Result<(), Rule> {
    let responses = &mut operation.responses.responses;
    let conflict = |status: &str| Rule::SuccessConflict {
        status: status.to_owned(),
    };
    if successes.is_empty() {
        return Err(Rule::NoSuccess);
    }
    for status in responses.keys() {
        match status.as_bytes().first() {
            Some(b'1' | b'3') => {
                return Err(Rule::InformationalOrRedirectResponse {
                    status: status.clone(),
                });
            }
            Some(b'2') if !successes.contains_key(status) => return Err(conflict(status)),
            _ => {}
        }
    }
    for (status, response) in successes {
        if status != "2XX"
            && !axum::http::StatusCode::from_bytes(status.as_bytes())
                .is_ok_and(|code| code.is_success())
        {
            return Err(conflict(&status));
        }
        let referenced = || Rule::ReferencedSuccess {
            status: status.clone(),
        };
        let RefOr::T(mut response) = response else {
            return Err(referenced());
        };
        prepare_success_headers(&mut response, &status)?;
        if let Some(annotation) = responses.remove(&status) {
            let RefOr::T(mut annotation) = annotation else {
                return Err(referenced());
            };
            prepare_success_headers(&mut annotation, &status)?;
            supplement_success(&mut response, annotation, schemas)
                .ok_or_else(|| conflict(&status))?;
        }
        responses.insert(status, response.into());
    }
    Ok(())
}

fn supplement_success(
    response: &mut Response,
    annotation: Response,
    schemas: &BTreeMap<String, RefOr<Schema>>,
) -> Option<()> {
    for (media_type, content) in annotation.content {
        supplement_content(response.content.get_mut(&media_type)?, content, schemas)?;
    }
    for (name, header) in annotation.headers {
        if let Some((_, existing)) = response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            if !same_definition(existing, &header) {
                return None;
            }
        } else {
            response.headers.insert(name, header);
        }
    }
    if !annotation.description.is_empty() {
        response.description = annotation.description;
    }
    if annotation.summary.is_some() {
        response.summary = annotation.summary;
    }
    response.links.extend(annotation.links);
    if let Some(extensions) = annotation.extensions {
        response
            .extensions
            .get_or_insert_default()
            .merge(extensions);
    }
    Some(())
}

fn supplement_content(
    command: &mut RefOr<Content>,
    annotation: RefOr<Content>,
    schemas: &BTreeMap<String, RefOr<Schema>>,
) -> Option<()> {
    let (command, mut annotation) = match (command, annotation) {
        (RefOr::T(command), RefOr::T(annotation)) => (command, annotation),
        (RefOr::Ref(command), RefOr::Ref(annotation)) if *command == annotation => return Some(()),
        _ => return None,
    };
    if let Some(annotation_schema) = &annotation.schema {
        let command_schema = command.schema.as_ref()?;
        if !same_definition(command_schema, annotation_schema)
            && !same_definition(
                crate::contract::resolve_schema(command_schema, schemas)?,
                crate::contract::resolve_schema(annotation_schema, schemas)?,
            )
        {
            return None;
        }
        command.schema.clone_from(&annotation.schema);
    } else {
        annotation.schema.clone_from(&command.schema);
    }
    if annotation.example.is_some() || !annotation.examples.is_empty() {
        command.example.clone_from(&annotation.example);
        command.examples.clone_from(&annotation.examples);
    } else {
        annotation.example.clone_from(&command.example);
        annotation.examples.clone_from(&command.examples);
    }
    if annotation.description.is_some() {
        command.description.clone_from(&annotation.description);
    } else {
        annotation.description.clone_from(&command.description);
    }
    same_definition(command, &annotation).then_some(())
}

fn prepare_success_headers(response: &mut Response, status: &str) -> Result<(), Rule> {
    let replay_headers = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(REPLAYED_HEADER));
    match replay_headers.at_most_one() {
        Ok(None) => {
            response
                .headers
                .insert(REPLAYED_HEADER.to_owned(), replay_header());
        }
        Ok(Some((_, header))) if same_definition(header, &replay_header()) => {}
        _ => {
            return Err(Rule::ReplayHeader {
                status: status.to_owned(),
            });
        }
    }
    if let Some(name) = response.headers.keys().find(|name| {
        !name.eq_ignore_ascii_case(REPLAYED_HEADER) && !stored::is_replayable_header_name(name)
    }) {
        return Err(Rule::UnreplayableHeader {
            status: status.to_owned(),
            name: name.clone(),
        });
    }
    Ok(())
}

fn route_subject(paths: &Paths) -> String {
    paths
        .paths
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "route".to_owned())
}

#[cfg(test)]
mod tests {
    use utoipa::IntoResponses as _;
    use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};
    use utoipa_axum::routes;

    use super::*;
    use crate::authn::ProtectedOperationProblemResponses;
    use crate::contract::{FinalizeKind, PolicyFault};
    use crate::idempotency::{CreatedText, Rollback};

    const WIDGETS: &str = "/_test/widgets";

    fn prepare(
        paths: &mut Paths,
        docs: Option<utoipa::openapi::external_docs::ExternalDocs>,
    ) -> Result<PreparedOperation, crate::FinalizeError> {
        let mut document = utoipa::openapi::OpenApiBuilder::new()
            .paths(std::mem::take(paths))
            .external_docs(docs)
            .build();
        let result = super::prepare(&mut document, CreatedText::responses(), Vec::new());
        *paths = document.paths;
        result
    }

    fn rule(error: crate::FinalizeError) -> Rule {
        match error {
            crate::FinalizeError(FinalizeKind::Idempotency(error)) => error.rule,
            other => panic!("expected idempotency composition error, got {other:?}"),
        }
    }

    #[utoipa::path(
        post,
        path = "/_test/widgets",
        operation_id = "infraHttpTestCreateWidget",
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only operation through idempotent composition"
        }))),
        responses(
            (status = 201, description = "widget created", content_type = "text/plain", body = String,
             headers(("Location" = String, description = "widget URL")), example = json!("created")),
            ProtectedOperationProblemResponses
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
            "rationale": "test-only unsupported method"
        }))),
        responses((status = 200, description = "found"), ProtectedOperationProblemResponses)
    )]
    async fn get_widget() -> &'static str {
        "found"
    }

    fn widget_post_mut(paths: &mut Paths) -> &mut Operation {
        paths
            .paths
            .get_mut(WIDGETS)
            .expect("widget path")
            .post
            .as_mut()
            .expect("widget POST operation")
    }

    fn route() -> UtoipaMethodRouter {
        let mut route = routes!(create_widget);
        widget_post_mut(&mut route.1).external_docs = Some(
            utoipa::openapi::external_docs::ExternalDocs::new("https://docs.example/widgets#retry"),
        );
        route
    }

    struct TestCommand;

    impl crate::idempotency::FreshCommand for TestCommand {
        type Success = CreatedText;
        type Terminal = std::convert::Infallible;

        fn execute(
            self,
            _: &mut super::super::Tx<'_>,
        ) -> impl Future<Output = Result<Self::Success, Rollback>> + Send {
            std::future::poll_fn(|_| unreachable!("offline export"))
        }
    }

    #[test]
    fn finalization_requires_effective_authentication_for_idempotent_routes() {
        let bearer = serde_json::json!([{ "bearerAuth": [] }]);
        for (root_security, operation_security, protected) in [
            (None, None, false),
            (Some(bearer.clone()), Some(serde_json::json!([])), false),
            (Some(bearer.clone()), None, true),
            (None, Some(bearer), true),
        ] {
            let mut document = serde_json::json!({
                "openapi": "3.1.0",
                "info": {"title": "idempotency fixture", "version": "1"},
                "paths": {},
                "components": {
                    "securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}}
                }
            });
            if let Some(security) = root_security {
                document["security"] = security;
            }
            let mut carrier = route();
            widget_post_mut(&mut carrier.1).security =
                operation_security.map(|value| serde_json::from_value(value).unwrap());
            let root = crate::HttpContract::application(serde_json::from_value(document).unwrap())
                .expect("application metadata");
            let feature = OpenApiRouter::new().routes(carrier).into_openapi();
            let contract: crate::HttpContract<()> = root
                .merge(crate::idempotency::route(feature, || async { Ok(TestCommand) }).unwrap())
                .unwrap();

            let result = contract.into_openapi().map(|_| ());
            if protected {
                result.expect("protected operations can be exported offline");
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    crate::FinalizeError::new(FinalizeKind::InvalidPolicy {
                        path: WIDGETS.to_owned(),
                        method: "POST".to_owned(),
                        fault: PolicyFault::IdempotencyRequiresAuthentication,
                    })
                );
            }
        }
    }

    #[test]
    fn idempotent_features_reject_root_policy_and_conflicting_shared_components() {
        for (field, value, expected) in [
            (
                "security",
                serde_json::json!([]),
                crate::FinalizeError::new(FinalizeKind::ApplicationMetadata),
            ),
            (
                "components",
                serde_json::json!({"securitySchemes": {"featureAuth": {"type": "http", "scheme": "basic"}}}),
                crate::FinalizeError::new(FinalizeKind::ApplicationMetadata),
            ),
            (
                "components",
                serde_json::json!({"responses": {"IdempotencyBadRequest": {"description": "conflicting feature response"}}}),
                crate::FinalizeError::new(FinalizeKind::ConflictingComponent {
                    kind: ComponentKind::Responses,
                    name: "IdempotencyBadRequest".to_owned(),
                }),
            ),
        ] {
            let mut document =
                serde_json::to_value(OpenApiRouter::new().routes(route()).into_openapi()).unwrap();
            document[field] = value;
            let result = crate::idempotency::route::<(), _, _>(
                serde_json::from_value(document).unwrap(),
                || async { Ok(TestCommand) },
            );
            assert_eq!(result.unwrap_err(), expected);
        }
    }

    #[test]
    fn an_operation_without_published_retry_rules_fails_registration() {
        let (_, mut paths, _): UtoipaMethodRouter = routes!(create_widget);
        assert_eq!(
            rule(prepare(&mut paths, None).unwrap_err()),
            Rule::RetryDocumentation
        );
    }

    #[test]
    fn preparation_generates_key_family_and_replay_metadata() {
        let (_, mut paths, _): UtoipaMethodRouter = route();
        assert_eq!(
            prepare(&mut paths, None).unwrap().id,
            "infraHttpTestCreateWidget"
        );
        let operation = paths.paths[WIDGETS].post.as_ref().expect("post");
        assert!(
            operation
                .extensions
                .as_ref()
                .is_none_or(|extensions| !extensions.contains_key("x-idempotent"))
        );
        assert_eq!(operation.parameters.as_ref().map(Vec::len), Some(1));
        assert!(operation.responses.responses.contains_key("400"));
        let Some(RefOr::T(response)) = operation.responses.responses.get("201") else {
            panic!("inline success");
        };
        let documented = serde_json::to_value(response).unwrap();
        assert_eq!(documented["description"], "widget created");
        assert_eq!(
            documented["headers"]["Location"]["description"],
            "widget URL"
        );
        assert_eq!(documented["content"]["text/plain"]["example"], "created");
        assert_eq!(
            serde_json::to_value(&response.headers[REPLAYED_HEADER]).expect("header"),
            serde_json::json!({
                "description": "present with the string `true` only when this response replays a stored idempotent success",
                "schema": { "type": "string", "enum": ["true"] }
            })
        );
    }

    #[test]
    fn a_declared_cookie_is_rejected_at_either_parameter_location() {
        for (path_level, subject) in [(true, WIDGETS), (false, "infraHttpTestCreateWidget")] {
            let (_, mut paths, _): UtoipaMethodRouter = route();
            let item = paths.paths.get_mut(WIDGETS).unwrap();
            let parameters = if path_level {
                &mut item.parameters
            } else {
                &mut item.post.as_mut().unwrap().parameters
            };
            parameters.get_or_insert_default().push(
                Parameter::builder()
                    .name("region")
                    .parameter_in(ParameterIn::Cookie)
                    .build()
                    .into(),
            );
            assert_eq!(
                prepare(&mut paths, None).unwrap_err().to_string(),
                format!(
                    "idempotent route composition is invalid: {subject}: cookie parameter region is outside the idempotent request fingerprint"
                )
            );
        }
    }

    #[test]
    fn preparation_rejects_unsupported_shape_and_successes() {
        let (_, mut unsupported, _): UtoipaMethodRouter = routes!(get_widget);
        assert_eq!(
            rule(prepare(&mut unsupported, None).unwrap_err()),
            Rule::Method
        );

        let (_, mut missing_success, _): UtoipaMethodRouter = route();
        assert_eq!(
            prepare_success_responses(
                widget_post_mut(&mut missing_success),
                BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap_err(),
            Rule::NoSuccess
        );

        for (status, content) in [
            ("202", serde_json::json!({})),
            (
                "201",
                serde_json::json!({"application/json": {"schema": {"type": "string"}}}),
            ),
            (
                "201",
                serde_json::json!({"text/plain": {"schema": {"type": "integer"}}}),
            ),
        ] {
            let (_, mut paths, _): UtoipaMethodRouter = route();
            let operation = widget_post_mut(&mut paths);
            let annotation =
                serde_json::json!({"description": "conflicting success", "content": content});
            operation.responses.responses.insert(
                status.to_owned(),
                serde_json::from_value(annotation).unwrap(),
            );
            assert_eq!(
                rule(prepare(&mut paths, None).unwrap_err()),
                Rule::SuccessConflict {
                    status: status.to_owned()
                }
            );
        }

        let (_, mut referenced, _): UtoipaMethodRouter = route();
        widget_post_mut(&mut referenced).responses.responses.insert(
            "201".to_owned(),
            utoipa::openapi::Ref::from_response_name("Created").into(),
        );
        assert_eq!(
            prepare(&mut referenced, None).unwrap_err().to_string(),
            "idempotent route composition is invalid: infraHttpTestCreateWidget: response 201 must be inline so its replay metadata can be checked"
        );

        let (_, mut redirect, _): UtoipaMethodRouter = route();
        widget_post_mut(&mut redirect).responses.responses.insert(
            "304".to_owned(),
            utoipa::openapi::Ref::from_response_name("NotModified").into(),
        );
        assert_eq!(
            prepare(&mut redirect, None).unwrap_err().to_string(),
            "idempotent route composition is invalid: infraHttpTestCreateWidget: an idempotent operation must not declare informational or redirect response 304"
        );
    }

    #[test]
    fn preparation_names_the_conflicting_response_slot() {
        let (_, mut paths, _): UtoipaMethodRouter = route();
        widget_post_mut(&mut paths).responses.responses.insert(
            "409".to_owned(),
            utoipa::openapi::Ref::from_response_name("Conflict").into(),
        );

        assert_eq!(
            prepare(&mut paths, None).unwrap_err().to_string(),
            "idempotent route composition is invalid: infraHttpTestCreateWidget: response 409 conflicts with the idempotency response family"
        );
    }

    #[test]
    fn an_operation_can_describe_the_same_external_success_schema_as_its_command() {
        let response: RefOr<Response> = serde_json::from_value(serde_json::json!({
            "description": "created",
            "content": {"application/json": {"schema": {"$ref": "urn:example:widget"}}}
        }))
        .unwrap();
        let successes = BTreeMap::from([("201".to_owned(), response)]);
        let mut operation = Operation::default();
        operation.responses.responses.clone_from(&successes);
        prepare_success_responses(&mut operation, successes, &BTreeMap::new()).unwrap();
        let documented = serde_json::to_value(operation.responses.responses).unwrap();
        assert_eq!(
            documented["201"]["content"]["application/json"]["schema"]["$ref"],
            "urn:example:widget"
        );
    }

    #[test]
    fn preparation_rejects_conflicting_or_unstorable_success_headers() {
        for headers in [
            vec![(
                REPLAYED_HEADER,
                RefOr::T(utoipa::openapi::header::Header::default()),
            )],
            vec![
                ("Idempotent-Replayed", replay_header()),
                ("idempotent-replayed", replay_header()),
            ],
        ] {
            let (_, mut conflicting, _): UtoipaMethodRouter = route();
            let operation = widget_post_mut(&mut conflicting);
            let Some(RefOr::T(response)) = operation.responses.responses.get_mut("201") else {
                panic!("inline success");
            };
            response.headers.extend(
                headers
                    .into_iter()
                    .map(|(name, header)| (name.to_owned(), header)),
            );
            assert_eq!(
                rule(prepare(&mut conflicting, None).unwrap_err()),
                Rule::ReplayHeader {
                    status: "201".to_owned()
                }
            );
        }

        let (_, mut unsupported, _): UtoipaMethodRouter = route();
        let Some(RefOr::T(response)) = widget_post_mut(&mut unsupported)
            .responses
            .responses
            .get_mut("201")
        else {
            panic!("inline success");
        };
        response.headers.insert(
            "X-Trace".to_owned(),
            RefOr::T(utoipa::openapi::header::Header::default()),
        );
        assert_eq!(
            prepare(&mut unsupported, None).unwrap_err().to_string(),
            "idempotent route composition is invalid: infraHttpTestCreateWidget: response 201 may declare only replayable headers: X-Trace cannot be replayed"
        );
    }
}
