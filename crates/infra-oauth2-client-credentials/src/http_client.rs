use bytes::Bytes;
use http::{Request, Response, StatusCode};
use infra_outbound_http::Client;
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;

use crate::{
    AcquisitionError, Credentials, Error,
    credentials::{acquisition_deadline, acquisition_stopped},
    request_admission::{AdmissionRefusal, admit_request_subject},
};

#[derive(Clone, derive_more::Debug)]
#[debug("AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    credentials: Credentials,
    resource: Client,
    subject_required: bool,
}

impl Credentials {
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
    #[must_use]
    pub fn require_on_behalf_of(mut self) -> Self {
        self.subject_required = true;
        self
    }

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
        let context = self.resource.begin_operation(context);
        let request_context = request.extensions().get::<OperationContext>().cloned();
        let acquisition_context = request_context
            .as_ref()
            .and_then(OperationContext::deadline)
            .map(|request_deadline| {
                let deadline = context
                    .deadline()
                    .map_or(request_deadline, |local| local.earlier(request_deadline));
                OperationContext::new(Some(deadline), context.cancellation().clone())
            });
        let acquisition_context = acquisition_context.as_ref().unwrap_or(&context);
        let check_stop = || {
            let request_stop = request_context
                .as_ref()
                .map_or(Ok(()), OperationContext::check);
            acquisition_context
                .check()
                .and(request_stop)
                .map_err(acquisition_stopped)
        };
        let request_stopped = async {
            match &request_context {
                Some(request_context) => request_context.wait_stopped().await,
                None => std::future::pending().await,
            }
        };
        self.credentials.check_lifecycle(acquisition_deadline(
            acquisition_context,
            request_context.as_ref(),
        ))?;
        let on_behalf_of =
            admit_request_subject(&mut request, self.subject_required).map_err(|refusal| {
                match refusal {
                    AdmissionRefusal::AuthorizationConflict => Error::AuthorizationConflict,
                    AdmissionRefusal::SubjectRequired => Error::SubjectRequired,
                }
            })?;
        check_stop()?;
        let acquired = tokio::select! {
            biased;
            stopped = request_stopped => Err(acquisition_stopped(stopped)),
            result = self.credentials.authorize(
                request.headers_mut(), on_behalf_of, acquisition_context, request_context.as_ref(),
            ) => result,
        };
        check_stop()?;
        let acquired = acquired?;
        self.resource
            .execute_with_dispatch_check(
                request,
                &context,
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
