//! Concrete gRPC binding. The credential and cache remain in their private owner.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use http::{HeaderValue, Request, Response, StatusCode, header::AUTHORIZATION};
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
            let budget = infra_grpc::grpc_timeout(request.headers());
            let deadline = Instant::now() + budget.unwrap_or(FETCH_TIMEOUT);
            let token = credentials
                .authorize(request.headers_mut(), deadline)
                .await
                .map_err(acquisition_status)?;
            if budget.is_some() {
                // Propagate what the token wait left of the caller's budget,
                // as gRPC clients do for a context deadline.
                let remaining = deadline.saturating_duration_since(Instant::now());
                request
                    .headers_mut()
                    .insert("grpc-timeout", grpc_timeout_value(remaining)?);
            }
            let response = resource.call(request).await?;
            let unauthenticated =
                response.headers().get("grpc-status").is_some_and(|status| {
                    Code::from_bytes(status.as_bytes()) == Code::Unauthenticated
                }) || (response.status() == StatusCode::UNAUTHORIZED
                    && !response.headers().contains_key("grpc-status"));
            if unauthenticated {
                credentials.reject(&token);
            }
            Ok(response)
        })
    }
}

fn acquisition_status(error: AcquisitionError) -> Status {
    match error {
        AcquisitionError::Timeout => Status::deadline_exceeded("request deadline exceeded"),
        _ => Status::unavailable("client credentials unavailable"),
    }
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
