//! Final contract-driven inbound bearer authentication and the Problem
//! responses every protected operation declares.

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::{Failure, Principal, Transport, Verifier};
use operation_context::Stopped;
use service_failure::INSUFFICIENT_SCOPE_DETAIL;
use utoipa::{IntoResponses, ToResponse};
use utoipa_axum::router::OpenApiRouter;

use crate::contract::{Access, FinalizeError, Policy, layer_routes};
use crate::problem::{Code, Problem, sanitized_internal_error};

/// A principal verified by the final authentication layer.
///
/// The wrapped principal is private, so handlers can inspect only identity
/// values the selected verifier accepted. The extractor never parses an
/// inbound header and cannot manufacture an anonymous identity.
#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(Principal);

impl VerifiedPrincipal {
    #[must_use]
    pub fn issuer(&self) -> &str {
        self.0.issuer()
    }

    /// See [`Principal::subject`].
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.0.subject()
    }

    /// See [`Principal::client_id`].
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.0.client_id()
    }

    /// See [`Principal::scopes`].
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.0.scopes()
    }

    /// Deserialize application claims from the same accepted verification evidence.
    ///
    /// # Errors
    /// Returns a sanitized typed error if the requested claim shape does not match.
    pub fn claims<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<T, infra_bearerauthn::ClaimAccessError> {
        self.0.claims()
    }

    /// The verified `exp` claim as Unix time in seconds, without leeway.
    #[must_use]
    pub fn expires_at(&self) -> u64 {
        self.0.expires_at()
    }

    /// The verified bearer token, only as the subject of an RFC 8693 token
    /// exchange; never forward it as an outbound `Authorization` header.
    #[must_use]
    pub fn access_token(&self) -> &secrecy::SecretString {
        self.0.access_token()
    }

    /// See [`Principal::actor`].
    #[must_use]
    pub fn actor(&self) -> Option<&infra_bearerauthn::Actor> {
        self.0.actor()
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
        std::future::ready(crate::context::extension::<Principal>(parts).map(Self))
    }
}

/// Consume an assembled contract, validate its effective security policy, and
/// apply the single outer authentication layer to every real endpoint.
///
/// # Errors
///
/// Returns a closed finalization error for unsupported or contradictory
/// OpenAPI effective security. A real endpoint absent from the
/// document remains a served, sanitized 500 rather than a silent bypass.
pub fn finalize<S>(
    contract: OpenApiRouter<S>,
    verifier: Verifier,
) -> Result<Router<S>, FinalizeError>
where
    S: Send + Sync + Clone + 'static,
{
    let policy = Policy::compile(contract.get_openapi())?;
    Ok(layer_routes(contract, |router| {
        router.route_layer(middleware::from_fn_with_state(
            AuthState { verifier, policy },
            authenticate,
        ))
    }))
}

#[derive(Clone)]
struct AuthState {
    verifier: Verifier,
    policy: Policy,
}

async fn authenticate(
    State(state): State<AuthState>,
    mut request: Request,
    next: Next,
) -> Response {
    // Identity comes only from verification in this layer.
    request.extensions_mut().remove::<Principal>();
    let AuthState { verifier, policy } = state;
    let Some(path) = crate::contract::contract_path(request.extensions()) else {
        return wiring_failure("path_missing");
    };
    match policy.access(path, request.method()) {
        Some(Access::Public) => next.run(request).await,
        None => {
            tracing::error!(
                route = path,
                method = %request.method(),
                failure = "policy_missing",
                "http_auth_wiring_failed"
            );
            sanitized_internal_error()
        }
        Some(Access::Protected(alternatives)) => {
            authenticate_protected(&verifier, alternatives, request, next).await
        }
    }
}

async fn authenticate_protected(
    verifier: &Verifier,
    alternatives: &[Box<[String]>],
    mut request: Request,
    next: Next,
) -> Response {
    let Some(context) = request.extensions().get::<crate::RequestContext>().cloned() else {
        return wiring_failure("context_missing");
    };
    let authorization = request
        .headers()
        .get_all(AUTHORIZATION)
        .iter()
        .map(axum::http::HeaderValue::as_bytes);
    let result = verifier
        .authenticate_with_context(authorization, Transport::Http, context.operation())
        .await;
    // The opening deadline or cancellation wins over the verifier's
    // unavailable result, the scope check, and handler start: a stopped
    // request answers 504 (deadline) or 503 (cancelled) and never reaches the
    // handler. Each check follows a step that can outlive the cutoff.
    if let Some(stopped) = context.operation().stopped() {
        return stopped_response(stopped);
    }
    let principal = match result {
        Ok(principal) => principal,
        Err(failure) => return failure_response(failure),
    };
    let granted = grants_any(alternatives, &principal);
    if let Some(stopped) = context.operation().stopped() {
        return stopped_response(stopped);
    }
    if !granted {
        return insufficient_scope_response();
    }
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(principal);
    if let Some(stopped) = context.operation().stopped() {
        return stopped_response(stopped);
    }
    next.run(request).await
}

fn stopped_response(stopped: Stopped) -> Response {
    match stopped {
        Stopped::Deadline => crate::context::budget_expired_response(),
        Stopped::Cancelled => failure_response(Failure::Unavailable),
    }
}

/// At least one alternative's scopes must all be granted; an alternative with
/// no scopes is any authenticated caller.
fn grants_any(alternatives: &[Box<[String]>], principal: &Principal) -> bool {
    alternatives
        .iter()
        .any(|scopes| principal.has_scopes(scopes))
}

fn insufficient_scope_response() -> Response {
    let mut response = Problem::new(Code::Forbidden)
        .detail(INSUFFICIENT_SCOPE_DETAIL)
        .into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        axum::http::HeaderValue::from_static("Bearer error=\"insufficient_scope\""),
    );
    response
}

fn wiring_failure(failure: &'static str) -> Response {
    tracing::error!(failure, "http_auth_wiring_failed");
    sanitized_internal_error()
}

fn failure_response(failure: Failure) -> Response {
    let (code, challenge) = match failure {
        Failure::Missing => (Code::AuthenticationRequired, Some("Bearer")),
        Failure::Malformed => (
            Code::AuthenticationMalformed,
            Some("Bearer error=\"invalid_request\""),
        ),
        Failure::Invalid => (
            Code::AuthenticationInvalid,
            Some("Bearer error=\"invalid_token\""),
        ),
        Failure::Unavailable => (Code::AuthenticationUnavailable, None),
    };
    let mut response = Problem::new(code)
        .detail(failure.to_string())
        .into_response();
    if let Some(challenge) = challenge {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static(challenge),
        );
    }
    response
}

/// bearer authentication is malformed
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct AuthenticationMalformed(pub Problem);

/// bearer authentication is required or invalid
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct AuthenticationUnauthorized(pub Problem);

/// the verified principal lacks the operation's scopes, or the handler denied it
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct AuthenticationForbidden(pub Problem);

/// bearer authentication trust or provider is unavailable
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct AuthenticationUnavailable(pub Problem);

/// the enclosing request budget expired before a response committed
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct GatewayTimeout(pub Problem);

/// Responses every protected operation declares in addition to its own
/// success shape. 403 is never a verification failure: it answers a
/// verified principal that lacks the operation's scopes, or a handler's
/// own authorization.
#[derive(Debug, IntoResponses)]
pub enum ProtectedOperationProblemResponses {
    /// 400: bearer authentication is malformed.
    #[response(status = 400)]
    Malformed(#[ref_response] AuthenticationMalformed),
    /// 401: bearer authentication is required or invalid.
    #[response(status = 401)]
    Unauthorized(#[ref_response] AuthenticationUnauthorized),
    /// 403: authorization denied the verified principal.
    #[response(status = 403)]
    Forbidden(#[ref_response] AuthenticationForbidden),
    /// 503: authentication trust or provider is unavailable.
    #[response(status = 503)]
    Unavailable(#[ref_response] AuthenticationUnavailable),
    /// 504: the request budget expired before a response committed.
    #[response(status = 504)]
    Timeout(#[ref_response] GatewayTimeout),
}
