//! A fixed-authority HTTPS client with finite exchange limits.
//!
//! The provider owns request content and interpretation. This crate owns
//! authority admission, bounded transport work, and static error semantics.

mod policy;

use policy::Authority;

#[cfg(test)]
mod tests;

use std::{error::Error as StdError, fmt, sync::Arc, time::Duration};

use infra_egress_dns::{PublicAddressResolver, ResolveError};
use reqwest::redirect::Policy;
pub use reqwest::{Method, StatusCode, header::HeaderMap};
use tokio::{sync::Semaphore, time::Instant};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;

/// Fixed client ceilings. Every field is required and finite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_active: usize,
    pub operation_timeout: Duration,
    pub request_header_count: usize,
    pub request_header_bytes: usize,
    pub response_header_count: usize,
    pub response_header_bytes: usize,
    pub request_body_bytes: usize,
    pub response_body_bytes: usize,
}

/// A buffered provider request.
pub struct Request {
    pub method: Method,
    pub target: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl fmt::Debug for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Request([REDACTED])")
    }
}

/// Request-specific custody supplied by the caller that owns the parent work.
#[derive(Clone)]
pub struct Operation {
    pub deadline: Instant,
    pub cancel: CancellationToken,
    pub timeout: Option<Duration>,
    pub response_body_bytes: Option<usize>,
}

impl fmt::Debug for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Operation([REDACTED])")
    }
}

/// One fully framed, bounded HTTP response.
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl fmt::Debug for Response {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Response([REDACTED])")
    }
}

/// Content-free outbound exchange failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("outbound HTTP configuration is invalid")]
    InvalidConfiguration,
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    #[error("outbound HTTP target is denied")]
    Denied,
    #[error("outbound HTTP is at capacity")]
    AtCapacity,
    #[error("outbound HTTP operation timed out")]
    Timeout,
    #[error("outbound HTTP operation was cancelled")]
    Cancelled,
    #[error("outbound HTTP request headers are too large")]
    RequestHeadersTooLarge,
    #[error("outbound HTTP request body is too large")]
    RequestBodyTooLarge,
    #[error("outbound HTTP response headers are too large")]
    ResponseHeadersTooLarge,
    #[error("outbound HTTP response body is too large")]
    ResponseBodyTooLarge,
    #[error("outbound HTTP transport failed")]
    Transport,
}

/// A reusable bounded client. Its configured authority is never exposed.
#[derive(Clone)]
pub struct Client {
    base: Url,
    authority: Authority,
    limits: Limits,
    transport: reqwest::Client,
    admission: Arc<Semaphore>,
    shutdown: CancellationToken,
    // Test-only phase evidence; absent from production layout and behavior.
    #[cfg(test)]
    response_head_observed: Option<Arc<tokio::sync::Notify>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Client([REDACTED])")
    }
}

impl Client {
    /// Creates a fixed-authority client without performing DNS or network I/O.
    ///
    /// # Errors
    ///
    /// Returns a static configuration or denial error when limits, the base
    /// destination, resolver setup, or private client construction are invalid.
    pub fn new(
        base: &str,
        limits: Limits,
        tracker: TaskTracker,
        shutdown: CancellationToken,
    ) -> Result<Self, Error> {
        policy::validate_limits(&limits)?;
        let (base, authority) = policy::admit_base(base)?;

        let resolver = PublicAddressResolver::new(tracker, shutdown.clone())
            .map_err(|_| Error::InvalidConfiguration)?;
        let transport = build_client(resolver, &limits)?;
        Ok(Self {
            base,
            authority,
            limits,
            transport,
            admission: Arc::new(Semaphore::new(limits.max_active)),
            shutdown,
            #[cfg(test)]
            response_head_observed: None,
        })
    }

    /// Executes one complete buffered exchange inside the supplied custody.
    ///
    /// # Errors
    ///
    /// Returns only content-free policy, capacity, timeout, cancellation, size,
    /// or transport errors.
    pub async fn execute(&self, request: Request, operation: Operation) -> Result<Response, Error> {
        let started = Instant::now();
        let (deadline, body_limit) = self.operation_limits(&operation, started)?;
        let target = policy::admit_target(&self.base, &self.authority, &request.target)?;
        let headers = policy::admit_request_headers(
            request.headers,
            self.limits.request_header_count,
            self.limits.request_header_bytes,
        )?;
        if request.body.len() > self.limits.request_body_bytes {
            return Err(Error::RequestBodyTooLarge);
        }
        if let Some(error) = self.interruption(&operation, deadline) {
            return Err(error);
        }

        let request_cancel = operation.cancel.clone();
        let work = async {
            let permit = self
                .admission
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::AtCapacity)?;
            if let Some(error) = self.interruption(&operation, deadline) {
                drop(permit);
                return Err(error);
            }

            let send = self
                .transport
                .request(request.method, target)
                .headers(headers)
                .body(request.body)
                .send();
            let response = send.await.map_err(|error| map_transport_error(&error))?;
            policy::admit_response_headers(
                response.headers(),
                self.limits.response_header_count,
                self.limits.response_header_bytes,
            )?;
            if response
                .content_length()
                .is_some_and(|length| length > body_limit as u64)
            {
                return Err(Error::ResponseBodyTooLarge);
            }

            #[cfg(test)]
            if let Some(observed) = &self.response_head_observed {
                observed.notify_one();
            }

            let mut response = response;
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| map_transport_error(&error))?
            {
                let remaining = body_limit.saturating_sub(body.len());
                if chunk.len() > remaining {
                    return Err(Error::ResponseBodyTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            let response = Response {
                status: response.status(),
                headers: response.headers().clone(),
                body,
            };
            drop(permit);
            Ok(response)
        };

        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => Err(Error::Cancelled),
            () = request_cancel.cancelled() => Err(Error::Cancelled),
            result = tokio::time::timeout_at(deadline, work) => result.map_err(|_| Error::Timeout)?,
        }
    }

    fn operation_limits(
        &self,
        operation: &Operation,
        started: Instant,
    ) -> Result<(Instant, usize), Error> {
        let timeout = operation.timeout.unwrap_or(self.limits.operation_timeout);
        let body_limit = operation
            .response_body_bytes
            .unwrap_or(self.limits.response_body_bytes);
        if timeout.is_zero()
            || timeout > self.limits.operation_timeout
            || body_limit == 0
            || body_limit > self.limits.response_body_bytes
        {
            return Err(Error::InvalidConfiguration);
        }
        let local_deadline = started
            .checked_add(timeout)
            .ok_or(Error::InvalidConfiguration)?;
        Ok((operation.deadline.min(local_deadline), body_limit))
    }

    fn interruption(&self, operation: &Operation, deadline: Instant) -> Option<Error> {
        if self.shutdown.is_cancelled() || operation.cancel.is_cancelled() {
            Some(Error::Cancelled)
        } else if Instant::now() >= deadline {
            Some(Error::Timeout)
        } else {
            None
        }
    }
}

fn build_client<R>(resolver: R, limits: &Limits) -> Result<reqwest::Client, Error>
where
    R: reqwest::dns::Resolve + 'static,
{
    client_builder(resolver, limits)
        .build()
        .map_err(|_| Error::InvalidConfiguration)
}

fn client_builder<R>(resolver: R, limits: &Limits) -> reqwest::ClientBuilder
where
    R: reqwest::dns::Resolve + 'static,
{
    reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(true)
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .http1_only()
        .pool_max_idle_per_host(0)
        .http1_max_headers(limits.response_header_count)
        .timeout(limits.operation_timeout)
        .dns_resolver(resolver)
}

#[cfg(test)]
fn build_fixture_client<R>(
    resolver: R,
    limits: &Limits,
    root: reqwest::Certificate,
) -> Result<reqwest::Client, Error>
where
    R: reqwest::dns::Resolve + 'static,
{
    client_builder(resolver, limits)
        // Fixture trust is fully local; platform roots can perform host trust
        // evaluation outside the deterministic in-process test boundary.
        .tls_certs_only([root])
        .build()
        .map_err(|_| Error::InvalidConfiguration)
}

fn map_transport_error(error: &reqwest::Error) -> Error {
    if error.is_timeout() {
        return Error::Timeout;
    }
    let mut source = error.source();
    while let Some(current) = source {
        if let Some(resolve) = current.downcast_ref::<ResolveError>() {
            return match resolve {
                ResolveError::Denied => Error::Denied,
                ResolveError::Cancelled => Error::Cancelled,
                ResolveError::Configuration | ResolveError::Lookup => Error::Transport,
            };
        }
        source = current.source();
    }
    Error::Transport
}
