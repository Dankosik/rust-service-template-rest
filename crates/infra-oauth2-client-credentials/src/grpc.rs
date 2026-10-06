//! Concrete gRPC binding. The credential and cache remain in their private owner.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use http::{HeaderName, Request, Response, StatusCode, header::AUTHORIZATION};
use infra_grpc::Client;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{AcquisitionError, Credentials, OnBehalfOf, acquisition_deadline};

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
        let authorization_conflict = request.headers().contains_key(AUTHORIZATION);
        let on_behalf_of = request.extensions_mut().remove::<OnBehalfOf>();
        let mut prepared = self.resource.prepare_call(request);
        let context = prepared.opening_context().clone();
        if let Err(error) = self
            .credentials
            .check_lifecycle(acquisition_deadline(&context))
        {
            return Box::pin(std::future::ready(Err(acquisition_status(error))));
        }
        // Both refusals are this service's own composition mistakes. They are
        // `INTERNAL`, as gRFC A54 has a channel report failed call credentials:
        // a code reserved for the application would blame the inbound caller
        // when a handler forwards the status.
        if authorization_conflict {
            return Box::pin(std::future::ready(Err(Status::internal(
                "authorization conflicts with client credentials",
            ))));
        }
        if self.subject_required && on_behalf_of.is_none() {
            return Box::pin(std::future::ready(Err(Status::internal(
                "on-behalf-of subject is required",
            ))));
        }
        let credentials = self.credentials.clone();
        Box::pin(async move {
            let acquired = credentials
                .authorize(prepared.headers_mut(), on_behalf_of, &context)
                .await
                .map_err(acquisition_status)?;
            let response = prepared.send().await?;
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
