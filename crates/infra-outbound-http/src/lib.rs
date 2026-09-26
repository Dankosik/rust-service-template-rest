//! A fixed-authority HTTPS client with finite exchange limits.
//!
//! The provider owns request content and interpretation. This crate owns
//! authority admission, bounded transport work, and static error semantics.

mod observe;
mod policy;

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "../../../test/fixtures/tls.rs"]
mod tls;

use std::{fmt, sync::Arc, time::Duration};

use tokio::{sync::Semaphore, time::Instant};
use tracing::Instrument as _;
use url::Url;

pub use bytes::Bytes;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};

/// Fixed client ceilings. Every field is required and finite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_active: usize,
    pub operation_timeout: Duration,
    pub response_header_count: usize,
    pub response_body_bytes: usize,
}

/// Request-specific custody supplied by the caller that owns the parent work.
#[derive(Clone, Debug)]
pub struct Operation {
    pub deadline: Instant,
    pub response_body_bytes: Option<usize>,
}

/// Outbound exchange failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("outbound HTTP configuration is invalid")]
    InvalidConfiguration,
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    #[error("outbound HTTP is at capacity")]
    AtCapacity,
    #[error("outbound HTTP operation timed out")]
    Timeout {
        #[source]
        source: Option<reqwest::Error>,
    },
    #[error("outbound HTTP response body is too large")]
    ResponseBodyTooLarge,
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

/// A reusable bounded client. Its configured authority is never request data.
///
/// ```no_run
/// use std::time::Duration;
///
/// use infra_outbound_http::{Bytes, Client, Limits, Operation};
/// use tokio::time::Instant;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::new(
///     "https://provider.example",
///     Limits {
///         max_active: 8,
///         operation_timeout: Duration::from_secs(2),
///         response_header_count: 100,
///         response_body_bytes: 1024 * 1024,
///     },
/// )?;
/// let request = http::Request::get("/v1/items?q=a%2Fb").body(Bytes::new())?;
/// let response = client
///     .execute(
///         request,
///         Operation {
///             deadline: Instant::now() + Duration::from_secs(3),
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
        formatter
            .debug_struct("Client")
            .field("base", &self.base)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a fixed-authority client without performing DNS or network I/O.
    ///
    /// # Errors
    ///
    /// Returns a static configuration error when limits or the configured
    /// origin are invalid, and retains client setup causes.
    pub fn new(base: &str, limits: Limits) -> Result<Self, Error> {
        policy::validate_limits(&limits)?;
        let base = policy::admit_base(base)?;
        let transport = build_client(&limits, true)?;
        Ok(Self {
            base,
            limits,
            transport,
            admission: Arc::new(Semaphore::new(limits.max_active)),
            #[cfg(test)]
            response_head_observed: None,
        })
    }

    /// Creates the narrow local-HTTP constructor used by downstream mock
    /// tests. Production code must use [`Client::new`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] unless `base` is a literal
    /// loopback HTTP origin with the same origin grammar as production.
    #[cfg(feature = "test-support")]
    pub fn new_for_test_http(base: &str, limits: Limits) -> Result<Self, Error> {
        policy::validate_limits(&limits)?;
        let base = policy::admit_test_http_base(base)?;
        let transport = build_client(&limits, false)?;
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
    /// The request URI must be origin-form. The caller deadline and client
    /// timeout cover DNS through framed response completion. Dropping this
    /// future releases admission; it does not undo a provider-side effect.
    ///
    /// # Errors
    ///
    /// Returns policy, capacity, timeout, response-size, or transport errors.
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        operation: Operation,
    ) -> Result<Response<Bytes>, Error> {
        let (parts, body) = request.into_parts();
        let mut attempt = observe::Attempt::start(&parts.method, &self.base);
        let span = attempt.span();
        let result = async {
            let started = Instant::now();
            let (deadline, body_limit) = self.operation_limits(&operation, started)?;
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
                let response = self
                    .transport
                    .request(parts.method, target)
                    .headers(headers)
                    .body(body)
                    .send()
                    .await
                    .map_err(map_transport_error)?;
                attempt.response_headers(response.status());
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
        .instrument(span)
        .await;
        attempt.finish(&result);
        result
    }

    fn operation_limits(
        &self,
        operation: &Operation,
        started: Instant,
    ) -> Result<(Instant, usize), Error> {
        let body_limit = operation
            .response_body_bytes
            .unwrap_or(self.limits.response_body_bytes);
        if body_limit == 0 || body_limit > self.limits.response_body_bytes {
            return Err(Error::InvalidConfiguration);
        }
        let local_deadline = started
            .checked_add(self.limits.operation_timeout)
            .ok_or(Error::InvalidConfiguration)?;
        Ok((operation.deadline.min(local_deadline), body_limit))
    }
}

fn build_client(limits: &Limits, https_only: bool) -> Result<reqwest::Client, Error> {
    client_builder(limits, https_only)
        .build()
        .map_err(|source| Error::ClientBuild {
            source: source.without_url(),
        })
}

fn client_builder(limits: &Limits, https_only: bool) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(https_only)
        .no_hickory_dns()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .http1_only()
        .http1_max_headers(limits.response_header_count)
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(limits.max_active)
        .timeout(limits.operation_timeout)
}

#[cfg(test)]
fn build_fixture_client(
    host: &str,
    address: std::net::SocketAddr,
    limits: &Limits,
    root: reqwest::Certificate,
) -> Result<reqwest::Client, Error> {
    client_builder(limits, true)
        .resolve(host, address)
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
    Error::Transport {
        source: error.without_url(),
    }
}
