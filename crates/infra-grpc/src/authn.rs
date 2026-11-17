use axum::extract::Request;
use axum::response::Response;
use foldhash::HashMap;
use http::header::AUTHORIZATION;
use infra_bearerauthn::{
    Actor, ClaimAccessError, Failure as AuthFailure, Principal, Transport, Verifier,
};
use operation_context::OperationContext;
use secrecy::SecretString;
use serde::de::DeserializeOwned;

use crate::Failure;
use crate::call::server_stopped;
use crate::router::reject_with_budget;

#[derive(Clone, Debug)]
pub struct VerifiedPrincipal(Principal);

impl VerifiedPrincipal {
    #[must_use]
    pub fn from_request<T>(request: &tonic::Request<T>) -> Option<&Self> {
        Self::from_extensions(request.extensions())
    }

    #[must_use]
    pub fn from_extensions(extensions: &http::Extensions) -> Option<&Self> {
        extensions.get::<Self>()
    }

    pub fn claims<T: DeserializeOwned>(&self) -> Result<T, ClaimAccessError> {
        self.0.claims()
    }

    #[must_use]
    pub fn actor(&self) -> Option<&Actor> {
        self.0.actor()
    }

    #[must_use]
    pub fn access_token(&self) -> &SecretString {
        self.0.access_token()
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
}

pub(crate) type Scopes = HashMap<String, Box<[String]>>;

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
        return Err(server_stopped(reason).into_http());
    }
    let result = {
        let authorization = request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(http::HeaderValue::as_bytes);
        verifier
            .authenticate_with_context(authorization, Transport::Grpc, context)
            .await
    };
    if let Some(reason) = context.stopped() {
        return Err(server_stopped(reason).into_http());
    }
    let principal = match result {
        Ok(principal) => principal,
        Err(failure) => {
            return Err(reject_with_budget(context, request, authentication_status(failure)).await);
        }
    };
    let granted = scopes
        .get(request.uri().path())
        .is_none_or(|required| principal.has_scopes(required));
    if !granted {
        let status = Failure::new(service_failure::Code::Forbidden).into_status_as(
            tonic::Code::PermissionDenied,
            service_failure::INSUFFICIENT_SCOPE_DETAIL,
        );
        return Err(reject_with_budget(context, request, status).await);
    }
    if let Some(reason) = context.stopped() {
        return Err(server_stopped(reason).into_http());
    }
    request.headers_mut().remove(AUTHORIZATION);
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
