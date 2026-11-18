use bytes::Bytes;
use http::{Request, Response, StatusCode};
use infra_outbound_http::Client;
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;

use crate::{
    AcquisitionError, ConfigurationError, Error, ExchangeTokens, ServiceTokens,
    credentials::{TokenSource, acquisition_deadline},
    request_admission::take_request_subject,
};

#[derive(Clone, derive_more::Debug)]
#[debug("AuthenticatedClient([REDACTED])")]
pub struct AuthenticatedClient {
    tokens: TokenSource,
    resource: Client,
}

#[derive(Clone, Copy, Debug)]
pub struct RequestClient<'a> {
    client: &'a AuthenticatedClient,
    context: &'a OperationContext,
}

impl ServiceTokens {
    pub fn http(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        AuthenticatedClient::new(TokenSource::Service(self.clone()), resource)
    }
}

impl ExchangeTokens {
    pub fn http(&self, resource: Client) -> Result<AuthenticatedClient, ConfigurationError> {
        AuthenticatedClient::new(TokenSource::Exchange(self.clone(), ()), resource)
    }
}

impl AuthenticatedClient {
    fn new(tokens: TokenSource, resource: Client) -> Result<Self, ConfigurationError> {
        if !resource.matches_origin(tokens.resource_origin()) {
            return Err(ConfigurationError::resource_origin());
        }
        Ok(Self { tokens, resource })
    }

    #[must_use]
    pub const fn with_context<'a>(&'a self, context: &'a OperationContext) -> RequestClient<'a> {
        RequestClient {
            client: self,
            context,
        }
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
        let subject = take_request_subject(&mut request);
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
        let deadline = acquisition_deadline(prepared.context());
        let grant = self.tokens.admit(subject, deadline)?;
        let acquired = grant.acquire_for_request(deadline).await?;
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

impl RequestClient<'_> {
    pub async fn execute(&self, request: Request<Bytes>) -> Result<Response<Bytes>, Error> {
        self.client
            .execute_with_context(request, self.context)
            .await
    }
}

impl From<AdmissionRefusal> for Error {
    fn from(refusal: AdmissionRefusal) -> Self {
        match refusal {
            AdmissionRefusal::AuthorizationConflict => Self::AuthorizationConflict,
            AdmissionRefusal::SubjectRequired => Self::SubjectRequired,
            AdmissionRefusal::UnexpectedSubject => Self::UnexpectedSubject,
            AdmissionRefusal::Acquisition(error) => Self::Acquisition(error),
        }
    }
}
