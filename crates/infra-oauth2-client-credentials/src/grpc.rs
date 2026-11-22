use std::{
    sync::Arc,
    task::{Context, Poll},
};

use http::{Request, Response, header::AUTHORIZATION};
use infra_outbound_grpc::{Client, ResponseFuture, WithContext, transport::PreparedCall};
use operation_context::OperationContext;
use tokio::time::Instant;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{
    AcquisitionError, ConfigurationError, ExchangeTokens, OnBehalfOf, ServiceTokens,
    credentials::acquisition_deadline, http_client::TokenSource,
};

#[derive(Clone)]
enum BoundTokens {
    Service(ServiceTokens),
    Exchange(ExchangeTokens, Arc<OnBehalfOf>),
}

impl BoundTokens {
    fn source(&self) -> TokenSource<'_> {
        match self {
            Self::Service(tokens) => TokenSource::Service(tokens),
            Self::Exchange(tokens, subject) => TokenSource::Exchange(tokens, subject),
        }
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("grpc::ExchangeClient([REDACTED])")]
pub struct ExchangeClient {
    tokens: ExchangeTokens,
    resource: Client,
}

impl ExchangeClient {
    #[must_use]
    pub fn on_behalf_of(&self, subject: OnBehalfOf) -> AuthenticatedClient {
        AuthenticatedClient {
            tokens: BoundTokens::Exchange(self.tokens.clone(), Arc::new(subject)),
            resource: self.resource.clone(),
        }
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("grpc::AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    tokens: BoundTokens,
    resource: Client,
}

impl ServiceTokens {
    pub fn grpc(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        validate_resource(&resource, self.resource_origin())?;
        Ok(AuthenticatedClient {
            tokens: BoundTokens::Service(self.clone()),
            resource,
        })
    }
}

impl ExchangeTokens {
    pub fn grpc(&self, resource: Client) -> Result<ExchangeClient, ConfigurationError> {
        validate_resource(&resource, self.resource_origin())?;
        Ok(ExchangeClient {
            tokens: self.clone(),
            resource,
        })
    }
}

fn validate_resource(resource: &Client, origin: &http::Uri) -> Result<(), ConfigurationError> {
    if !resource.uses_tls() {
        return Err(ConfigurationError {
            key: "grpc_resource",
            reason: "must use TLS to carry OAuth2 bearer tokens",
        });
    }
    if !resource.matches_origin(origin) {
        return Err(ConfigurationError::resource_origin_mismatch());
    }
    Ok(())
}

impl AuthenticatedClient {
    #[must_use]
    pub fn with_context(&self, context: &OperationContext) -> WithContext<Self> {
        WithContext::new(self.clone(), context)
    }

    fn prepare(&self, request: Request<Body>) -> Result<PreparedCall, Status> {
        PreparedCall::prepare(&self.resource, request)
            .map_err(status_at_prepare)?
            .try_step(|opening, headers| {
                self.tokens
                    .source()
                    .check_active(acquisition_deadline(opening))
                    .map_err(acquisition_status)?;
                if headers.contains_key(AUTHORIZATION) {
                    return Err(Status::internal(
                        "authorization conflicts with client credentials",
                    ));
                }
                Ok(())
            })
    }
}

fn status_at_prepare(status: Status) -> Status {
    match status.code() {
        Code::Cancelled => acquisition_status(AcquisitionError::Cancelled),
        Code::DeadlineExceeded => acquisition_status(AcquisitionError::Timeout),
        _ => status,
    }
}

async fn send_admitted(
    prepared: PreparedCall,
    tokens: BoundTokens,
) -> Result<Response<Body>, Status> {
    let (prepared, token) = prepared
        .try_await(async move |opening| {
            tokens
                .source()
                .acquire_for_request(acquisition_deadline(opening))
                .await
                .map_err(acquisition_status)
        })
        .await?;
    let used = Arc::downgrade(&token);
    prepared
        .on_status(move |code| {
            if code == Code::Unauthenticated
                && let Some(token) = used.upgrade()
            {
                token.reject_if_old_enough();
            }
        })
        .send_with_dispatch(move |headers| {
            token
                .check_expiry(Instant::now())
                .map_err(acquisition_status)?;
            headers.insert(AUTHORIZATION, token.header.clone());
            Ok(())
        })
        .await
}

impl Service<Request<Body>> for AuthenticatedClient {
    type Response = Response<Body>;
    type Error = Status;
    type Future = ResponseFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        ResponseFuture::from_prepared(self.prepare(request), |prepared| {
            send_admitted(prepared, self.tokens.clone())
        })
    }
}

/// gRFC A54
fn acquisition_status(error: AcquisitionError) -> Status {
    let mut status = match error {
        AcquisitionError::Timeout => Status::deadline_exceeded("credential acquisition timed out"),
        AcquisitionError::Cancelled => Status::cancelled("request cancelled"),
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
