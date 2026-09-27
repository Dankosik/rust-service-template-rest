//! A fixed-origin HTTPS client with finite exchange limits.
//!
//! The provider owns request content and interpretation. This crate owns
//! origin admission, bounded transport work, and static error semantics.

mod observe;
mod policy;

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "../../../test/fixtures/tls.rs"]
mod tls;

use std::{fmt, time::Duration};

use http_body_util::{BodyExt as _, Limited};
use tokio::time::Instant;
use tracing::Instrument as _;

pub use bytes::Bytes;
pub use http::{HeaderMap, Method, Request, Response, StatusCode, Version, header};
pub use observe::{REQUEST_DURATION_BUCKETS, REQUEST_DURATION_METRIC};
pub use url::Url;

/// Fixed client ceilings. Every field is required and positive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub operation_timeout: Duration,
    pub response_header_count: usize,
    pub response_body_bytes: usize,
}

/// Outbound exchange failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("outbound HTTP configuration is invalid")]
    InvalidConfiguration,
    #[error("outbound HTTP target is invalid")]
    InvalidTarget,
    #[error("outbound HTTP operation timed out")]
    Timeout,
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

/// A reusable bounded client for one configured origin. Requests name
/// absolute URLs on that origin; any other origin is refused before I/O.
///
/// ```no_run
/// use std::time::Duration;
///
/// use infra_outbound_http::{Bytes, Client, Limits, Url};
/// use tokio::time::Instant;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = Client::new(
///     &Url::parse("https://provider.example")?,
///     Limits {
///         operation_timeout: Duration::from_secs(2),
///         response_header_count: 100,
///         response_body_bytes: 1024 * 1024,
///     },
/// )?;
/// let request = http::Request::get("https://provider.example/v1/items?q=a%2Fb")
///     .body(Bytes::new())?;
/// let response = client
///     .execute(request, Instant::now() + Duration::from_secs(3))
///     .await?;
/// assert!(response.status().is_success());
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Client {
    origin: url::Origin,
    limits: Limits,
    transport: reqwest::Client,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("origin", &self.origin)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Creates a client bound to the origin (scheme, host, and port) of
    /// `origin`. Its path, query, and fragment are ignored. Construction
    /// performs no DNS or network I/O.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] for invalid limits or a
    /// non-HTTPS URL, a URL without a host, or a URL with userinfo.
    pub fn new(origin: &Url, limits: Limits) -> Result<Self, Error> {
        policy::validate_limits(&limits)?;
        let origin = policy::admit_origin(origin)?;
        Ok(Self {
            origin,
            limits,
            transport: build_client(&limits, true)?,
        })
    }

    /// Creates the narrow local-HTTP constructor used by downstream mock
    /// tests. Production code must use [`Client::new`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfiguration`] unless `origin` is an `http`
    /// URL whose host is a literal loopback IP address.
    #[cfg(feature = "test-support")]
    pub fn new_for_test_http(origin: &Url, limits: Limits) -> Result<Self, Error> {
        policy::validate_limits(&limits)?;
        let origin = policy::admit_test_http_origin(origin)?;
        Ok(Self {
            origin,
            limits,
            transport: build_client(&limits, false)?,
        })
    }

    /// Executes one complete buffered exchange before `deadline`.
    ///
    /// The exchange ends at the earlier of `deadline` and its start plus
    /// [`Limits::operation_timeout`]; that timeout covers DNS through the last
    /// body byte. Dropping this future ends the exchange; it does not undo a
    /// provider-side effect.
    ///
    /// # Errors
    ///
    /// Returns target, timeout, response-size, or transport errors.
    pub async fn execute(
        &self,
        request: Request<Bytes>,
        deadline: Instant,
    ) -> Result<Response<Bytes>, Error> {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(self.limits.operation_timeout);
        if timeout.is_zero() {
            return Err(Error::Timeout);
        }
        let mut request = policy::admit_request(&self.origin, request)?;
        *request.timeout_mut() = Some(timeout);

        let mut attempt = observe::Attempt::start(request.method(), &self.origin);
        let result = self.exchange(request, &mut attempt).await;
        attempt.finish(&result);
        result
    }

    async fn exchange(
        &self,
        request: reqwest::Request,
        attempt: &mut observe::Attempt,
    ) -> Result<Response<Bytes>, Error> {
        let span = attempt.span();
        async {
            let response = self
                .transport
                .execute(request)
                .await
                .map_err(map_transport_error)?;
            attempt.response_headers(response.status());
            let body_limit = self.limits.response_body_bytes;
            if response
                .content_length()
                .is_some_and(|length| length > body_limit as u64)
            {
                return Err(Error::ResponseBodyTooLarge);
            }

            let (parts, body) = http::Response::from(response).into_parts();
            let body = Limited::new(body, body_limit)
                .collect()
                .await
                .map_err(map_body_error)?
                .to_bytes();
            Ok(Response::from_parts(parts, body))
        }
        .instrument(span)
        .await
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
        return Error::Timeout;
    }
    Error::Transport {
        source: error.without_url(),
    }
}

// `Limited` yields either the reqwest body error or its own length error.
fn map_body_error(error: Box<dyn std::error::Error + Send + Sync>) -> Error {
    match error.downcast::<reqwest::Error>() {
        Ok(error) => map_transport_error(*error),
        Err(_) => Error::ResponseBodyTooLarge,
    }
}
