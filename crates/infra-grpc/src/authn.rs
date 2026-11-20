use axum::extract::Request;
use http::header::AUTHORIZATION;
use infra_bearerauthn::{
    DelegationCredential, Failure as AuthFailure, Principal, Transport, Verifier,
};
use operation_context::OperationContext;

use crate::Failure;
use crate::call::check_open;
use crate::router::{Rejection, reject_with_budget};

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

pub(crate) fn strip_identity(extensions: &mut http::Extensions) {
    extensions.remove::<Principal>();
    extensions.remove::<DelegationCredential>();
}

#[derive(Debug, Default)]
pub(crate) struct ScopeRequirement {
    pub(crate) scopes: Box<[String]>,
    pub(crate) delegate: bool,
}

pub(crate) async fn authenticate(
    verifier: &Verifier,
    requirement: Option<&ScopeRequirement>,
    context: &OperationContext,
    mut request: Request,
) -> Result<Request, Rejection> {
    check_open(context)?;
    let result = {
        let authorization = request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(http::HeaderValue::as_bytes);
        verifier
            .authenticate_presented(context, authorization, Transport::Grpc)
            .await
    };
    check_open(context)?;
    let (principal, token) = match result {
        Ok(presentation) => presentation,
        Err(failure) => {
            return Err(
                reject_with_budget(context, request, authentication_status(failure))
                    .await
                    .into(),
            );
        }
    };
    let Some(requirement) = requirement else {
        return Err(
            reject_with_budget(context, request, tonic::Status::unimplemented(""))
                .await
                .into(),
        );
    };
    if !principal.has_scopes(requirement.scopes.iter()) {
        let status = Failure::new(service_failure::Code::Forbidden).into_status_as(
            tonic::Code::PermissionDenied,
            service_failure::INSUFFICIENT_SCOPE_DETAIL,
        );
        return Err(Rejection::InsufficientScope(
            reject_with_budget(context, request, status).await,
        ));
    }
    check_open(context)?;
    let delegation = requirement
        .delegate
        .then(|| token.into_delegation_credential());
    request.headers_mut().remove(AUTHORIZATION);
    if let Some(credential) = delegation {
        request.extensions_mut().insert(credential);
    }
    request.extensions_mut().insert(principal);
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
