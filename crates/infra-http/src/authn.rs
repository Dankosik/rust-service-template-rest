//! Final contract-driven inbound bearer authentication.

use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_bearerauthn::{Failure, Principal, Verifier};
use utoipa_axum::router::OpenApiRouter;

use crate::contract::{Access, FinalizeError, Policy};
use crate::problem::{Code, Problem, sanitized_internal_error};

const AUTHENTICATION_REQUIRED_DETAIL: &str = "bearer authentication is required";
const AUTHENTICATION_MALFORMED_DETAIL: &str = "bearer authentication is malformed";
const AUTHENTICATION_INVALID_DETAIL: &str = "bearer authentication is invalid";
const AUTHENTICATION_UNAVAILABLE_DETAIL: &str = "bearer authentication is unavailable";
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

    /// Deserialize application claims from the same accepted verification evidence.
    ///
    /// # Errors
    /// Returns a sanitized typed error if the requested claim shape does not match.
    pub fn claims<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<T, infra_bearerauthn::ClaimAccessError> {
        self.0.claims()
    }

    /// The verified `exp` claim in Unix epoch seconds.
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

    /// The current RFC 8693 actor, when the verified evidence names one.
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
            parts
                .extensions
                .get::<Principal>()
                .cloned()
                .map(Self)
                .ok_or_else(sanitized_internal_error),
        )
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
    let (router, _) = contract.split_for_parts();
    if !router.has_routes() {
        return Ok(router);
    }
    Ok(router.route_layer(middleware::from_fn_with_state(
        AuthState { verifier, policy },
        authenticate,
    )))
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
    request.extensions_mut().remove::<VerifiedPrincipal>();
    let Some(path) = crate::contract::contract_path(request.extensions()) else {
        return wiring_failure();
    };
    match state.policy.access(path, request.method()).cloned() {
        Some(Access::Public) => next.run(request).await,
        None => wiring_failure(),
        Some(Access::Protected(alternatives)) => {
            authenticate_protected(state.verifier, alternatives, request, next).await
        }
    }
}

async fn authenticate_protected(
    verifier: Verifier,
    alternatives: Arc<[Box<[String]>]>,
    mut request: Request,
    next: Next,
) -> Response {
    let authorization = request
        .headers()
        .get_all(AUTHORIZATION)
        .iter()
        .map(axum::http::HeaderValue::as_bytes);
    let principal = match verifier.authenticate(authorization, "http").await {
        Ok(principal) => principal,
        Err(failure) => return failure_response(failure),
    };
    if !grants_any(&alternatives, &principal) {
        return insufficient_scope_response();
    }
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert(principal);
    next.run(request).await
}

/// At least one alternative's scopes must all be present in the sorted,
/// deduplicated `principal.scopes()`; an alternative with no scopes is any
/// authenticated caller.
fn grants_any(alternatives: &[Box<[String]>], principal: &Principal) -> bool {
    alternatives.iter().any(|scopes| {
        scopes
            .iter()
            .all(|scope| principal.scopes().binary_search(scope).is_ok())
    })
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

fn wiring_failure() -> Response {
    sanitized_internal_error()
}

fn failure_response(failure: Failure) -> Response {
    let (code, detail, challenge) = match failure {
        Failure::Missing => (
            Code::AuthenticationRequired,
            AUTHENTICATION_REQUIRED_DETAIL,
            Some("Bearer"),
        ),
        Failure::Malformed => (
            Code::AuthenticationMalformed,
            AUTHENTICATION_MALFORMED_DETAIL,
            Some("Bearer error=\"invalid_request\""),
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
    };
    let mut response = Problem::new(code).detail(detail).into_response();
    if let Some(challenge) = challenge {
        response.headers_mut().insert(
            WWW_AUTHENTICATE,
            axum::http::HeaderValue::from_static(challenge),
        );
    }
    response
}
