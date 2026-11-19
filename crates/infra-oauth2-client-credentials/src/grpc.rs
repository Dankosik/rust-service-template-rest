use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_util::future::Either;
use http::{Request, Response, header::AUTHORIZATION};
use infra_outbound_grpc::{Client, PreparedCall};
use operation_context::OperationContext;
use tokio::time::Instant;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{
    AcquisitionError, ConfigurationError, ExchangeTokens, OnBehalfOf, ServiceTokens,
    credentials::{AcquisitionDeadline, acquisition_deadline},
    token::Token,
};

type RpcFuture = Pin<Box<dyn Future<Output = Result<Response<Body>, Status>> + Send>>;
type CallFuture = Either<std::future::Ready<Result<Response<Body>, Status>>, RpcFuture>;

#[derive(Clone)]
enum BoundTokens {
    Service(ServiceTokens),
    Exchange(ExchangeTokens, OnBehalfOf),
}

impl BoundTokens {
    fn check_active(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        match self {
            Self::Service(service) => service.check_active(deadline),
            Self::Exchange(exchange, _) => exchange.check_active(deadline),
        }
    }

    async fn acquire_for_request(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        match self {
            Self::Service(service) => service.acquire_for_request(deadline).await,
            Self::Exchange(exchange, subject) => {
                exchange.acquire_for_request(subject, deadline).await
            }
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
            tokens: BoundTokens::Exchange(self.tokens.clone(), subject),
            resource: self.resource.clone(),
            context: None,
        }
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("grpc::AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    tokens: BoundTokens,
    resource: Client,
    context: Option<OperationContext>,
}

impl ServiceTokens {
    pub fn grpc(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        validate_resource(&resource, self.resource_origin())?;
        Ok(AuthenticatedClient {
            tokens: BoundTokens::Service(self.clone()),
            resource,
            context: None,
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
        return Err(ConfigurationError::resource_origin());
    }
    Ok(())
}

impl AuthenticatedClient {
    #[must_use]
    pub fn with_context(&self, context: &OperationContext) -> Self {
        Self {
            tokens: self.tokens.clone(),
            resource: self.resource.clone(),
            context: Some(context.clone()),
        }
    }

    fn begin_call(&self, mut request: Request<Body>) -> Result<RpcFuture, Status> {
        if let Some(context) = &self.context {
            if request.extensions().get::<OperationContext>().is_some() {
                return Err(self.resource.refuse_call(
                    &request,
                    Status::internal("request operation context conflicts with client context"),
                ));
            }
            request.extensions_mut().insert(context.clone());
        }
        let has_authorization = request.headers().contains_key(AUTHORIZATION);
        let prepared =
            self.resource
                .prepare_call(request)
                .map_err(|status| match status.code() {
                    Code::Cancelled => acquisition_status(AcquisitionError::Cancelled),
                    Code::DeadlineExceeded => acquisition_status(AcquisitionError::Timeout),
                    _ => status,
                })?;
        let context = prepared.opening_context();
        if let Err(error) = self.tokens.check_active(acquisition_deadline(&context)) {
            return Err(prepared.refuse(acquisition_status(error)));
        }
        if has_authorization {
            return Err(prepared.refuse(Status::internal(
                "authorization conflicts with client credentials",
            )));
        }
        Ok(Box::pin(send_admitted(
            prepared,
            self.tokens.clone(),
            context,
        )))
    }
}

async fn send_admitted(
    mut prepared: PreparedCall,
    tokens: BoundTokens,
    context: OperationContext,
) -> Result<Response<Body>, Status> {
    let token = match tokens
        .acquire_for_request(acquisition_deadline(&context))
        .await
    {
        Ok(acquired) => acquired,
        Err(error) => return Err(prepared.refuse(acquisition_status(error))),
    };
    prepared
        .headers_mut()
        .insert(AUTHORIZATION, token.header.clone());
    let used = Arc::downgrade(&token);
    prepared
        .send_with_dispatch_check(
            move || {
                token
                    .check_expiry(Instant::now())
                    .map_err(acquisition_status)
            },
            move |code| {
                if code == Code::Unauthenticated
                    && let Some(token) = used.upgrade()
                {
                    token.reject_if_old_enough();
                }
            },
        )
        .await
}

impl Service<Request<Body>> for AuthenticatedClient {
    type Response = Response<Body>;
    type Error = Status;
    type Future = CallFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        match self.begin_call(request) {
            Ok(future) => Either::Right(future),
            Err(status) => Either::Left(std::future::ready(Err(status))),
        }
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
