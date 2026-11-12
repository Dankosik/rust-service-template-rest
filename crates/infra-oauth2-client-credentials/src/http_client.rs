//! Concrete HTTP binding. The credential and cache remain in their private
//! owner; [`AuthenticatedClient::execute`] documents the call contract.

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use infra_outbound_http::Client;
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;

use crate::{
    Credentials, Error,
    credentials::{AdmissionRefusal, acquisition_deadline, acquisition_stopped, take_subject},
};

#[derive(Clone, derive_more::Debug)]
#[debug("AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    subject_required: bool,
}

impl Credentials {
    /// Binds this credential owner to an HTTP resource client. Requests carry
    /// the service token, or one exchanged for an attached [`crate::OnBehalfOf`]; see
    /// [`AuthenticatedClient::execute`] and
    /// [`AuthenticatedClient::require_on_behalf_of`].
    #[must_use]
    pub fn http(&self, resource: Client) -> AuthenticatedClient {
        AuthenticatedClient {
            credentials: self.clone(),
            resource,
            subject_required: false,
        }
    }
}

impl AuthenticatedClient {
    /// Refuses a request without [`crate::OnBehalfOf`] instead of sending the service
    /// token. See [`crate::OnBehalfOf`] for when a binding requires a subject.
    #[must_use]
    pub fn require_on_behalf_of(mut self) -> Self {
        self.subject_required = true;
        self
    }

    /// Acquires credentials, injects Bearer, and spends the original deadline.
    /// A request carrying [`crate::OnBehalfOf`] in its extensions is sent with a
    /// token exchanged for that subject instead of the service token; the
    /// extension is always removed before dispatch. Completed responses,
    /// including 401 and 403, are returned without replay; a 401 evicts the
    /// token it used once that token is at least 30 seconds old and no newer
    /// token replaced it, so the next call may acquire anew.
    ///
    /// # Errors
    /// Rejects caller Authorization, or a missing subject this client
    /// [requires](Self::require_on_behalf_of), before I/O, acquisition failure
    /// before resource dispatch, and otherwise preserves bounded resource
    /// errors.
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_with_context(
            request,
            &OperationContext::from_deadline(Deadline::at(deadline)),
        )
        .await
    }

    /// Acquires credentials and completes the resource exchange within one fixed
    /// context, clamped to the resource client's local ceiling before token work.
    /// Request context extensions additionally bound credentials and dispatch.
    /// Cancellation stops this caller's wait without closing the credential owner.
    ///
    /// # Errors
    /// Returns composition, acquisition, or bounded resource errors, as [`Self::execute`].
    pub async fn execute_with_context(
        &self,
        mut request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        let context = self.resource.begin_operation(context);
        let request_context = request.extensions().get::<OperationContext>().cloned();
        let check_request = || {
            request_context
                .as_ref()
                .map_or(Ok(()), OperationContext::check)
                .map_err(acquisition_stopped)
        };
        let request_stopped = async {
            match &request_context {
                Some(request_context) => request_context.wait_stopped().await,
                None => std::future::pending().await,
            }
        };
        self.credentials
            .check_lifecycle(acquisition_deadline(&context))?;
        let on_behalf_of =
            take_subject(&mut request, self.subject_required).map_err(|refusal| match refusal {
                AdmissionRefusal::AuthorizationConflict => Error::AuthorizationConflict,
                AdmissionRefusal::SubjectRequired => Error::SubjectRequired,
            })?;
        check_request()?;
        // Authorization first: a cached token then never arms the deadline timer.
        let acquired = tokio::select! {
            biased;
            result = self.credentials.authorize(request.headers_mut(), on_behalf_of, &context) => result,
            stopped = request_stopped => return Err(acquisition_stopped(stopped).into()),
        };
        check_request()?;
        let acquired = acquired?;
        let response = self
            .resource
            .execute_with_context(request, &context)
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            self.credentials.evict_acquired(&acquired).await;
        }
        Ok(response)
    }
}
