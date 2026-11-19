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
enum GrpcTokens {
    Service(ServiceTokens),
    Exchange(ExchangeTokens),
}

enum AdmittedTokens {
    Service(ServiceTokens),
    Exchange(ExchangeTokens, OnBehalfOf),
}

enum RequestAuth {
    None,
    OnBehalfOf(OnBehalfOf),
    AuthorizationConflict,
}

#[derive(Clone, Copy)]
enum AdmissionRefusal {
    AuthorizationConflict,
    SubjectRequired,
    UnexpectedSubject,
    Acquisition(AcquisitionError),
}

impl GrpcTokens {
    fn resource_origin(&self) -> &http::Uri {
        match self {
            Self::Service(service) => service.resource_origin(),
            Self::Exchange(exchange) => exchange.resource_origin(),
        }
    }

    fn admit(
        &self,
        auth: RequestAuth,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<AdmittedTokens, AdmissionRefusal> {
        match self {
            Self::Service(service) => service.check_active(deadline),
            Self::Exchange(exchange) => exchange.check_active(deadline),
        }
        .map_err(AdmissionRefusal::Acquisition)?;
        match (self, auth) {
            (_, RequestAuth::AuthorizationConflict) => Err(AdmissionRefusal::AuthorizationConflict),
            (Self::Service(service), RequestAuth::None) => {
                Ok(AdmittedTokens::Service(service.clone()))
            }
            (Self::Service(_), RequestAuth::OnBehalfOf(_)) => {
                Err(AdmissionRefusal::UnexpectedSubject)
            }
            (Self::Exchange(exchange), RequestAuth::OnBehalfOf(subject)) => {
                Ok(AdmittedTokens::Exchange(exchange.clone(), subject))
            }
            (Self::Exchange(_), RequestAuth::None) => Err(AdmissionRefusal::SubjectRequired),
        }
    }
}

impl AdmittedTokens {
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

fn take_request_auth<B>(request: &mut Request<B>) -> RequestAuth {
    if request.headers().contains_key(AUTHORIZATION) {
        return RequestAuth::AuthorizationConflict;
    }
    request
        .extensions_mut()
        .remove::<OnBehalfOf>()
        .map_or(RequestAuth::None, RequestAuth::OnBehalfOf)
}

#[derive(Clone, derive_more::Debug)]
#[debug("grpc::AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    tokens: GrpcTokens,
    resource: Client,
    context: Option<OperationContext>,
}

impl ServiceTokens {
    pub fn grpc(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        AuthenticatedClient::new(GrpcTokens::Service(self.clone()), resource)
    }
}

impl ExchangeTokens {
    pub fn grpc(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        AuthenticatedClient::new(GrpcTokens::Exchange(self.clone()), resource)
    }
}

impl AuthenticatedClient {
    fn new(tokens: GrpcTokens, resource: Client) -> Result<Self, ConfigurationError> {
        if !resource.uses_tls() {
            return Err(ConfigurationError {
                key: "grpc_resource",
                reason: "must use TLS to carry OAuth2 bearer tokens",
            });
        }
        if !resource.matches_origin(tokens.resource_origin()) {
            return Err(ConfigurationError::resource_origin());
        }
        Ok(Self {
            tokens,
            resource,
            context: None,
        })
    }

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
        let auth = take_request_auth(&mut request);
        let prepared =
            self.resource
                .prepare_call(request)
                .map_err(|status| match status.code() {
                    Code::Cancelled => acquisition_status(AcquisitionError::Cancelled),
                    Code::DeadlineExceeded => acquisition_status(AcquisitionError::Timeout),
                    _ => status,
                })?;
        let context = prepared.opening_context().clone();
        let admitted_tokens = match self.tokens.admit(auth, acquisition_deadline(&context)) {
            Ok(admitted_tokens) => admitted_tokens,
            Err(refusal) => return Err(prepared.refuse(admission_status(refusal))),
        };
        Ok(Box::pin(send_admitted(prepared, admitted_tokens, context)))
    }
}

async fn send_admitted(
    mut prepared: PreparedCall,
    admitted_tokens: AdmittedTokens,
    context: OperationContext,
) -> Result<Response<Body>, Status> {
    let token = match admitted_tokens
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
fn admission_status(refusal: AdmissionRefusal) -> Status {
    match refusal {
        AdmissionRefusal::AuthorizationConflict => {
            Status::internal("authorization conflicts with client credentials")
        }
        AdmissionRefusal::SubjectRequired => Status::internal("on-behalf-of subject is required"),
        AdmissionRefusal::UnexpectedSubject => {
            Status::internal("service identity does not accept an on-behalf-of subject")
        }
        AdmissionRefusal::Acquisition(error) => acquisition_status(error),
    }
}

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
