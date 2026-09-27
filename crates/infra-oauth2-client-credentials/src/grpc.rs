//! Concrete gRPC binding. The credential and cache remain in their private owner.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::{Request, Response, StatusCode, header::AUTHORIZATION};
use infra_grpc::Client;
use tokio::time::Instant;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{AcquisitionError, Credentials, FETCH_TIMEOUT};

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
            let deadline = Instant::now()
                + infra_grpc::grpc_timeout(request.headers()).unwrap_or(FETCH_TIMEOUT);
            let value = credentials
                .acquire(deadline)
                .await
                .map_err(acquisition_status)?;
            if Instant::now() >= deadline {
                return Err(Status::deadline_exceeded("request deadline exceeded"));
            }
            if value
                .hard_expiry
                .is_some_and(|expiry| Instant::now() >= expiry)
            {
                return Err(acquisition_status(AcquisitionError::Timeout));
            }
            // The cache owner validates the bearer grammar and marks this value sensitive.
            request
                .headers_mut()
                .insert(AUTHORIZATION, value.header.clone());
            let response = resource.call(request).await?;
            let unauthenticated =
                response.headers().get("grpc-status").is_some_and(|status| {
                    Code::from_bytes(status.as_bytes()) == Code::Unauthenticated
                }) || (response.status() == StatusCode::UNAUTHORIZED
                    && !response.headers().contains_key("grpc-status"));
            if unauthenticated {
                let _ = tokio::time::timeout_at(deadline, credentials.invalidate(&value)).await;
            }
            Ok(response)
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
