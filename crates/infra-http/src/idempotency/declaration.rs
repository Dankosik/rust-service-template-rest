use std::collections::BTreeMap;

use axum::http::HeaderName;
use axum::http::header::CONTENT_TYPE;
use axum::routing::MethodFilter;
use itertools::Itertools as _;
use utoipa::openapi::path::{Operation, Parameter, ParameterIn, PathItem, Paths};
use utoipa::openapi::response::Response;
use utoipa::openapi::{RefOr, Schema};

use crate::contract::fixed_path_operations;

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

    pub(super) fn contract(subject: &str, error: crate::FinalizeError) -> Self {
        Self::new(subject, Rule::Contract(error))
    }

    #[cfg(test)]
    const fn rule(&self) -> &Rule {
        &self.rule
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
enum Rule {
    #[error(transparent)]
    Contract(crate::FinalizeError),
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
) -> Result<PreparedOperation, CompositionError> {
    let paths = &mut document.paths;
    let route_docs = document.external_docs.take();
    let subject = route_subject(paths);
    let shape = || CompositionError::new(&subject, Rule::Shape);
    let Ok(item) = paths.paths.values_mut().exactly_one() else {
        return Err(shape());
    };
    if item.ref_path.is_some() {
        return Err(shape());
    }
    let mut fingerprint_headers = Vec::new();
    collect_fingerprint_headers(
        item.parameters.as_deref().unwrap_or_default(),
        &subject,
        &mut fingerprint_headers,
    )?;
    let (method, operation) = one_operation_mut(item).ok_or_else(shape)?;
    let operation_id = operation
        .operation_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(shape)?;
    let method = match method {
        "POST" => MethodFilter::POST,
        "PUT" => MethodFilter::PUT,
        "PATCH" => MethodFilter::PATCH,
        "DELETE" => MethodFilter::DELETE,
        _ => return Err(CompositionError::new(&operation_id, Rule::Method)),
    };

    prepare_key_parameter(operation, &operation_id)?;
    collect_fingerprint_headers(
        operation.parameters.as_deref().unwrap_or_default(),
        &operation_id,
        &mut fingerprint_headers,
    )?;
    fingerprint_headers.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
    fingerprint_headers.dedup();
    prepare_problem_responses(operation, &operation_id)?;
    let schemas = &mut document.components.get_or_insert_default().schemas;
    crate::contract::merge_map("schemas", schemas, success_schemas)
        .map_err(|error| CompositionError::contract(&operation_id, error))?;
    prepare_success_responses(operation, &operation_id, successes, schemas)?;
    if operation.external_docs.is_none() {
        operation.external_docs = route_docs;
    }
    let rules_link = super::openapi::key_rules_link(operation.external_docs.as_ref())
        .ok_or_else(|| CompositionError::new(&operation_id, Rule::RetryDocumentation))?;
    Ok(PreparedOperation {
        id: operation_id,
        method,
        fingerprint_headers,
        rules_link,
    })
}

fn collect_fingerprint_headers(
    parameters: &[RefOr<Parameter>],
    subject: &str,
    headers: &mut Vec<HeaderName>,
) -> Result<(), CompositionError> {
    let invalid = || CompositionError::new(subject, Rule::FingerprintParameter);
    for parameter in parameters {
        let RefOr::T(parameter) = parameter else {
            return Err(invalid());
        };
        if parameter.parameter_in == ParameterIn::Cookie {
            return Err(CompositionError::new(
                subject,
                Rule::CookieParameter(parameter.name.clone()),
            ));
        }
        if parameter.parameter_in == ParameterIn::Header
            && !parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        {
            let name = HeaderName::from_bytes(parameter.name.as_bytes()).map_err(|_| invalid())?;
            if name != CONTENT_TYPE {
                headers.push(name);
            }
        }
    }
    Ok(())
}

fn prepare_key_parameter(
    operation: &mut Operation,
    operation_id: &str,
) -> Result<(), CompositionError> {
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
        _ => return Err(CompositionError::new(operation_id, Rule::KeyParameter)),
    }
    Ok(())
}

fn prepare_problem_responses(
    operation: &mut Operation,
    operation_id: &str,
) -> Result<(), CompositionError> {
    for (status, generated) in response_family() {
        if let Some(existing) = operation.responses.responses.get(&status)
            && existing != &generated
            && !can_replace_with_idempotency_response(&status, existing)
        {
            return Err(CompositionError::new(
                operation_id,
                Rule::ProblemResponseConflict { status },
            ));
        }
        operation.responses.responses.insert(status, generated);
    }
    Ok(())
}

fn prepare_success_responses(
    operation: &mut Operation,
    operation_id: &str,
    successes: BTreeMap<String, RefOr<Response>>,
    schemas: &BTreeMap<String, RefOr<Schema>>,
) -> Result<(), CompositionError> {
    let responses = &mut operation.responses.responses;
    let conflict = |status: &str| {
        CompositionError::new(
            operation_id,
            Rule::SuccessConflict {
                status: status.to_owned(),
            },
        )
    };
    if successes.is_empty() {
        return Err(CompositionError::new(operation_id, Rule::NoSuccess));
    }
    for status in responses.keys() {
        match status.as_bytes().first() {
            Some(b'1' | b'3') => {
                return Err(CompositionError::new(
                    operation_id,
                    Rule::InformationalOrRedirectResponse {
                        status: status.clone(),
                    },
                ));
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
        let referenced = || {
            CompositionError::new(
                operation_id,
                Rule::ReferencedSuccess {
                    status: status.clone(),
                },
            )
        };
        let RefOr::T(mut response) = response else {
            return Err(referenced());
        };
        prepare_success_headers(&mut response, operation_id, &status)?;
        if let Some(annotation) = responses.remove(&status) {
            let RefOr::T(mut annotation) = annotation else {
                return Err(referenced());
            };
            prepare_success_headers(&mut annotation, operation_id, &status)?;
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
        let actual = response.content.get_mut(&media_type)?;
        let (actual, mut supplement) = match (actual, content) {
            (RefOr::T(actual), RefOr::T(supplement)) => (actual, supplement),
            (RefOr::Ref(actual), RefOr::Ref(supplement)) if *actual == supplement => continue,
            _ => return None,
        };
        if let Some(declared) = &supplement.schema {
            let typed = actual.schema.as_ref()?;
            if crate::contract::resolve_schema(typed, schemas)?
                != crate::contract::resolve_schema(declared, schemas)?
            {
                return None;
            }
            actual.schema.clone_from(&supplement.schema);
        } else {
            supplement.schema.clone_from(&actual.schema);
        }
        if supplement.example.is_some() || !supplement.examples.is_empty() {
            actual.example.clone_from(&supplement.example);
            actual.examples.clone_from(&supplement.examples);
        } else {
            supplement.example.clone_from(&actual.example);
            supplement.examples.clone_from(&actual.examples);
        }
        if supplement.description.is_some() {
            actual.description.clone_from(&supplement.description);
        } else {
            supplement.description.clone_from(&actual.description);
        }
        if *actual != supplement {
            return None;
        }
    }
    for (name, header) in annotation.headers {
        if let Some((_, existing)) = response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(&name))
        {
            if existing != &header {
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

fn prepare_success_headers(
    response: &mut Response,
    operation_id: &str,
    status: &str,
) -> Result<(), CompositionError> {
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
        Ok(Some((_, header))) if header == &replay_header() => {}
        _ => {
            return Err(CompositionError::new(
                operation_id,
                Rule::ReplayHeader {
                    status: status.to_owned(),
                },
            ));
        }
    }
    if let Some(name) = response.headers.keys().find(|name| {
        !name.eq_ignore_ascii_case(REPLAYED_HEADER) && !stored::is_replayable_header_name(name)
    }) {
        return Err(CompositionError::new(
            operation_id,
            Rule::UnreplayableHeader {
                status: status.to_owned(),
                name: name.clone(),
            },
        ));
    }
    Ok(())
}

fn one_operation_mut(item: &mut PathItem) -> Option<(&'static str, &mut Operation)> {
    fixed_path_operations!(item, as_mut)
        .into_iter()
        .filter_map(|(method, operation)| operation.map(|operation| (method, operation)))
        .chain(
            item.additional_operations
                .values_mut()
                .map(|operation| ("additional", operation)),
        )
        .exactly_one()
        .ok()
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
    use crate::idempotency::{CreatedText, Rollback};

    const WIDGETS: &str = "/_test/widgets";

    fn prepare(
        paths: &mut Paths,
        docs: Option<utoipa::openapi::external_docs::ExternalDocs>,
    ) -> Result<PreparedOperation, CompositionError> {
        let mut document = utoipa::openapi::OpenApiBuilder::new()
            .paths(std::mem::take(paths))
            .external_docs(docs)
            .build();
        let result = super::prepare(&mut document, CreatedText::responses(), Vec::new());
        *paths = document.paths;
        result
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

    fn route() -> UtoipaMethodRouter {
        let mut route = routes!(create_widget);
        route
            .1
            .paths
            .get_mut(WIDGETS)
            .unwrap()
            .post
            .as_mut()
            .unwrap()
            .external_docs = Some(utoipa::openapi::external_docs::ExternalDocs::new(
            "https://docs.example/widgets#retry",
        ));
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
            carrier
                .1
                .paths
                .get_mut(WIDGETS)
                .unwrap()
                .post
                .as_mut()
                .unwrap()
                .security = operation_security.map(|value| serde_json::from_value(value).unwrap());
            let root = crate::HttpContract::application(serde_json::from_value(document).unwrap())
                .expect("application metadata");
            let feature = OpenApiRouter::new().routes(carrier).into_openapi();
            let contract: crate::HttpContract<()> = root
                .merge(
                    crate::idempotency::route(feature, |_, _, _, ()| async { Ok(TestCommand) })
                        .unwrap(),
                )
                .unwrap();

            let result = contract.into_openapi().map(|_| ());
            if protected {
                result.expect("protected operations can be exported offline");
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    crate::FinalizeError::InvalidPolicy {
                        path: WIDGETS.to_owned(),
                        method: "POST".to_owned(),
                        fault: "idempotency_requires_authentication",
                    }
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
                crate::FinalizeError::ApplicationMetadata,
            ),
            (
                "components",
                serde_json::json!({"securitySchemes": {"featureAuth": {"type": "http", "scheme": "basic"}}}),
                crate::FinalizeError::ApplicationMetadata,
            ),
            (
                "components",
                serde_json::json!({"responses": {"IdempotencyBadRequest": {"description": "conflicting feature response"}}}),
                crate::FinalizeError::ConflictingComponent {
                    kind: "responses",
                    name: "IdempotencyBadRequest".to_owned(),
                },
            ),
        ] {
            let mut document =
                serde_json::to_value(OpenApiRouter::new().routes(route()).into_openapi()).unwrap();
            document[field] = value;
            let result = crate::idempotency::route(
                serde_json::from_value(document).unwrap(),
                |_, _, _, ()| async { Ok(TestCommand) },
            );
            assert_eq!(result.unwrap_err().rule(), &Rule::Contract(expected));
        }
    }

    #[test]
    fn an_operation_without_published_retry_rules_fails_registration() {
        let (_, mut paths, _): UtoipaMethodRouter = routes!(create_widget);
        assert_eq!(
            prepare(&mut paths, None).unwrap_err().rule(),
            &Rule::RetryDocumentation
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
        for path_level in [true, false] {
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
                prepare(&mut paths, None).unwrap_err().rule(),
                &Rule::CookieParameter("region".to_owned())
            );
        }
    }

    #[test]
    fn preparation_rejects_unsupported_shape_and_successes() {
        let (_, mut unsupported, _): UtoipaMethodRouter = routes!(get_widget);
        assert_eq!(
            prepare(&mut unsupported, None).unwrap_err().rule(),
            &Rule::Method
        );

        let (_, mut missing_success, _): UtoipaMethodRouter = route();
        assert_eq!(
            prepare_success_responses(
                missing_success
                    .paths
                    .get_mut(WIDGETS)
                    .unwrap()
                    .post
                    .as_mut()
                    .unwrap(),
                "infraHttpTestCreateWidget",
                BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap_err()
            .rule(),
            &Rule::NoSuccess
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
            let operation = paths.paths.get_mut(WIDGETS).unwrap().post.as_mut().unwrap();
            let annotation =
                serde_json::json!({"description": "conflicting success", "content": content});
            operation.responses.responses.insert(
                status.to_owned(),
                serde_json::from_value(annotation).unwrap(),
            );
            assert_eq!(
                prepare(&mut paths, None).unwrap_err().rule(),
                &Rule::SuccessConflict {
                    status: status.to_owned()
                }
            );
        }

        let (_, mut referenced, _): UtoipaMethodRouter = route();
        referenced
            .paths
            .get_mut(WIDGETS)
            .expect("path")
            .post
            .as_mut()
            .expect("post")
            .responses
            .responses
            .insert(
                "201".to_owned(),
                utoipa::openapi::Ref::from_response_name("Created").into(),
            );
        assert_eq!(
            prepare(&mut referenced, None).unwrap_err().to_string(),
            "idempotent route composition is invalid: infraHttpTestCreateWidget: response 201 must be inline so its replay metadata can be checked"
        );

        let (_, mut redirect, _): UtoipaMethodRouter = route();
        redirect
            .paths
            .get_mut(WIDGETS)
            .expect("path")
            .post
            .as_mut()
            .expect("post")
            .responses
            .responses
            .insert(
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
        paths
            .paths
            .get_mut(WIDGETS)
            .expect("path")
            .post
            .as_mut()
            .expect("post")
            .responses
            .responses
            .insert(
                "409".to_owned(),
                utoipa::openapi::Ref::from_response_name("Conflict").into(),
            );

        assert_eq!(
            prepare(&mut paths, None).unwrap_err().to_string(),
            "idempotent route composition is invalid: infraHttpTestCreateWidget: response 409 conflicts with the idempotency response family"
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
            let operation = conflicting
                .paths
                .get_mut(WIDGETS)
                .unwrap()
                .post
                .as_mut()
                .unwrap();
            let Some(RefOr::T(response)) = operation.responses.responses.get_mut("201") else {
                panic!("inline success");
            };
            response.headers.extend(
                headers
                    .into_iter()
                    .map(|(name, header)| (name.to_owned(), header)),
            );
            assert_eq!(
                prepare(&mut conflicting, None).unwrap_err().rule(),
                &Rule::ReplayHeader {
                    status: "201".to_owned()
                }
            );
        }

        let (_, mut unsupported, _): UtoipaMethodRouter = route();
        let Some(RefOr::T(response)) = unsupported
            .paths
            .get_mut(WIDGETS)
            .expect("path")
            .post
            .as_mut()
            .expect("post")
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
