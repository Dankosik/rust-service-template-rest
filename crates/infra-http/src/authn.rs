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
use operation_context::{OperationContext, Stopped};
use service_failure::INSUFFICIENT_SCOPE_DETAIL;
use utoipa::{IntoResponses, ToResponse};

use crate::contract::{Access, Policy, layer_routes};
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

pub(crate) fn finalize<S>(contract: Router<S>, policy: Policy, verifier: Verifier) -> Router<S>
where
    S: Send + Sync + Clone + 'static,
{
    layer_routes(contract, |router| {
        router.route_layer(middleware::from_fn_with_state(
            AuthState { verifier, policy },
            authenticate,
        ))
    })
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
    strip_identity(request.extensions_mut());
    let AuthState { verifier, policy } = state;
    let Some(path) = crate::contract::contract_path(request.extensions()) else {
        return wiring_failure("path_missing");
    };
    match policy.access(path, request.method()) {
        Some(Access::Public) => next.run(request).await,
        None => policy.unregistered(Some(path), request.method()).await,
        Some(Access::Protected(alternatives, delegate)) => {
            authenticate_protected(&verifier, alternatives, *delegate, request, next).await
        }
    }
}

pub(crate) fn strip_identity(extensions: &mut Extensions) {
    extensions.remove::<Principal>();
    extensions.remove::<VerifiedPrincipal>();
    extensions.remove::<DelegationCredential>();
}

async fn authenticate_protected(
    verifier: &Verifier,
    alternatives: &[Box<[String]>],
    delegate: bool,
    mut request: Request,
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
            .authenticate_presented_with_context(
                authorization,
                Transport::Http,
                context.operation(),
            )
            .await
    };
    let (principal, delegation) =
        match authorize(context.operation(), result, alternatives, delegate) {
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
    alternatives: &[Box<[String]>],
    delegate: bool,
) -> Result<(Principal, Option<DelegationCredential>), Response> {
    if let Some(stopped) = operation.stopped() {
        return Err(stopped_response(stopped));
    }
    let (principal, token) = result.map_err(failure_response)?;
    if !alternatives
        .iter()
        .any(|scopes| principal.has_scopes(scopes))
    {
        return Err(insufficient_scope_response());
    }
    if let Some(stopped) = operation.stopped() {
        return Err(stopped_response(stopped));
    }
    Ok((
        principal,
        delegate.then(|| token.into_delegation_credential()),
    ))
}

fn stopped_response(stopped: Stopped) -> Response {
    match stopped {
        Stopped::Deadline => crate::context::budget_expired_response(),
        Stopped::Cancelled => crate::context::cancelled_response(),
    }
}

#[derive(Clone, Copy)]
pub(super) struct InsufficientScope;

fn insufficient_scope_response() -> Response {
    let mut response = Problem::new(Code::Forbidden)
        .detail(INSUFFICIENT_SCOPE_DETAIL)
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

        let response = authorize(&context, Err(Failure::Unavailable), &[], false).unwrap_err();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(!response.headers().contains_key(WWW_AUTHENTICATE));
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        let problem = body_json(response).await;
        assert_eq!(problem["code"], "service_unavailable");
    }
}
