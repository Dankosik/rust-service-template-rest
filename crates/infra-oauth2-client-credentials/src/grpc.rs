//! Concrete gRPC binding. The credential and cache remain in their private owner.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::{Request, Response, header::AUTHORIZATION};
use infra_grpc::{Client, Operation};
use tokio::time::Instant;
use tonic::{Status, body::Body};
use tower::Service;

use crate::{AcquisitionError, Credentials};

/// A cloneable governed gRPC client with private machine credentials.
#[derive(Clone)]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
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
        }
    }
}

impl Service<Request<Body>> for AuthenticatedClient {
    type Response = Response<Body>;
    type Error = Status;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // The operation, including resource readiness, spends its absolute deadline.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let credentials = self.credentials.clone();
        let mut resource = self.resource.clone();
        Box::pin(async move {
            if request.headers().contains_key(AUTHORIZATION) {
                return Err(Status::invalid_argument(
                    "authorization conflicts with client credentials",
                ));
            }
            let operation = request
                .extensions()
                .get::<Operation>()
                .copied()
                .ok_or_else(|| Status::invalid_argument("operation deadline is required"))?;
            if Instant::now() >= operation.deadline {
                return Err(Status::deadline_exceeded("request deadline exceeded"));
            }
            let value = credentials
                .acquire(operation.deadline)
                .await
                .map_err(acquisition_status)?;
            if Instant::now() >= operation.deadline {
                return Err(Status::deadline_exceeded("request deadline exceeded"));
            }
            if value
                .hard_expiry
                .is_some_and(|expiry| Instant::now() >= expiry)
            {
                return Err(Status::unavailable("client credentials unavailable"));
            }
            // The cache owner validates the bearer grammar and marks this value sensitive.
            request
                .headers_mut()
                .insert(AUTHORIZATION, value.header.clone());
            resource.call(request).await
        })
    }
}

fn acquisition_status(error: AcquisitionError) -> Status {
    match error {
        AcquisitionError::Timeout => Status::deadline_exceeded("request deadline exceeded"),
        AcquisitionError::Transport
        | AcquisitionError::ResponseLimit
        | AcquisitionError::Rejected
        | AcquisitionError::InvalidResponse => {
            Status::unavailable("client credentials unavailable")
        }
    }
}
