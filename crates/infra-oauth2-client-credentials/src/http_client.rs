use bytes::Bytes;
use http::{Request, Response, StatusCode};
use infra_outbound_http::Client;
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;

use crate::{
    AcquisitionError, Credentials, Error, IdentityMode,
    credentials::acquisition_deadline,
    request_admission::{AdmissionRefusal, admit_request_subject},
};

#[derive(Clone, derive_more::Debug)]
#[debug("AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    mode: IdentityMode,
}

impl Credentials {
    #[must_use]
    pub fn http(&self, resource: Client, mode: IdentityMode) -> AuthenticatedClient {
        AuthenticatedClient {
            credentials: self.clone(),
            resource,
            mode,
        }
    }
}

impl AuthenticatedClient {
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

    pub async fn execute_with_context(
        &self,
        mut request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        let subject = admit_request_subject(&mut request, self.mode);
        let mut prepared =
            self.resource
                .prepare(request, context)
                .map_err(|error| match error {
                    infra_outbound_http::Error::Timeout => {
                        Error::Acquisition(AcquisitionError::Timeout)
                    }
                    infra_outbound_http::Error::Cancelled => {
                        Error::Acquisition(AcquisitionError::Cancelled)
                    }
                    other => Error::Resource(other),
                })?;
        let deadline = acquisition_deadline(&prepared);
        self.credentials.check_lifecycle(deadline)?;
        let subject = subject.map_err(|refusal| match refusal {
            AdmissionRefusal::AuthorizationConflict => Error::AuthorizationConflict,
            AdmissionRefusal::SubjectRequired => Error::SubjectRequired,
            AdmissionRefusal::UnexpectedSubject => Error::UnexpectedSubject,
        })?;
        let acquired = self
            .credentials
            .acquire_for_request(subject, deadline)
            .await?;
        prepared.set_authorization(acquired.header.clone());
        prepared
            .send(
                || acquired.check_expiry(Instant::now()).is_ok(),
                |status| {
                    if status == StatusCode::UNAUTHORIZED {
                        acquired.reject_if_old_enough();
                    }
                },
            )
            .await
            .map_err(|error| match error {
                infra_outbound_http::Error::DispatchRefused => {
                    AcquisitionError::InvalidResponse.into()
                }
                other => Error::from(other),
            })
    }
}
