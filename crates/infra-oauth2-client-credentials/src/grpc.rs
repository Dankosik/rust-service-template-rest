//! Concrete gRPC binding. The credential and cache remain in their private owner.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use http::{HeaderName, HeaderValue, Request, Response, StatusCode, header::AUTHORIZATION};
use infra_grpc::Client;
use tokio::time::Instant;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{AcquisitionError, Credentials, FETCH_TIMEOUT, OnBehalfOf};

const GRPC_TIMEOUT: HeaderName = HeaderName::from_static("grpc-timeout");
const GRPC_STATUS: HeaderName = HeaderName::from_static("grpc-status");

/// A cloneable governed gRPC client with private machine credentials.
#[derive(Clone)]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    subject_required: bool,
}

impl AuthenticatedClient {
    /// Refuses a call without [`OnBehalfOf`] instead of sending the service
    /// token. For an integration that only ever acts for a verified user, so
    /// that a forgotten subject is an error and never a call made with the
    /// service's own authority.
    #[must_use]
    pub fn require_on_behalf_of(mut self) -> Self {
        self.subject_required = true;
        self
    }
}

impl std::fmt::Debug for AuthenticatedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthenticatedGrpcClient([REDACTED])")
    }
}

impl Credentials {
    /// Binds private credentials to a reusable, lazy governed gRPC client.
    #[must_use]
    pub fn grpc(&self, resource: Client) -> AuthenticatedClient {
        AuthenticatedClient {
            credentials: self.clone(),
            resource,
            subject_required: false,
        }
    }
}

impl Service<Request<Body>> for AuthenticatedClient {
    type Response = Response<Body>;
    type Error = Status;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        // Both refusals are this service's own composition mistakes. They are
        // `INTERNAL`, as gRFC A54 has a channel report failed call credentials:
        // a code reserved for the application would blame the inbound caller
        // when a handler forwards the status.
        if request.headers().contains_key(AUTHORIZATION) {
            return Box::pin(std::future::ready(Err(Status::internal(
                "authorization conflicts with client credentials",
            ))));
        }
        let on_behalf_of = request.extensions_mut().remove::<OnBehalfOf>();
        if self.subject_required && on_behalf_of.is_none() {
            return Box::pin(std::future::ready(Err(Status::internal(
                "on-behalf-of subject is required",
            ))));
        }
        let budget = infra_grpc::grpc_timeout(request.headers());
        let started = Instant::now();
        let deadline = started + budget.unwrap_or(FETCH_TIMEOUT);
        let credentials = self.credentials.clone();
        // A reusable service token spends none of the budget, so the resource is
        // called now, without cloning it or rewriting grpc-timeout.
        if on_behalf_of.is_none()
            && started < deadline
            && let Some(token) = credentials.reusable_service_token(started)
        {
            request
                .headers_mut()
                .insert(AUTHORIZATION, token.header.clone());
            let response = self.resource.call(request);
            return Box::pin(async move {
                let response = response.await?;
                if unauthenticated(&response) {
                    credentials.reject_service_token(&token);
                }
                Ok(response)
            });
        }
        let mut resource = self.resource.clone();
        Box::pin(async move {
            let acquired = credentials
                .authorize(request.headers_mut(), on_behalf_of, deadline)
                .await
                .map_err(acquisition_status)?;
            let now = Instant::now();
            if budget.is_some() && now - started >= Duration::from_millis(1) {
                // Propagate what the token wait left of the caller's budget,
                // as gRPC clients do for a context deadline. A shorter wait
                // spends less than the millisecond this header resolves.
                let remaining = deadline.saturating_duration_since(now);
                request
                    .headers_mut()
                    .insert(GRPC_TIMEOUT, grpc_timeout_value(remaining)?);
            }
            let response = resource.call(request).await?;
            if unauthenticated(&response) {
                credentials.reject_acquired(&acquired).await;
            }
            Ok(response)
        })
    }
}

/// Whether the resource reports the dispatched token unauthenticated.
fn unauthenticated(response: &Response<Body>) -> bool {
    response
        .headers()
        .get(GRPC_STATUS)
        .is_some_and(|status| Code::from_bytes(status.as_bytes()) == Code::Unauthenticated)
        || (response.status() == StatusCode::UNAUTHORIZED
            && !response.headers().contains_key(GRPC_STATUS))
}

/// A failure that may pass unchanged later is `UNAVAILABLE`; any other is
/// `UNAUTHENTICATED`, as gRPC clients report credentials that could not
/// produce call metadata. The closed
/// reason stays reachable as the status source and never crosses the wire.
fn acquisition_status(error: AcquisitionError) -> Status {
    let mut status = match error {
        AcquisitionError::Timeout => Status::deadline_exceeded("request deadline exceeded"),
        AcquisitionError::Transport
        | AcquisitionError::Unavailable
        | AcquisitionError::AtCapacity => Status::unavailable("client credentials unavailable"),
        AcquisitionError::ResponseLimit
        | AcquisitionError::Rejected(_)
        | AcquisitionError::InvalidResponse
        | AcquisitionError::Assertion => Status::unauthenticated("client credentials refused"),
    };
    status.set_source(Arc::new(error));
    status
}

/// Encodes whole milliseconds, or whole seconds beyond eight millisecond digits.
fn grpc_timeout_value(remaining: Duration) -> Result<HeaderValue, Status> {
    let millis = remaining.as_millis();
    if millis == 0 {
        return Err(Status::deadline_exceeded("request deadline exceeded"));
    }
    let value = if millis < 100_000_000 {
        format!("{millis}m")
    } else {
        format!("{}S", remaining.as_secs().min(99_999_999))
    };
    HeaderValue::try_from(value).map_err(|_| Status::internal("invalid grpc-timeout"))
}
