use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::Extensions;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::{Failure, Principal, Transport, Verifier};
use operation_context::{OperationContext, Stopped};
use service_failure::INSUFFICIENT_SCOPE_DETAIL;
use utoipa::{IntoResponses, ToResponse};
use utoipa_axum::router::OpenApiRouter;

use crate::contract::{Access, FinalizeError, Policy, layer_routes};
pub use crate::problem::responses::GatewayTimeout;
use crate::problem::{Code, Problem, sanitized_internal_error};

#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(Principal);

impl VerifiedPrincipal {
    pub(crate) fn from_extensions(extensions: &Extensions) -> Option<Self> {
        extensions.get::<Principal>().cloned().map(Self)
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        self.0.issuer()
    }

    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.0.subject()
    }

    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.0.client_id()
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.0.scopes()
    }

    pub fn claims<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<T, infra_bearerauthn::ClaimAccessError> {
        self.0.claims()
    }

    #[must_use]
    pub fn expires_at(&self) -> u64 {
        self.0.expires_at()
    }

    #[must_use]
    pub fn access_token(&self) -> &secrecy::SecretString {
        self.0.access_token()
    }

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
        std::future::ready(
            Self::from_extensions(&parts.extensions)
                .ok_or_else(crate::context::missing_extension::<Self>),
        )
    }
}

pub(crate) fn finalize<S>(
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
    request: Request,
    next: Next,
) -> Response {
    let Some(context) = request.extensions().get::<crate::RequestContext>().cloned() else {
        return wiring_failure("context_missing");
    };
    let result = {
        let authorization = request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(axum::http::HeaderValue::as_bytes);
        verifier
            .authenticate_with_context(authorization, Transport::Http, context.operation())
            .await
    };
    settle_protected(context.operation(), result, alternatives, request, next).await
}

async fn settle_protected(
    operation: &OperationContext,
    result: Result<Principal, Failure>,
    alternatives: &[Box<[String]>],
    mut request: Request,
    next: Next,
) -> Response {
    if let Some(stopped) = operation.stopped() {
        return stopped_response(stopped);
    }
    let principal = match result {
        Ok(principal) => principal,
        Err(failure) => return failure_response(failure),
    };
    if !grants_any(alternatives, &principal) {
        return insufficient_scope_response();
    }
    if let Some(stopped) = operation.stopped() {
        return stopped_response(stopped);
    }
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(principal);
    next.run(request).await
}

fn stopped_response(stopped: Stopped) -> Response {
    match stopped {
        Stopped::Deadline => crate::context::budget_expired_response(),
        Stopped::Cancelled => crate::context::cancelled_response(),
    }
}

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

/// Responses every protected operation declares in addition to its own
/// success shape. 403 is never a verification failure: it answers a
/// verified principal that lacks the operation's scopes, or a handler's
/// own authorization.
#[derive(Debug, IntoResponses)]
pub enum ProtectedOperationProblemResponses {
    #[response(status = 400)]
    Malformed(#[ref_response] AuthenticationMalformed),
    #[response(status = 401)]
    Unauthorized(#[ref_response] AuthenticationUnauthorized),
    #[response(status = 403)]
    Forbidden(#[ref_response] AuthenticationForbidden),
    #[response(status = 503)]
    Unavailable(#[ref_response] AuthenticationUnavailable),
    #[response(status = 504)]
    Timeout(#[ref_response] GatewayTimeout),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::http::header::CONTENT_TYPE;
    use tower::ServiceExt as _;

    use super::*;
    use crate::test_support::body_json;

    #[tokio::test]
    async fn cancelled_operation_context_answers_service_unavailable_not_authentication_unavailable()
     {
        let reached_handler = Arc::new(AtomicBool::new(false));
        let handler_flag = Arc::clone(&reached_handler);
        let context = OperationContext::unbounded();
        context.cancellation().cancel();

        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = Router::new()
            .route(
                "/",
                axum::routing::get(move || async move {
                    handler_flag.store(true, Ordering::SeqCst);
                    StatusCode::OK
                }),
            )
            .layer(middleware::from_fn(move |request, next| {
                let context = context.clone();
                async move {
                    settle_protected(&context, Err(Failure::Unavailable), &[], request, next).await
                }
            }));

        let response = app.oneshot(Request::new(Body::empty())).await.unwrap();
        assert!(!reached_handler.load(Ordering::SeqCst));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key(WWW_AUTHENTICATE));
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        let problem = body_json(response).await;
        assert_eq!(problem["code"], "service_unavailable");
    }
}
