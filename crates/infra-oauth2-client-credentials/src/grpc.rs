use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use http::{Request, Response};
use infra_outbound_grpc::Client;
use operation_context::OperationContext;
use tokio::time::Instant;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{
    AcquisitionError, ConfigurationError, ExchangeTokens, ServiceTokens,
    credentials::{TokenSource, acquisition_deadline},
    request_admission::{AdmissionRefusal, take_request_subject},
};

#[derive(Clone, derive_more::Debug)]
#[debug("grpc::AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    tokens: TokenSource,
    resource: Client,
    context: Option<OperationContext>,
}

impl ServiceTokens {
    pub fn grpc(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        AuthenticatedClient::new(TokenSource::Service(self.clone()), resource)
    }
}

impl ExchangeTokens {
    pub fn grpc(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        AuthenticatedClient::new(TokenSource::Exchange(self.clone(), ()), resource)
    }
}

impl AuthenticatedClient {
    fn new(tokens: TokenSource, resource: Client) -> Result<Self, ConfigurationError> {
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
}

impl Service<Request<Body>> for AuthenticatedClient {
    type Response = Response<Body>;
    type Error = Status;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        if let Some(context) = &self.context {
            if request.extensions().get::<OperationContext>().is_some() {
                let status = self.resource.refuse_call(
                    &request,
                    Status::internal("request operation context conflicts with client context"),
                );
                return Box::pin(std::future::ready(Err(status)));
            }
            request.extensions_mut().insert(context.clone());
        }
        let subject = take_request_subject(&mut request);
        let mut prepared = match self.resource.prepare_call(request) {
            Ok(prepared) => prepared,
            Err(status) => {
                let status = match status.code() {
                    Code::Cancelled => acquisition_status(AcquisitionError::Cancelled),
                    Code::DeadlineExceeded => acquisition_status(AcquisitionError::Timeout),
                    _ => status,
                };
                return Box::pin(std::future::ready(Err(status)));
            }
        };
        let context = prepared.opening_context().clone();
        let grant = match self.tokens.admit(subject, acquisition_deadline(&context)) {
            Ok(grant) => grant,
            Err(refusal) => {
                return Box::pin(std::future::ready(Err(
                    prepared.refuse(admission_status(refusal))
                )));
            }
        };
        Box::pin(async move {
            let token = match grant
                .acquire_for_request(acquisition_deadline(&context))
                .await
            {
                Ok(acquired) => acquired,
                Err(error) => return Err(prepared.refuse(acquisition_status(error))),
            };
            prepared
                .headers_mut()
                .insert(http::header::AUTHORIZATION, token.header.clone());
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
        })
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
