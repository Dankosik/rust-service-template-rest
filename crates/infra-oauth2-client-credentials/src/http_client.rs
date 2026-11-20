use std::sync::Arc;

use bytes::Bytes;
use http::{Request, Response, StatusCode, Uri, header::AUTHORIZATION};
use infra_outbound_http::{Client, DispatchError, Error as HttpError, ExchangeError};
use operation_context::OperationContext;
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

#[derive(Clone, derive_more::Debug)]
#[debug("ExchangeHttpClient([REDACTED])")]
pub struct ExchangeHttpClient {
    tokens: ExchangeTokens,
    resource: Client,
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
    pub fn host(&self) -> &str {
        self.resource.host()
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.resource.port()
    }

    pub fn uri(&self, path_and_query: &str) -> Result<Uri, http::Error> {
        self.resource.uri(path_and_query)
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
    ) -> Result<Response<Bytes>, Error> {
        execute_authenticated(
            &self.resource,
            context,
            request,
            Grant::Service(&self.tokens),
        )
        .await
    }
}

impl ExchangeHttpClient {
    #[must_use]
    pub fn host(&self) -> &str {
        self.resource.host()
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.resource.port()
    }

    pub fn uri(&self, path_and_query: &str) -> Result<Uri, http::Error> {
        self.resource.uri(path_and_query)
    }

    pub async fn execute(
        &self,
        context: &OperationContext,
        request: Request<Bytes>,
        subject: &OnBehalfOf,
    ) -> Result<Response<Bytes>, Error> {
        execute_authenticated(
            &self.resource,
            context,
            request,
            Grant::Exchange(&self.tokens, subject),
        )
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
    context: &OperationContext,
    request: Request<Bytes>,
    grant: Grant<'_>,
) -> Result<Response<Bytes>, Error> {
    let mut prepared = resource
        .prepare(context, request)
        .map_err(Error::at_prepare)?;
    let deadline = acquisition_deadline(prepared.context());
    grant.check_active(deadline)?;
    if prepared.headers().contains_key(AUTHORIZATION) {
        return Err(Error::AuthorizationConflict);
    }
    let acquired = grant.acquire_for_request(deadline).await?;
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
            other => Self::Resource(ExchangeError::BeforeResponse(other)),
        }
    }

    fn at_send(error: DispatchError<AcquisitionError>) -> Self {
        match error {
            DispatchError::Refused(error) => Self::Acquisition(error),
            DispatchError::Exchange(error) => Self::Resource(error),
        }
    }
}
