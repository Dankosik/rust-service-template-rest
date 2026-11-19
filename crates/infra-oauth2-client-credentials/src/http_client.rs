use std::sync::Arc;

use bytes::Bytes;
use http::{Request, Response, StatusCode, header::AUTHORIZATION};
use infra_outbound_http::{Client, Error as HttpError};
use operation_context::{Deadline, OperationContext};
use tokio::time::Instant;

use crate::{
    AcquisitionError, ConfigurationError, Error, ExchangeTokens, OnBehalfOf, ServiceTokens,
    credentials::{AcquisitionDeadline, acquisition_deadline},
    token::Token,
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
        request: Request<Bytes>,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        execute_authenticated(
            &self.resource,
            request,
            context,
            Grant::Service(&self.tokens),
        )
        .await
    }
}

impl ServiceRequestClient<'_> {
    pub async fn execute(&self, request: Request<Bytes>) -> Result<Response<Bytes>, Error> {
        self.client
            .execute_with_context(request, self.context)
            .await
    }
}

impl ExchangeHttpClient {
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
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        self.execute_with_context(
            request,
            subject,
            &OperationContext::from_deadline(Deadline::at(deadline)),
        )
        .await
    }

    pub async fn execute_with_context(
        &self,
        request: Request<Bytes>,
        subject: &OnBehalfOf,
        context: &OperationContext,
    ) -> Result<Response<Bytes>, Error> {
        execute_authenticated(
            &self.resource,
            request,
            context,
            Grant::Exchange(&self.tokens, subject),
        )
        .await
    }
}

impl ExchangeRequestClient<'_> {
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        subject: &OnBehalfOf,
    ) -> Result<Response<Bytes>, Error> {
        self.client
            .execute_with_context(request, subject, self.context)
            .await
    }
}

#[derive(Clone, Copy)]
enum Grant<'a> {
    Service(&'a ServiceTokens),
    Exchange(&'a ExchangeTokens, &'a OnBehalfOf),
}

impl Grant<'_> {
    fn check_active(self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        match self {
            Self::Service(tokens) => tokens.check_active(deadline),
            Self::Exchange(tokens, _) => tokens.check_active(deadline),
        }
    }

    async fn acquire_for_request(
        self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        match self {
            Self::Service(tokens) => tokens.acquire_for_request(deadline).await,
            Self::Exchange(tokens, subject) => tokens.acquire_for_request(subject, deadline).await,
        }
    }
}

async fn execute_authenticated(
    resource: &Client,
    request: Request<Bytes>,
    context: &OperationContext,
    grant: Grant<'_>,
) -> Result<Response<Bytes>, Error> {
    let has_authorization = request.headers().contains_key(AUTHORIZATION);
    let mut prepared = resource
        .prepare(request, context)
        .map_err(Error::at_prepare)?;
    let deadline = acquisition_deadline(prepared.context());
    grant.check_active(deadline)?;
    if has_authorization {
        return Err(Error::AuthorizationConflict);
    }
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

    fn at_send(error: HttpError) -> Self {
        match error {
            HttpError::DispatchRefused => AcquisitionError::InvalidResponse.into(),
            other => Self::Resource(other),
        }
    }
}
