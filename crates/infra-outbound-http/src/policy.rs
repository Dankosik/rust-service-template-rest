use std::{fmt, sync::Arc};

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Uri, Version, header, uri::Scheme};
#[cfg(feature = "test-support")]
use url::Host;
use url::Url;

use crate::{BuildError, Error};

#[derive(Clone)]
pub(crate) struct Origin {
    scheme: Scheme,
    host: Arc<str>,
    port: u16,
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
        let host_header =
            HeaderValue::from_str(url.authority()).map_err(|_| BuildError::InvalidOrigin)?;
        Ok(Self {
            scheme,
            host: Arc::from(host),
            port,
            host_header,
        })
    }

    pub(crate) fn host_arc(&self) -> Arc<str> {
        Arc::clone(&self.host)
    }

    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn is_https(&self) -> bool {
        self.scheme == Scheme::HTTPS
    }

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

/// A caller `Host` could route to another virtual host, so it is refused. The host compares
/// ASCII case-insensitively with the origin's serialized host; other spellings of one
/// address, such as a non-canonical IP literal, are refused rather than normalized. The
/// version and body framing are then set whatever the caller wrote; a missing `Accept` gets
/// `*/*`, as reqwest sent.
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

/// Hyper sends a caller `Content-Length` as written, so a wrong value would leave bytes on a
/// pooled connection for the provider to read as the next request. Content, or a method that
/// expects it, carries its length (RFC 9110 section 8.6); any other carries none.
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
