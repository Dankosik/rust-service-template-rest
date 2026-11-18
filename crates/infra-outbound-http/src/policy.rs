use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Uri, Version, header, uri::Scheme};
#[cfg(feature = "test-support")]
use url::Host;
use url::Url;

use crate::{BuildError, Error};

#[derive(Clone, derive_more::Debug)]
#[debug("{scheme}://{host}:{port}")]
pub(crate) struct Origin {
    scheme: Scheme,
    host: Arc<str>,
    port: u16,
    host_header: HeaderValue,
}

impl Origin {
    fn for_scheme(url: &Url, scheme: Scheme) -> Result<Self, BuildError> {
        if url.scheme() != scheme.as_str() || has_userinfo(url) {
            return Err(BuildError::InvalidOrigin);
        }
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

    pub(crate) fn admits(&self, uri: &Uri) -> bool {
        uri.scheme() == Some(&self.scheme)
            && uri.authority().is_some_and(|authority| {
                authority.host().eq_ignore_ascii_case(&self.host)
                    && shared_rules::uri::http_origin_port(
                        self.scheme.as_str(),
                        authority.as_str(),
                        authority.host(),
                    ) == Some(self.port)
            })
    }
}

pub(crate) fn admit_origin(url: &Url) -> Result<Origin, BuildError> {
    Origin::for_scheme(url, Scheme::HTTPS)
}

#[cfg(feature = "test-support")]
pub(crate) fn admit_test_http_origin(url: &Url) -> Result<Origin, BuildError> {
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !loopback {
        return Err(BuildError::InvalidOrigin);
    }
    Origin::for_scheme(url, Scheme::HTTP)
}

pub(crate) fn admit_request(
    origin: &Origin,
    mut request: Request<Bytes>,
) -> Result<Request<Bytes>, Error> {
    if request.headers().contains_key(header::HOST) || !origin.admits(request.uri()) {
        return Err(Error::InvalidTarget);
    }
    *request.version_mut() = Version::HTTP_11;
    frame_body(&mut request);
    let headers = request.headers_mut();
    headers.insert(header::HOST, origin.host_header.clone());
    headers
        .entry(header::ACCEPT)
        .or_insert(HeaderValue::from_static("*/*"));
    Ok(request)
}

/// RFC 9110 §8.6
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
