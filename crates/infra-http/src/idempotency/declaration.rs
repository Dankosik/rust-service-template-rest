//! Local contract generation and validation for one idempotent route.

use axum::http::HeaderName;
use axum::http::header::CONTENT_TYPE;
use utoipa::openapi::RefOr;
use utoipa::openapi::path::{Operation, Parameter, ParameterIn, PathItem, Paths};
use utoipa::openapi::response::Response;

use super::openapi::{
    KEY_HEADER, key_parameter, replaces_protected_response, replay_header, response_family,
};
use super::stored;

const IDEMPOTENT_METHODS: [&str; 4] = ["post", "put", "patch", "delete"];

/// A local authoring error on the one route carrier being composed.
///
/// The error names only the route or operation and fixed authoring rule, never
/// request data. A caller must correct it before the service can assemble its
/// routes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("idempotent route composition is invalid: {subject}: {}", .rule.text())]
pub struct CompositionError {
    /// The route path while no operationId is readable, otherwise the operationId.
    subject: String,
    rule: Rule,
}

impl CompositionError {
    pub(super) fn new(subject: &str, rule: Rule) -> Self {
        Self {
            subject: subject.to_owned(),
            rule,
        }
    }

    #[cfg(test)]
    pub(super) const fn rule(&self) -> Rule {
        self.rule
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rule {
    Shape,
    Method,
    KeyParameter,
    FingerprintParameter,
    NonSuccessStatus,
    NoSuccess,
    UnreplayableHeader,
    ReferencedSuccess,
    ProblemResponses,
    ReplayHeader,
}

impl Rule {
    const fn text(self) -> &'static str {
        match self {
            Self::Shape => {
                "a composed route must be one path and one operation with a nonempty operationId"
            }
            Self::Method => "the method must be POST, PUT, PATCH, or DELETE",
            Self::KeyParameter => {
                "Idempotency-Key must be absent or one byte-equal generated header parameter"
            }
            Self::FingerprintParameter => {
                "request parameters must be inline and header parameters must have valid HTTP field names"
            }
            Self::NonSuccessStatus => {
                "an idempotent operation must not declare a 1xx or 3xx response"
            }
            Self::NoSuccess => "an idempotent operation must declare a 2xx response",
            Self::UnreplayableHeader => "a 2xx response may declare only replayable headers",
            Self::ReferencedSuccess => {
                "each 2xx response must be inline so its replay metadata can be checked"
            }
            Self::ProblemResponses => {
                "a generated response slot conflicts with the idempotency response family"
            }
            Self::ReplayHeader => {
                "Idempotent-Replayed is reserved for exact generated replay metadata"
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct PreparedOperation {
    pub(super) id: String,
    pub(super) fingerprint_headers: Vec<HeaderName>,
}

pub(super) fn prepare(paths: &mut Paths) -> Result<PreparedOperation, CompositionError> {
    let subject = route_subject(paths);
    let shape = || CompositionError::new(&subject, Rule::Shape);
    let mut items = paths.paths.values_mut();
    let (Some(item), None) = (items.next(), items.next()) else {
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
    if !IDEMPOTENT_METHODS.contains(&method) {
        return Err(CompositionError::new(&operation_id, Rule::Method));
    }

    prepare_key_parameter(operation, &operation_id)?;
    collect_fingerprint_headers(
        operation.parameters.as_deref().unwrap_or_default(),
        &operation_id,
        &mut fingerprint_headers,
    )?;
    fingerprint_headers.sort_unstable_by(|left, right| left.as_str().cmp(right.as_str()));
    fingerprint_headers.dedup();
    prepare_problem_responses(operation, &operation_id)?;
    prepare_success_responses(operation, &operation_id)?;
    Ok(PreparedOperation {
        id: operation_id,
        fingerprint_headers,
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
    let mut existing_keys = operation
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
    match (existing_keys.next(), existing_keys.next()) {
        (None, None) => operation
            .parameters
            .get_or_insert_default()
            .push(generated_key.into()),
        (Some(existing), None) if existing == &generated_key => {}
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
            && !replaces_protected_response(&status, existing)
        {
            return Err(CompositionError::new(operation_id, Rule::ProblemResponses));
        }
        operation.responses.responses.insert(status, generated);
    }
    Ok(())
}

fn prepare_success_responses(
    operation: &mut Operation,
    operation_id: &str,
) -> Result<(), CompositionError> {
    let mut success = false;
    for (status, response) in &mut operation.responses.responses {
        match status.as_bytes().first() {
            Some(b'1' | b'3') => {
                return Err(CompositionError::new(operation_id, Rule::NonSuccessStatus));
            }
            Some(b'2') => {
                success = true;
                let RefOr::T(response) = response else {
                    return Err(CompositionError::new(operation_id, Rule::ReferencedSuccess));
                };
                prepare_success_headers(response, operation_id)?;
            }
            _ => {}
        }
    }
    if !success {
        return Err(CompositionError::new(operation_id, Rule::NoSuccess));
    }
    Ok(())
}

fn prepare_success_headers(
    response: &mut Response,
    operation_id: &str,
) -> Result<(), CompositionError> {
    let mut replay_headers = response
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(super::openapi::REPLAYED_HEADER));
    match (replay_headers.next(), replay_headers.next()) {
        (None, None) => {
            response
                .headers
                .insert(super::openapi::REPLAYED_HEADER.to_owned(), replay_header());
        }
        (Some((_, header)), None) if header == &replay_header() => {}
        _ => return Err(CompositionError::new(operation_id, Rule::ReplayHeader)),
    }
    if response.headers.keys().any(|name| {
        !name.eq_ignore_ascii_case(super::openapi::REPLAYED_HEADER)
            && !stored::is_replayable_header_name(name)
    }) {
        return Err(CompositionError::new(
            operation_id,
            Rule::UnreplayableHeader,
        ));
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
        ("query", item.query.as_mut()),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.map(|operation| (method, operation)))
    .chain(
        item.additional_operations
            .values_mut()
            .map(|operation| ("additional", operation)),
    );
    let operation = operations.next()?;
    operations.next().is_none().then_some(operation)
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
    use utoipa_axum::router::UtoipaMethodRouter;
    use utoipa_axum::routes;

    use super::*;
    use crate::authn::ProtectedOperationProblemResponses;

    const WIDGETS: &str = "/_test/widgets";

    #[utoipa::path(
        post,
        path = "/_test/widgets",
        operation_id = "infraHttpTestCreateWidget",
        security(("bearerAuth" = [])),
        extensions(("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only operation through idempotent composition"
        }))),
        responses((status = 201, description = "created", content_type = "text/plain", body = String), ProtectedOperationProblemResponses)
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
        routes!(create_widget)
    }

    #[test]
    fn preparation_generates_key_family_and_replay_metadata() {
        let (_, mut paths, _): UtoipaMethodRouter = route();
        assert_eq!(prepare(&mut paths).unwrap().id, "infraHttpTestCreateWidget");
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
        assert_eq!(
            serde_json::to_value(&response.headers[super::super::openapi::REPLAYED_HEADER])
                .expect("header"),
            serde_json::json!({
                "description": "present with the string `true` only when this response replays a stored idempotent success",
                "schema": { "type": "string", "enum": ["true"] }
            })
        );
    }

    #[test]
    fn preparation_rejects_unsupported_shape_and_successes() {
        let (_, mut unsupported, _): UtoipaMethodRouter = routes!(get_widget);
        assert_eq!(prepare(&mut unsupported).unwrap_err().rule(), Rule::Method);

        let (_, mut missing_success, _): UtoipaMethodRouter = route();
        missing_success
            .paths
            .get_mut(WIDGETS)
            .expect("path")
            .post
            .as_mut()
            .expect("post")
            .responses
            .responses
            .remove("201");
        assert_eq!(
            prepare(&mut missing_success).unwrap_err().rule(),
            Rule::NoSuccess
        );

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
            prepare(&mut referenced).unwrap_err().rule(),
            Rule::ReferencedSuccess
        );
    }

    #[test]
    fn preparation_rejects_conflicting_or_unstorable_success_headers() {
        let (_, mut conflicting, _): UtoipaMethodRouter = route();
        let Some(RefOr::T(response)) = conflicting
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
            super::super::openapi::REPLAYED_HEADER.to_owned(),
            RefOr::T(utoipa::openapi::header::Header::default()),
        );
        assert_eq!(
            prepare(&mut conflicting).unwrap_err().rule(),
            Rule::ReplayHeader
        );

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
            prepare(&mut unsupported).unwrap_err().rule(),
            Rule::UnreplayableHeader
        );
    }
}
