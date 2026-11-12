//! Bearer authentication of business calls and their declared scopes.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::Failure;
use crate::call::Side;
use crate::router::{opening_context, reject};

pub(crate) async fn authenticate(
    verifier: infra_bearerauthn::Verifier,
    scopes: Arc<HashMap<String, Box<[String]>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let authorization = request
        .headers()
        .get_all(http::header::AUTHORIZATION)
        .iter()
        .map(http::HeaderValue::as_bytes);
    let Some(context) = opening_context(&request).cloned() else {
        return crate::status::internal_status().into_http();
    };
    let result = verifier
        .authenticate_with_context(authorization, infra_bearerauthn::Transport::Grpc, &context)
        .await;
    if let Some(reason) = context.stopped() {
        return Side::Server.stopped(reason).into_http();
    }
    let principal = match result {
        Ok(principal) => principal,
        Err(failure) => return reject(request, authentication_status(failure)).await,
    };
    let granted = scopes
        .get(request.uri().path())
        .is_none_or(|required| principal.has_scopes(required));
    if !granted {
        let status = Failure::new(service_failure::Code::Forbidden).into_status_as(
            tonic::Code::PermissionDenied,
            service_failure::INSUFFICIENT_SCOPE_DETAIL,
        );
        return reject(request, status).await;
    }
    if let Some(reason) = context.stopped() {
        return Side::Server.stopped(reason).into_http();
    }
    request.extensions_mut().insert(principal);
    request.headers_mut().remove(http::header::AUTHORIZATION);
    next.run(request).await
}

fn authentication_status(failure: infra_bearerauthn::Failure) -> tonic::Status {
    Failure::new(match failure {
        infra_bearerauthn::Failure::Missing => service_failure::Code::AuthenticationRequired,
        infra_bearerauthn::Failure::Malformed => service_failure::Code::AuthenticationMalformed,
        infra_bearerauthn::Failure::Invalid => service_failure::Code::AuthenticationInvalid,
        infra_bearerauthn::Failure::Unavailable => service_failure::Code::AuthenticationUnavailable,
    })
    .into()
}
