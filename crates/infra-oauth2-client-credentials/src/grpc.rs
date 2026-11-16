use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use http::{Request, Response};
use infra_grpc::Client;
use tokio::time::Instant;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{
    AcquisitionError, Credentials,
    credentials::acquisition_deadline,
    request_admission::{AdmissionRefusal, admit_request_subject},
};

#[derive(Clone, derive_more::Debug)]
#[debug("grpc::AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    subject_required: bool,
}

impl AuthenticatedClient {
    #[must_use]
    pub fn require_on_behalf_of(mut self) -> Self {
        self.subject_required = true;
        self
    }
}

impl Credentials {
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
        let admission = admit_request_subject(&mut request, self.subject_required);
        let mut prepared = self.resource.prepare_call(request);
        let context = prepared.opening_context().clone();
        let precheck = self
            .credentials
            .check_lifecycle(acquisition_deadline(&context, None))
            .map_err(acquisition_status)
            .and_then(|()| admission.map_err(admission_status));
        let on_behalf_of = match precheck {
            Ok(subject) => subject,
            Err(status) => return Box::pin(std::future::ready(Err(prepared.refuse(status)))),
        };
        let credentials = self.credentials.clone();
        Box::pin(async move {
            let token = match credentials
                .authorize(prepared.headers_mut(), on_behalf_of, &context, None)
                .await
            {
                Ok(acquired) => acquired,
                Err(error) => return Err(prepared.refuse(acquisition_status(error))),
            };
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
