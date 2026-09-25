//! A fixed-authority HTTPS client with finite exchange limits.
//!
//! The provider owns request content and interpretation. This crate owns
//! authority admission, bounded transport work, and static error semantics.

mod policy;

#[cfg(test)]
mod tests;

use std::{error::Error as StdError, fmt, sync::Arc, time::Duration};

use infra_egress_dns::{PublicAddressResolver, ResolveError, https_client_builder};
use tokio::{sync::Semaphore, time::Instant};
use url::Url;

pub use bytes::Bytes;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};

/// Fixed client ceilings. Every field is required and finite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_active: usize,
    pub operation_timeout: Duration,
    pub response_header_count: usize,
    pub response_header_bytes: usize,
    pub response_body_bytes: usize,
}

/// Request-specific custody supplied by the caller that owns the parent work.
#[derive(Clone)]
pub struct Operation {
    pub deadline: Instant,
    pub timeout: Option<Duration>,
    pub response_body_bytes: Option<usize>,
}

impl fmt::Debug for Operation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Operation([REDACTED])")
    }
}

/// Outbound exchange failures.
#[derive(Debug, thiserror::Error)]
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
    Timeout {
        #[source]
        source: Option<reqwest::Error>,
    },
    #[error("outbound HTTP response headers are too large")]
    ResponseHeadersTooLarge,
    #[error("outbound HTTP response body is too large")]
    ResponseBodyTooLarge,
    #[error("outbound DNS configuration failed")]
    ResolverConfiguration {
        #[source]
        source: ResolveError,
    },
    #[error("outbound HTTP client construction failed")]
    ClientBuild {
        #[source]
        source: reqwest::Error,
    },
    #[error("outbound HTTP transport failed")]
    Transport {
        #[source]
        source: reqwest::Error,
    },
}

/// A reusable bounded client. Its configured authority is never exposed.
///
/// ```no_run
/// use std::time::Duration;
///
/// use infra_outbound_http::{Bytes, Client, Limits, Operation};
/// use tokio::time::Instant;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::new(
///     "https://provider.example/v1",
///     Limits {
///         max_active: 8,
///         operation_timeout: Duration::from_secs(2),
///         response_header_count: 100,
///         response_header_bytes: 16 * 1024,
///         response_body_bytes: 1024 * 1024,
///     },
/// )?;
/// // A raw-string adapter rejects a literal `#` before constructing this URI;
/// // `%23` remains path or query data at the typed boundary.
/// let request = http::Request::get("/v1/items?q=a%2Fb")
///     .body(Bytes::new())?;
/// let response = client
///     .execute(
///         request,
///         Operation {
///             // The caller reserves time for work after the exchange.
///             deadline: Instant::now() + Duration::from_secs(3),
///             timeout: None,
///             response_body_bytes: None,
///         },
///     )
///     .await?;
/// assert!(response.status().is_success());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    base: Url,
    limits: Limits,
    transport: reqwest::Client,
    admission: Arc<Semaphore>,
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
    /// Returns a static configuration or denial error when limits or the base
    /// destination are invalid, and retains resolver or client setup causes.
    pub fn new(base: &str, limits: Limits) -> Result<Self, Error> {
        policy::validate_limits(&limits)?;
        let base = policy::admit_base(base)?;
        let resolver = PublicAddressResolver::new()
            .map_err(|source| Error::ResolverConfiguration { source })?;
        let transport = build_client(resolver, &limits)?;
        Ok(Self {
            base,
            limits,
            transport,
            admission: Arc::new(Semaphore::new(limits.max_active)),
            #[cfg(test)]
            response_head_observed: None,
        })
    }

    /// Executes one complete buffered exchange inside the supplied custody.
    ///
    /// The request URI must be origin-form. The parent deadline and optional
    /// narrower operation limits cover DNS through framed response completion.
    /// Dropping this future cancels its request-owned work and releases admission.
    ///
    /// # Errors
    ///
    /// Returns policy, capacity, timeout, response-size, or transport errors.
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        operation: Operation,
    ) -> Result<Response<Bytes>, Error> {
        let started = Instant::now();
        let (deadline, body_limit) = self.operation_limits(&operation, started)?;
        let (parts, body) = request.into_parts();
        let target = policy::admit_target(&self.base, &parts.uri)?;
        let headers = policy::admit_request_headers(parts.headers)?;
        if Instant::now() >= deadline {
            return Err(Error::Timeout { source: None });
        }

        let work = async {
            let permit = self
                .admission
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::AtCapacity)?;
            if Instant::now() >= deadline {
                return Err(Error::Timeout { source: None });
            }

            let response = self
                .transport
                .request(parts.method, target)
                .headers(headers)
                .body(body)
                .send()
                .await
                .map_err(map_transport_error)?;
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

            let status = response.status();
            let version = response.version();
            let headers = response.headers().clone();
            let mut response = response;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(map_transport_error)? {
                let remaining = body_limit.saturating_sub(body.len());
                if chunk.len() > remaining {
                    return Err(Error::ResponseBodyTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            drop(permit);

            let mut result = Response::new(Bytes::from(body));
            *result.status_mut() = status;
            *result.version_mut() = version;
            *result.headers_mut() = headers;
            Ok(result)
        };

        tokio::time::timeout_at(deadline, work)
            .await
            .map_err(|_| Error::Timeout { source: None })?
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
}

fn build_client<R>(resolver: R, limits: &Limits) -> Result<reqwest::Client, Error>
where
    R: reqwest::dns::Resolve + 'static,
{
    https_client_builder(
        resolver,
        limits.operation_timeout,
        limits.response_header_count,
        limits.max_active,
    )
    .build()
    .map_err(|source| Error::ClientBuild {
        source: source.without_url(),
    })
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
    https_client_builder(
        resolver,
        limits.operation_timeout,
        limits.response_header_count,
        limits.max_active,
    )
    // Fixture trust is fully local; platform roots can perform host trust
    // evaluation outside the deterministic in-process test boundary.
    .tls_certs_only([root])
    .build()
    .map_err(|source| Error::ClientBuild {
        source: source.without_url(),
    })
}

fn map_transport_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        return Error::Timeout {
            source: Some(error.without_url()),
        };
    }
    let mut source = error.source();
    while let Some(current) = source {
        if let Some(ResolveError::Denied) = current.downcast_ref::<ResolveError>() {
            return Error::Denied;
        }
        source = current.source();
    }
    Error::Transport {
        source: error.without_url(),
    }
}
