use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::Extensions;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::{
    DelegationCredential, Failure, PresentedToken, Principal, Transport, Verifier,
};
use operation_context::OperationContext;
use service_failure::INSUFFICIENT_SCOPE_DETAIL;
use utoipa::{IntoResponses, ToResponse};

use crate::contract::{Access, Policy, layer_routes};
pub use crate::problem::responses::GatewayTimeout;
use crate::problem::{Code, Problem, sanitized_internal_error};

#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(pub Principal);

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
                .ok_or_else(crate::context::missing_extension::<Self>),
        )
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedDelegation(pub DelegationCredential);

impl<S> FromRequestParts<S> for VerifiedDelegation
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
                .get::<DelegationCredential>()
                .cloned()
                .map(Self)
                .ok_or_else(crate::context::missing_extension::<Self>),
        )
    }
}

pub(crate) fn finalize<S>(contract: Router<S>, policy: Policy, verifier: Verifier) -> Router<S>
where
    S: Send + Sync + Clone + 'static,
{
    layer_routes(contract, |router| {
        router.route_layer(middleware::from_fn_with_state(
            Arc::new(AuthState { verifier, policy }),
            authenticate,
        ))
    })
}

pub(crate) struct ProtectedAccess {
    pub(crate) scope_alternatives: Box<[Box<[String]>]>,
    pub(crate) delegate: bool,
}

struct AuthState {
    verifier: Verifier,
    policy: Policy,
}

async fn authenticate(
    State(state): State<Arc<AuthState>>,
    mut request: Request,
    next: Next,
) -> Response {
    strip_identity(request.extensions_mut());
    let AuthState { verifier, policy } = state.as_ref();
    let Some(path) = crate::contract::contract_path(request.extensions()) else {
        return wiring_failure("path_missing");
    };
    match policy.access(path, request.method()) {
        Some(Access::Public) => next.run(request).await,
        None => {
            policy
                .unregistered_response(Some(path), request.method())
                .await
        }
        Some(Access::Protected(required)) => {
            authenticate_protected(verifier, required, request, next).await
        }
    }
}

pub(crate) fn strip_identity(extensions: &mut Extensions) {
    extensions.remove::<Principal>();
    extensions.remove::<DelegationCredential>();
}

async fn authenticate_protected(
    verifier: &Verifier,
    required: &ProtectedAccess,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(context) = request.extensions().get::<crate::RequestContext>() else {
        return wiring_failure("context_missing");
    };
    let result = {
        let authorization = request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(axum::http::HeaderValue::as_bytes);
        verifier
            .authenticate_presented(context.operation(), authorization, Transport::Http)
            .await
    };
    let (principal, delegation) = match authorize(context.operation(), result, required) {
        Ok(admitted) => admitted,
        Err(response) => return response,
    };
    request.headers_mut().remove(AUTHORIZATION);
    if let Some(credential) = delegation {
        request.extensions_mut().insert(credential);
    }
    request.extensions_mut().insert(principal);
    next.run(request).await
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response returned by the route"
)]
fn authorize(
    operation: &OperationContext,
    result: Result<(Principal, PresentedToken<'_>), Failure>,
    required: &ProtectedAccess,
) -> Result<(Principal, Option<DelegationCredential>), Response> {
    if let Some(stopped) = operation.stopped() {
        return Err(Problem::from(stopped).into_response());
    }
    let (principal, token) = result.map_err(failure_response)?;
    if !required
        .scope_alternatives
        .iter()
        .any(|scopes| principal.has_scopes(scopes.iter()))
    {
        return Err(insufficient_scope_response());
    }
    Ok((
        principal,
        required
            .delegate
            .then(|| token.into_delegation_credential()),
    ))
}

#[derive(Clone, Copy)]
pub(super) struct InsufficientScope;

fn insufficient_scope_response() -> Response {
    let mut response = Problem::new(Code::Forbidden)
        .detail_static(INSUFFICIENT_SCOPE_DETAIL)
        .into_response();
    response.extensions_mut().insert(InsufficientScope);
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
        Failure::Missing => (Code::AuthenticationRequired, Some("Bearer realm=\"api\"")),
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
        .detail_static(failure.as_str())
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
#[response(content_type = "application/problem+json", headers(("WWW-Authenticate" = String, description = "Bearer challenge with invalid_request when the Authorization header is malformed")))]
pub struct AuthenticationMalformed(pub Problem);

/// bearer authentication is required or invalid
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json", headers(("WWW-Authenticate" = String, description = "Bearer challenge for missing credentials or invalid_token for an invalid token")))]
pub struct AuthenticationUnauthorized(pub Problem);

/// the verified principal lacks the operation's scopes, or the handler denied it
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json", headers(("WWW-Authenticate" = String, description = "Bearer challenge with insufficient_scope when the verified principal lacks the required scopes")))]
pub struct AuthenticationForbidden(pub Problem);

/// bearer authentication trust or provider is unavailable
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct AuthenticationUnavailable(pub Problem);

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
    use axum::http::StatusCode;
    use axum::http::header::CONTENT_TYPE;

    use super::*;
    use crate::test_support::body_json;

    #[tokio::test]
    async fn cancelled_operation_context_answers_service_unavailable_not_authentication_unavailable()
     {
        let context = OperationContext::unbounded();
        context.cancellation().cancel();

        let required = ProtectedAccess {
            scope_alternatives: Box::new([]),
            delegate: false,
        };
        let response = authorize(&context, Err(Failure::Unavailable), &required).unwrap_err();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key(WWW_AUTHENTICATE));
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        let problem = body_json(response).await;
        assert_eq!(problem["code"], "service_unavailable");
    }

    #[tokio::test]
    async fn failure_responses_match_the_canonical_failure_details() {
        for failure in [
            Failure::Missing,
            Failure::Malformed,
            Failure::Invalid,
            Failure::Unavailable,
        ] {
            let problem = body_json(failure_response(failure)).await;
            assert_eq!(problem["detail"], failure.to_string());
        }
    }

    #[tokio::test]
    async fn missing_delegation_credential_is_a_sanitized_internal_failure() {
        use axum::body::Body;
        use axum::routing::get;
        use tower::ServiceExt as _;

        let handler = |_: VerifiedDelegation| async { "unreachable" };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = Router::new().route("/", get(handler));
        let response = app.oneshot(Request::new(Body::empty())).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
    }
}
