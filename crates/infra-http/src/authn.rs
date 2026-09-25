//! Final contract-driven inbound bearer authentication.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{FromRequestParts, MatchedPath, Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::{Failure, Principal, Verifier, parse_bearer};
use utoipa::openapi::{OpenApi, path::Operation};

use crate::contract::{ContractRouter, FinalizeError, RouteMethod, RouteMethods};
use crate::harden::RequestDeadline;
use crate::problem::{Code, Problem, SANITIZED_DETAIL};
use crate::request_id;

/// Verification outcomes at the HTTP authentication boundary.
pub const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

const PROTECTED_STATUSES: &[&str] = &["400", "401", "403", "431", "503", "504"];
const AUTHENTICATION_REQUIRED_DETAIL: &str = "bearer authentication is required";
const AUTHENTICATION_MALFORMED_DETAIL: &str = "bearer authentication is malformed";
const AUTHENTICATION_OVERSIZE_DETAIL: &str = "bearer authentication is too large";
const AUTHENTICATION_INVALID_DETAIL: &str = "bearer authentication is invalid";
const AUTHENTICATION_UNAVAILABLE_DETAIL: &str = "bearer authentication is unavailable";
const AUTHENTICATION_TIMEOUT_DETAIL: &str = "bearer authentication exceeded the request deadline";
const INSUFFICIENT_SCOPE_DETAIL: &str = "the verified principal lacks the required scope";

/// A principal verified by the final authentication layer.
///
/// The wrapped principal is private, so handlers can inspect only identity
/// values the selected verifier accepted. The extractor never parses an
/// inbound header and cannot manufacture an anonymous identity.
#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(Principal);

impl VerifiedPrincipal {
    /// The exact issuer that the verifier accepted.
    #[must_use]
    pub fn issuer(&self) -> &str {
        self.0.issuer()
    }

    /// The verified subject, when present in the selected token profile.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.0.subject()
    }

    /// The verified client identity, when present in the selected token profile.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.0.client_id()
    }

    /// The normalized verified scopes.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.0.scopes()
    }

    /// The verified `exp` claim in Unix epoch seconds.
    #[must_use]
    pub fn expires_at(&self) -> u64 {
        self.0.expires_at()
    }
}

impl<S> FromRequestParts<S> for VerifiedPrincipal
where
    S: Send + Sync,
{
    type Rejection = Response;

    fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Principal>()
                .cloned()
                .map(Self)
                .ok_or_else(|| {
                    Problem::new(Code::InternalServerError)
                        .detail(SANITIZED_DETAIL)
                        .request_id(request_id::request_id(&parts.extensions))
                        .into_response()
                }),
        )
    }
}

/// Require one scope without adding route-policy or role machinery.
///
/// The named scope is application policy, never inbound request data. A
/// verified principal missing it receives the standard insufficient-scope
/// challenge and the closed 403 Problem.
pub fn require_scope(principal: &VerifiedPrincipal, required: &str) -> Result<(), Response> {
    if principal.scopes().iter().any(|scope| scope == required) {
        return Ok(());
    }
    let mut response = Problem::new(Code::Forbidden)
        .detail(INSUFFICIENT_SCOPE_DETAIL)
        .into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Bearer error=\"insufficient_scope\""),
    );
    Err(response)
}

/// Consume an assembled contract, validate its effective security policy, and
/// apply the single outer authentication layer to every real endpoint.
///
/// # Errors
///
/// Returns a closed finalization error for unsupported or contradictory
/// OpenAPI security/exposure metadata. A real endpoint absent from the
/// document remains a served, sanitized 500 rather than a silent bypass.
pub fn finalize<S>(
    contract: ContractRouter<S>,
    verifier: Verifier,
    token_bound: usize,
) -> Result<Router<S>, FinalizeError>
where
    S: Send + Sync + Clone + 'static,
{
    let (contract, methods) = contract.into_parts();
    let policy = Policy::compile(contract.get_openapi(), &methods)?;
    let (router, _) = contract.split_for_parts();
    if methods.is_empty() {
        return Ok(router);
    }
    metrics::describe_counter!(
        AUTHN_VERIFICATIONS_METRIC,
        metrics::Unit::Count,
        "Inbound bearer-authentication verification outcomes."
    );
    Ok(router.route_layer(middleware::from_fn_with_state(
        AuthState {
            verifier,
            token_bound,
            methods,
            policy,
        },
        authenticate,
    )))
}

#[derive(Clone)]
struct AuthState {
    verifier: Verifier,
    token_bound: usize,
    methods: RouteMethods,
    policy: Policy,
}

#[derive(Clone, Copy)]
enum Access {
    Public,
    Protected,
    Missing,
}

#[derive(Clone)]
struct Policy(BTreeMap<(String, RouteMethod), Access>);

impl Policy {
    fn compile(document: &OpenApi, methods: &RouteMethods) -> Result<Self, FinalizeError> {
        let mut table = BTreeMap::new();
        for (path, method) in methods.iter() {
            let access = document
                .paths
                .paths
                .get(path)
                .and_then(|item| method.operation(item))
                .map(|operation| classify(document, operation))
                .transpose()?
                .unwrap_or(Access::Missing);
            table.insert((path.to_owned(), method), access);
        }
        Ok(Self(table))
    }

    fn access(&self, path: &str, method: RouteMethod) -> Access {
        self.0
            .get(&(path.to_owned(), method))
            .copied()
            .unwrap_or(Access::Missing)
    }
}

async fn authenticate(
    State(state): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request_id::request_id(request.extensions());
    request.extensions_mut().remove::<Principal>();
    request.extensions_mut().remove::<VerifiedPrincipal>();
    let Some(path) = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
    else {
        return wiring_failure(request_id);
    };
    let Some(method) = state.methods.effective(path, request.method()) else {
        return wiring_failure(request_id);
    };
    match state.policy.access(path, method) {
        Access::Public => next.run(request).await,
        Access::Missing => wiring_failure(request_id),
        Access::Protected => authenticate_protected(state, request, next, request_id).await,
    }
}

async fn authenticate_protected(
    state: AuthState,
    mut request: Request,
    next: Next,
    request_id: Option<String>,
) -> Response {
    let mut metric = VerificationMetric::new();
    let Some(deadline) = request
        .extensions()
        .get::<RequestDeadline>()
        .map(RequestDeadline::at)
    else {
        metric.wiring_failure();
        return wiring_failure(request_id);
    };
    let token = match parse_bearer(
        request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(axum::http::HeaderValue::as_bytes),
        state.token_bound,
    ) {
        Ok(token) => token,
        Err(failure) => {
            metric.failure(failure);
            return failure_response(failure, request_id);
        }
    };
    let principal = match state.verifier.verify(&token, deadline).await {
        Ok(principal) => principal,
        Err(failure) => {
            metric.failure(failure);
            return failure_response(failure, request_id);
        }
    };

    metric.success();
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn classify(document: &OpenApi, operation: &Operation) -> Result<Access, FinalizeError> {
    let Some(decision) = operation
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("x-security-decision"))
        .and_then(serde_json::Value::as_object)
    else {
        return Err(FinalizeError::InvalidPolicy);
    };
    let valid_rationale = decision
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|rationale| !rationale.trim().is_empty());
    if !valid_rationale {
        return Err(FinalizeError::InvalidPolicy);
    }
    match decision.get("exposure").and_then(serde_json::Value::as_str) {
        Some("public") if explicit_security(operation).is_some_and(is_empty_security) => {
            Ok(Access::Public)
        }
        Some("protected")
            if effective_security(document, operation).is_some_and(is_bearer_only)
                && protected_problem_responses(operation) =>
        {
            Ok(Access::Protected)
        }
        _ => Err(FinalizeError::InvalidPolicy),
    }
}

fn explicit_security(operation: &Operation) -> Option<serde_json::Value> {
    serde_json::to_value(operation)
        .ok()?
        .get("security")
        .cloned()
}

fn effective_security(document: &OpenApi, operation: &Operation) -> Option<serde_json::Value> {
    explicit_security(operation).or_else(|| {
        serde_json::to_value(document)
            .ok()?
            .get("security")
            .cloned()
    })
}

fn is_empty_security(security: serde_json::Value) -> bool {
    security.as_array().is_some_and(Vec::is_empty)
}

fn is_bearer_only(security: serde_json::Value) -> bool {
    security.as_array().is_some_and(|requirements| {
        !requirements.is_empty()
            && requirements.iter().all(|requirement| {
                requirement.as_object().is_some_and(|requirement| {
                    requirement.len() == 1
                        && requirement.get("bearerAuth")
                            == Some(&serde_json::Value::Array(Vec::new()))
                })
            })
    })
}

fn protected_problem_responses(operation: &Operation) -> bool {
    let Ok(serde_json::Value::Object(responses)) = serde_json::to_value(&operation.responses)
    else {
        return false;
    };
    PROTECTED_STATUSES
        .iter()
        .all(|status| responses.contains_key(*status))
}

fn wiring_failure(request_id: Option<String>) -> Response {
    Problem::new(Code::InternalServerError)
        .detail(SANITIZED_DETAIL)
        .request_id(request_id)
        .into_response()
}

fn failure_response(failure: Failure, request_id: Option<String>) -> Response {
    let (code, detail, challenge) = match failure {
        Failure::Missing => (
            Code::AuthenticationRequired,
            AUTHENTICATION_REQUIRED_DETAIL,
            Some("Bearer"),
        ),
        Failure::Malformed => (
            Code::AuthenticationMalformed,
            AUTHENTICATION_MALFORMED_DETAIL,
            None,
        ),
        Failure::Oversize => (
            Code::AuthenticationOversize,
            AUTHENTICATION_OVERSIZE_DETAIL,
            None,
        ),
        Failure::Invalid => (
            Code::AuthenticationInvalid,
            AUTHENTICATION_INVALID_DETAIL,
            Some("Bearer error=\"invalid_token\""),
        ),
        Failure::Unavailable => (
            Code::AuthenticationUnavailable,
            AUTHENTICATION_UNAVAILABLE_DETAIL,
            None,
        ),
        Failure::Timeout => (Code::RequestTimeout, AUTHENTICATION_TIMEOUT_DETAIL, None),
    };
    let mut response = Problem::new(code)
        .detail(detail)
        .request_id(request_id)
        .into_response();
    if let Some(challenge) = challenge {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static(challenge),
        );
    }
    response
}

struct VerificationMetric {
    recorded: bool,
}

impl VerificationMetric {
    const fn new() -> Self {
        Self { recorded: false }
    }

    fn success(&mut self) {
        record_verification("success", "none");
        self.recorded = true;
    }

    fn failure(&mut self, failure: Failure) {
        record_verification("failure", failure_class(failure));
        self.recorded = true;
    }

    fn wiring_failure(&mut self) {
        record_verification("failure", "wiring");
        self.recorded = true;
    }
}

impl Drop for VerificationMetric {
    fn drop(&mut self) {
        if !self.recorded {
            record_verification("cancelled", "cancelled");
        }
    }
}

fn record_verification(result: &'static str, failure: &'static str) {
    metrics::counter!(
        AUTHN_VERIFICATIONS_METRIC,
        "transport" => "http",
        "result" => result,
        "failure" => failure
    )
    .increment(1);
}

const fn failure_class(failure: Failure) -> &'static str {
    match failure {
        Failure::Missing => "missing",
        Failure::Malformed => "malformed",
        Failure::Oversize => "oversize",
        Failure::Invalid => "invalid",
        Failure::Unavailable => "unavailable",
        Failure::Timeout => "timeout",
    }
}
