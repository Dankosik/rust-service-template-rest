use std::sync::Arc;

use bytes::Bytes;
use http::{Request, Response, StatusCode, header};
use infra_outbound_http::{Client, DispatchError, Error as HttpError, Origin, PreparedRequest};
use operation_context::OperationContext;
use tokio::time::Instant;

use crate::{
    AcquisitionError, ConfigurationError, Error, ExchangeTokens, OnBehalfOf, ServiceTokens,
    credentials::acquisition_deadline, token::Token,
};

#[derive(Clone, derive_more::Debug)]
#[debug("ServiceHttpClient([REDACTED])")]
pub struct ServiceHttpClient {
    tokens: ServiceTokens,
    resource: Client,
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceRequestClient<'a> {
    client: &'a ServiceHttpClient,
    context: &'a OperationContext,
}

#[derive(Clone, derive_more::Debug)]
#[debug("ExchangeHttpClient([REDACTED])")]
pub struct ExchangeHttpClient {
    tokens: ExchangeTokens,
    resource: Client,
}

#[derive(Clone, Copy, Debug)]
pub struct ExchangeRequestClient<'a> {
    client: &'a ExchangeHttpClient,
    context: &'a OperationContext,
}

impl ServiceTokens {
    pub fn http(&self, resource: Client) -> Result<ServiceHttpClient, ConfigurationError> {
        if !resource.matches_origin(self.resource_origin()) {
            return Err(ConfigurationError::resource_origin());
        }
        Ok(ServiceHttpClient {
            tokens: self.clone(),
            resource,
        })
    }
}

impl ExchangeTokens {
    pub fn http(&self, resource: Client) -> Result<ExchangeHttpClient, ConfigurationError> {
        if !resource.matches_origin(self.resource_origin()) {
            return Err(ConfigurationError::resource_origin());
        }
        Ok(ExchangeHttpClient {
            tokens: self.clone(),
            resource,
        })
    }
}

impl ServiceHttpClient {
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        self.resource.origin()
    }

    #[must_use]
    pub const fn with_context<'a>(
        &'a self,
        context: &'a OperationContext,
    ) -> ServiceRequestClient<'a> {
        ServiceRequestClient {
            client: self,
            context,
        }
    }

    pub async fn execute(
        &self,
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        let has_auth = request.headers().contains_key(header::AUTHORIZATION);
        let prepared = self
            .resource
            .prepare(request, context)
            .map_err(Error::at_prepare)?;
        let deadline = acquisition_deadline(prepared.context());
        self.tokens.check_active(deadline)?;
        if has_auth {
            return Err(Error::AuthorizationConflict);
        }
        let acquired = self.tokens.acquire_for_request(deadline).await?;
        send_authorized(prepared, acquired).await
    }
}

impl ServiceRequestClient<'_> {
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        self.client.origin()
    }

    pub async fn execute(&self, request: Request<Bytes>) -> Result<Response<Bytes>, Error> {
        self.client.execute(request, self.context).await
    }
}

impl ExchangeHttpClient {
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        self.resource.origin()
    }

    #[must_use]
    pub const fn with_context<'a>(
        &'a self,
        context: &'a OperationContext,
    ) -> ExchangeRequestClient<'a> {
        ExchangeRequestClient {
            client: self,
            context,
        }
    }

    pub async fn execute(
        &self,
        request: Request<Bytes>,
        subject: &OnBehalfOf,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        let has_auth = request.headers().contains_key(header::AUTHORIZATION);
        let prepared = self
            .resource
            .prepare(request, context)
            .map_err(Error::at_prepare)?;
        let deadline = acquisition_deadline(prepared.context());
        self.tokens.check_active(deadline)?;
        if has_auth {
            return Err(Error::AuthorizationConflict);
        }
        let acquired = self.tokens.acquire_for_request(subject, deadline).await?;
        send_authorized(prepared, acquired).await
    }
}

impl ExchangeRequestClient<'_> {
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        self.client.origin()
    }

    pub async fn execute(
        &self,
        request: Request<Bytes>,
        subject: &OnBehalfOf,
    ) -> Result<Response<Bytes>, Error> {
        self.client.execute(request, subject, self.context).await
    }
}

async fn send_authorized(
    mut prepared: PreparedRequest<'_>,
    acquired: Arc<Token>,
) -> Result<Response<Bytes>, Error> {
    prepared.set_authorization(acquired.header.clone());
    prepared
        .send_checked(
            || acquired.check_expiry(Instant::now()),
            |status| {
                if status == StatusCode::UNAUTHORIZED {
                    acquired.reject_if_old_enough();
                }
            },
        )
        .await
        .map_err(Error::at_send)
}

impl Error {
    fn at_prepare(error: HttpError) -> Self {
        match error {
            HttpError::Timeout => AcquisitionError::Timeout.into(),
            HttpError::Cancelled => AcquisitionError::Cancelled.into(),
            other => Self::Resource(other),
        }
    }

    fn at_send(error: DispatchError<AcquisitionError>) -> Self {
        match error {
            DispatchError::Refused(error) => Self::Acquisition(error),
            DispatchError::Exchange(error) => Self::Resource(error.into_error()),
        }
    }
}
