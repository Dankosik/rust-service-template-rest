use axum::extract::Request;
use axum::response::Response;
use foldhash::HashMap;
use http::header::AUTHORIZATION;
use infra_bearerauthn::{
    Actor, ClaimAccessError, DelegationCredential, Failure as AuthFailure, Principal, Transport,
    Verifier,
};
use operation_context::OperationContext;
use serde::Deserialize;

use crate::Failure;
use crate::call::stopped_status;
use crate::router::reject_with_budget;
use crate::status::internal_status;

pub(super) const VERIFIER_SETTINGS: &[&str] = &[
    // template:begin oidc-jwt:grpc-verifier-setting-jwt
    "authn.mode = oidc-jwt",
    // template:end oidc-jwt:grpc-verifier-setting-jwt
    // template:begin oidc-introspection:grpc-verifier-setting-introspection
    "authn.mode = oidc-introspection",
    // template:end oidc-introspection:grpc-verifier-setting-introspection
    // template:begin service-auth:grpc-verifier-setting-service-tokens
    "service_auth.mode = jwt",
    // template:end service-auth:grpc-verifier-setting-service-tokens
];

#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(Principal);

impl VerifiedPrincipal {
    pub fn require_from_request<T>(request: &tonic::Request<T>) -> Result<&Self, tonic::Status> {
        Self::from_request(request).ok_or_else(internal_status)
    }

    #[must_use]
    pub fn from_request<T>(request: &tonic::Request<T>) -> Option<&Self> {
        Self::from_extensions(request.extensions())
    }

    #[must_use]
    pub fn from_extensions(extensions: &http::Extensions) -> Option<&Self> {
        extensions.get::<Self>()
    }

    pub fn claims<'a, T: Deserialize<'a>>(&'a self) -> Result<T, ClaimAccessError> {
        self.0.claims()
    }

    #[must_use]
    pub fn actor(&self) -> Option<&Actor> {
        self.0.actor()
    }

    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.0.expires_at()
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.0.scopes()
    }

    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.0.client_id()
    }

    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.0.subject()
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        self.0.issuer()
    }
}

pub(crate) fn strip_identity(extensions: &mut http::Extensions) {
    extensions.remove::<VerifiedPrincipal>();
    extensions.remove::<Principal>();
    extensions.remove::<DelegationCredential>();
}

#[derive(Debug, Default)]
pub(crate) struct ScopeRequirement {
    pub(crate) scopes: Box<[String]>,
    pub(crate) delegate: bool,
}

pub(crate) type Scopes = HashMap<String, ScopeRequirement>;

#[derive(Clone)]
pub(crate) struct InsufficientScope;

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the call returns"
)]
pub(crate) async fn authenticate(
    verifier: &Verifier,
    scopes: &Scopes,
    context: &OperationContext,
    mut request: Request,
) -> Result<Request, Response> {
    if let Some(reason) = context.stopped() {
        return Err(stopped_status(reason).into_http());
    }
    let result = {
        let authorization = request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(http::HeaderValue::as_bytes);
        verifier
            .authenticate_presented_with_context(authorization, Transport::Grpc, context)
            .await
    };
    if let Some(reason) = context.stopped() {
        return Err(stopped_status(reason).into_http());
    }
    let (principal, token) = match result {
        Ok(presentation) => presentation,
        Err(failure) => {
            return Err(reject_with_budget(context, request, authentication_status(failure)).await);
        }
    };
    let Some(requirement) = scopes.get(request.uri().path()) else {
        return Err(reject_with_budget(context, request, tonic::Status::unimplemented("")).await);
    };
    if !principal.has_scopes(&requirement.scopes) {
        let status = Failure::new(service_failure::Code::Forbidden).into_status_as(
            tonic::Code::PermissionDenied,
            service_failure::INSUFFICIENT_SCOPE_DETAIL,
        );
        let mut response = reject_with_budget(context, request, status).await;
        response.extensions_mut().insert(InsufficientScope);
        return Err(response);
    }
    if let Some(reason) = context.stopped() {
        return Err(stopped_status(reason).into_http());
    }
    let delegation = requirement
        .delegate
        .then(|| token.into_delegation_credential());
    request.headers_mut().remove(AUTHORIZATION);
    if let Some(credential) = delegation {
        request.extensions_mut().insert(credential);
    }
    request
        .extensions_mut()
        .insert(VerifiedPrincipal(principal));
    Ok(request)
}

fn authentication_status(failure: AuthFailure) -> tonic::Status {
    Failure::new(match failure {
        AuthFailure::Missing => service_failure::Code::AuthenticationRequired,
        AuthFailure::Malformed => service_failure::Code::AuthenticationMalformed,
        AuthFailure::Invalid => service_failure::Code::AuthenticationInvalid,
        AuthFailure::Unavailable => service_failure::Code::AuthenticationUnavailable,
    })
    .into()
}
