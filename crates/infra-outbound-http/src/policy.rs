use std::fmt;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Uri, Version, header, uri::Scheme};
#[cfg(feature = "test-support")]
use url::Host;
use url::Url;

use crate::{BuildError, Error};

/// The configured origin in the form an absolute request URI is compared
/// against: scheme, serialized host, and effective port.
#[derive(Clone)]
pub(crate) struct Origin {
    scheme: Scheme,
    host: Box<str>,
    port: u16,
    /// The `Host` value hyper would derive from every admitted URI. The origin
    /// is fixed, so it is formatted once.
    host_header: HeaderValue,
}

impl Origin {
    fn try_new(url: &Url) -> Result<Self, BuildError> {
        let scheme = match url.scheme() {
            "https" => Scheme::HTTPS,
            "http" => Scheme::HTTP,
            _ => return Err(BuildError::InvalidOrigin),
        };
        let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
            return Err(BuildError::InvalidOrigin);
        };
        // `Url::port` omits the scheme's default port, as the `Host` header does.
        let host_header = match url.port() {
            Some(port) => HeaderValue::from_str(&format!("{host}:{port}")),
            None => HeaderValue::from_str(host),
        }
        .map_err(|_| BuildError::InvalidOrigin)?;
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

    pub(crate) fn is_https(&self) -> bool {
        self.scheme == Scheme::HTTPS
    }

    /// Whether `uri` names this origin: the same scheme, no userinfo, the
    /// host ASCII case-insensitively, and the port, defaulted by scheme.
    fn admits(&self, uri: &Uri) -> bool {
        let default_port = if self.is_https() { 443 } else { 80 };
        uri.scheme() == Some(&self.scheme)
            && uri.authority().is_some_and(|authority| {
                !authority.as_str().contains('@')
                    && authority.host().eq_ignore_ascii_case(&self.host)
                    && authority.port_u16().unwrap_or(default_port) == self.port
            })
    }
}

impl fmt::Debug for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

pub(crate) fn admit_origin(url: &Url) -> Result<Origin, BuildError> {
    if url.scheme() != "https" || has_userinfo(url) {
        return Err(BuildError::InvalidOrigin);
    }
    Origin::try_new(url)
}

#[cfg(feature = "test-support")]
pub(crate) fn admit_test_http_origin(url: &Url) -> Result<Origin, BuildError> {
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http" || !loopback || has_userinfo(url) {
        return Err(BuildError::InvalidOrigin);
    }
    Origin::try_new(url)
}

/// Admits an absolute request on the configured origin and sets its `Host`.
/// A caller `Host` header could route the request to another virtual host,
/// so it is refused. The host compares ASCII case-insensitively with the
/// origin's serialized host; any other spelling of the same address, such as
/// a non-canonical IP literal, is refused rather than normalized.
///
/// The transport's own request properties are then set, whatever the caller
/// wrote: the version it speaks and the framing of the buffered body. A
/// request without `Accept` gets `*/*`, the value reqwest sent.
pub(crate) fn admit_request(
    origin: &Origin,
    mut request: Request<Bytes>,
) -> Result<Request<Bytes>, Error> {
    if request.headers().contains_key(header::HOST) || !origin.admits(request.uri()) {
        return Err(Error::InvalidTarget);
    }
    // Hyper connects before it refuses a version it cannot send.
    *request.version_mut() = Version::HTTP_11;
    frame_body(&mut request);
    let headers = request.headers_mut();
    headers.insert(header::HOST, origin.host_header.clone());
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

#[cfg(test)]
mod tests;
