use std::fmt;

use http::{HeaderValue, Method, Version, uri::Scheme};
#[cfg(feature = "test-support")]
use url::Host;
use url::Url;

use crate::{BuildError, Bytes, Error, Limits, Request, header};

// Hyper sizes a response `HeaderMap` from the parser header count, and
// http 1.5 `HeaderMap` panics above 32,768 entries.
const MAX_HEADER_COUNT: usize = 32_768;

pub(crate) fn validate_limits(limits: &Limits) -> Result<(), BuildError> {
    if limits.operation_timeout.is_zero()
        || limits.response_header_count == 0
        || limits.response_header_count > MAX_HEADER_COUNT
        || limits.response_body_bytes == 0
    {
        return Err(BuildError::InvalidConfiguration);
    }
    Ok(())
}

/// The configured origin in the form an absolute request URI is compared
/// against: scheme, serialized host, and effective port.
#[derive(Clone)]
pub(crate) struct Target {
    scheme: Scheme,
    host: Box<str>,
    port: u16,
    /// The `Host` value hyper would derive from every admitted URI. The origin
    /// is fixed, so it is formatted once.
    host_header: HeaderValue,
}

impl Target {
    fn new(url: &Url) -> Result<Self, BuildError> {
        let (scheme, default_port) = match url.scheme() {
            "https" => (Scheme::HTTPS, 443),
            "http" => (Scheme::HTTP, 80),
            _ => return Err(BuildError::InvalidConfiguration),
        };
        let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
            return Err(BuildError::InvalidConfiguration);
        };
        let host_header = if port == default_port {
            HeaderValue::from_str(host)
        } else {
            HeaderValue::from_str(&format!("{host}:{port}"))
        }
        .map_err(|_| BuildError::InvalidConfiguration)?;
        Ok(Self {
            scheme,
            host: host.into(),
            port,
            host_header,
        })
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

pub(crate) fn admit_origin(url: &Url) -> Result<Target, BuildError> {
    if url.scheme() != "https" || has_userinfo(url) {
        return Err(BuildError::InvalidConfiguration);
    }
    Target::new(url)
}

#[cfg(feature = "test-support")]
pub(crate) fn admit_test_http_origin(url: &Url) -> Result<Target, BuildError> {
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http" || !loopback || has_userinfo(url) {
        return Err(BuildError::InvalidConfiguration);
    }
    Target::new(url)
}

/// Admits an absolute request on the configured origin and sets its `Host`.
/// A caller `Host` header could route the request to another virtual host,
/// so it is refused. The host compares ASCII case-insensitively with the
/// origin's serialized host; any other spelling of the same address, such as
/// a non-canonical IP literal, is refused rather than normalized.
///
/// The transport's own request properties are then set, whatever the caller
/// wrote: the version it speaks and the framing of the buffered body.
pub(crate) fn admit_request(
    target: &Target,
    mut request: Request<Bytes>,
) -> Result<Request<Bytes>, Error> {
    let uri = request.uri();
    let default_port = if target.scheme == Scheme::HTTPS {
        443
    } else {
        80
    };
    let admitted = !request.headers().contains_key(header::HOST)
        && uri.scheme() == Some(&target.scheme)
        && uri.authority().is_some_and(|authority| {
            !authority.as_str().contains('@')
                && authority.host().eq_ignore_ascii_case(&target.host)
                && authority.port_u16().unwrap_or(default_port) == target.port
        });
    if !admitted {
        return Err(Error::InvalidTarget);
    }
    // Hyper connects before it refuses a version it cannot send.
    *request.version_mut() = Version::HTTP_11;
    frame_body(&mut request);
    let headers = request.headers_mut();
    headers.insert(header::HOST, target.host_header.clone());
    headers
        .entry(header::ACCEPT)
        .or_insert(HeaderValue::from_static("*/*"));
    Ok(request)
}

/// States the length of the buffered body. Hyper sends a caller
/// `Content-Length` as written, so a value other than the body's length would
/// leave bytes on a pooled connection for the provider to read as the next
/// request. A request with content, or whose method expects content, carries
/// its length (RFC 9110 section 8.6); any other carries none.
fn frame_body(request: &mut Request<Bytes>) {
    let length = request.body().len();
    let expects_content = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH
    );
    let headers = request.headers_mut();
    headers.remove(header::TRANSFER_ENCODING);
    if length > 0 || expects_content {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    } else {
        headers.remove(header::CONTENT_LENGTH);
    }
}

fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}
