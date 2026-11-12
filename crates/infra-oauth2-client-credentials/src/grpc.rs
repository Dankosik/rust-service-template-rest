//! Concrete gRPC binding. The credential and cache remain in their private
//! owner; [`Credentials::grpc`] documents the call contract.

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use http::{Request, Response};
use infra_grpc::Client;
use tonic::{Code, Status, body::Body};
use tower::Service;

use crate::{
    AcquisitionError, Credentials,
    credentials::{AdmissionRefusal, acquisition_deadline, take_subject},
};

/// A cloneable governed gRPC client with private machine credentials.
#[derive(Clone)]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    subject_required: bool,
}

impl AuthenticatedClient {
    /// Refuses a call without [`crate::OnBehalfOf`] instead of sending the
    /// service token. See [`crate::OnBehalfOf`] for when a binding requires a
    /// subject.
    #[must_use]
    pub fn require_on_behalf_of(mut self) -> Self {
        self.subject_required = true;
        self
    }
}

impl std::fmt::Debug for AuthenticatedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("grpc::AuthenticatedClient([REDACTED])")
    }
}

impl Credentials {
    /// Binds this credential owner to a gRPC resource client.
    ///
    /// The returned service removes an [`crate::OnBehalfOf`] extension before
    /// dispatch and sends the token exchanged for that subject; without one it
    /// sends the service token unless
    /// [`AuthenticatedClient::require_on_behalf_of`] is set. A caller-supplied
    /// `Authorization` header and a missing required subject are refused as
    /// `INTERNAL` before dispatch. A response carrying
    /// `grpc-status: UNAUTHENTICATED`, or HTTP 401 without `grpc-status`, evicts
    /// the token that call used once it is old enough; completed responses are
    /// returned without replay. The service is always ready, and acquisition
    /// failures map to `DEADLINE_EXCEEDED`, `CANCELLED`, `UNAVAILABLE`, or
    /// `UNAUTHENTICATED` by cause.
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
        let admission = take_subject(&mut request, self.subject_required);
        let mut prepared = self.resource.prepare_call(request);
        let context = prepared.opening_context().clone();
        let precheck = self
            .credentials
            .check_lifecycle(acquisition_deadline(&context))
            .map_err(acquisition_status)
            .and_then(|()| admission.map_err(admission_status));
        let on_behalf_of = match precheck {
            Ok(subject) => subject,
            Err(status) => return Box::pin(std::future::ready(Err(prepared.refuse(status)))),
        };
        let credentials = self.credentials.clone();
        Box::pin(async move {
            let acquired = match credentials
                .authorize(prepared.headers_mut(), on_behalf_of, &context)
                .await
            {
                Ok(acquired) => acquired,
                Err(error) => return Err(prepared.refuse(acquisition_status(error))),
            };
            let response = prepared.send().await?;
            if infra_grpc::client_response_code(&response) == Some(Code::Unauthenticated) {
                credentials.evict_acquired(&acquired).await;
            }
            Ok(response)
        })
    }
}

/// Both refusals are this service's own composition mistakes. They are
/// `INTERNAL`, as gRFC A54 has a channel report failed call credentials: a
/// code reserved for the application would blame the inbound caller when a
/// handler forwards the status.
fn admission_status(refusal: AdmissionRefusal) -> Status {
    match refusal {
        AdmissionRefusal::AuthorizationConflict => {
            Status::internal("authorization conflicts with client credentials")
        }
        AdmissionRefusal::SubjectRequired => Status::internal("on-behalf-of subject is required"),
    }
}

/// A caller deadline is `DEADLINE_EXCEEDED`; a cancelled call is `CANCELLED`;
/// a failure that may pass unchanged later is `UNAVAILABLE`; any other is
/// `UNAUTHENTICATED`, as gRPC clients report credentials that could not
/// produce call metadata. The closed
/// reason stays reachable as the status source and never crosses the wire.
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
