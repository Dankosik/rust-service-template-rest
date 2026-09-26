//! Concrete gRPC binding. The credential and cache remain in their private owner.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use http::{Request, Response, StatusCode, header::AUTHORIZATION};
use http_body_util::BodyExt as _;
use infra_grpc::{Client, Operation};
use tokio::{sync::oneshot, time::Instant};
use tonic::{Code, Status, body::Body};
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
                return Err(acquisition_status(AcquisitionError::Timeout));
            }
            // The cache owner validates the bearer grammar and marks this value sensitive.
            request
                .headers_mut()
                .insert(AUTHORIZATION, value.header.clone());
            let response = resource.call(request).await?;
            if let Some(status) = response.headers().get("grpc-status") {
                if Code::from_bytes(status.as_bytes()) == Code::Unauthenticated {
                    let _ =
                        tokio::time::timeout_at(operation.deadline, credentials.invalidate(&value))
                            .await;
                }
                return Ok(response);
            }

            let (parts, body) = response.into_parts();
            let http_status = parts.status;
            let (sender, receiver) = oneshot::channel();
            let mut sender = Some(sender);
            let body = body
                .inspect_frame(move |frame| {
                    if let Some(trailers) = frame.trailers_ref()
                        && let Some(sender) = sender.take()
                    {
                        let status = trailers
                            .get("grpc-status")
                            .map(|status| Code::from_bytes(status.as_bytes()));
                        let _ = sender.send(status);
                    }
                })
                // Drop the observer at EOF/error so status-less completion closes the channel.
                .fuse()
                .with_trailers(async move {
                    let rejected = match receiver.await {
                        Ok(Some(code)) => code == Code::Unauthenticated,
                        Ok(None) | Err(_) => http_status == StatusCode::UNAUTHORIZED,
                    };
                    if rejected {
                        let _ = tokio::time::timeout_at(
                            operation.deadline,
                            credentials.invalidate(&value),
                        )
                        .await;
                    }
                    None
                });
            Ok(Response::from_parts(parts, Body::new(body)))
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
