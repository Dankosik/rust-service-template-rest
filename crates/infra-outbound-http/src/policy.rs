use std::{fmt, str::FromStr, sync::Arc};

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Uri, Version, header, uri::Scheme};

use crate::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("outbound HTTP origin is invalid")]
pub struct InvalidOrigin;

#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("{scheme}://{host}:{port}")]
pub struct Origin {
    scheme: Scheme,
    host: Arc<str>,
    port: u16,
    host_header: HeaderValue,
    uri: Uri,
}

impl Origin {
    pub fn parse_https(raw: &str) -> Result<Self, InvalidOrigin> {
        let uri = parse_origin_uri(raw)?;
        Self::from_uri_with_scheme(&uri, Scheme::HTTPS)
    }

    pub fn from_https_uri(uri: &Uri) -> Result<Self, InvalidOrigin> {
        Self::from_uri_with_scheme(uri, Scheme::HTTPS)
    }

    fn from_uri_with_scheme(uri: &Uri, scheme: Scheme) -> Result<Self, InvalidOrigin> {
        if uri.scheme() != Some(&scheme) {
            return Err(InvalidOrigin);
        }
        let authority = uri.authority().ok_or(InvalidOrigin)?;
        let raw_host = authority.host();
        let port =
            shared_rules::uri::http_origin_port(scheme.as_str(), authority.as_str(), raw_host)
                .ok_or(InvalidOrigin)?;
        let host = raw_host.to_ascii_lowercase();
        let default_port = if scheme == Scheme::HTTPS { 443 } else { 80 };
        let host_header = if port == default_port {
            HeaderValue::from_str(&host)
        } else {
            HeaderValue::from_str(&format!("{host}:{port}"))
        }
        .map_err(|_| InvalidOrigin)?;
        let canonical_uri = Uri::builder()
            .scheme(scheme.clone())
            .authority(host_header.as_bytes())
            .path_and_query("/")
            .build()
            .map_err(|_| InvalidOrigin)?;
        Ok(Self {
            scheme,
            host: Arc::from(host),
            port,
            host_header,
            uri: canonical_uri,
        })
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn is_https(&self) -> bool {
        self.scheme == Scheme::HTTPS
    }

    #[must_use]
    pub const fn as_uri(&self) -> &Uri {
        &self.uri
    }

    pub fn uri(&self, path_and_query: &str) -> Result<Uri, http::Error> {
        Uri::builder()
            .scheme(self.scheme.clone())
            .authority(self.host_header.as_bytes())
            .path_and_query(path_and_query)
            .build()
    }

    #[must_use]
    pub fn admits(&self, uri: &Uri) -> bool {
        uri.scheme() == Some(&self.scheme)
            && uri.authority().is_some_and(|authority| {
                let host = authority.host();
                host.eq_ignore_ascii_case(&self.host)
                    && shared_rules::uri::http_origin_port(
                        self.scheme.as_str(),
                        authority.as_str(),
                        host,
                    ) == Some(self.port)
            })
    }

    pub(crate) fn host_arc(&self) -> Arc<str> {
        Arc::clone(&self.host)
    }
}

impl FromStr for Origin {
    type Err = InvalidOrigin;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse_https(raw)
    }
}

impl From<&Origin> for Uri {
    fn from(origin: &Origin) -> Self {
        origin.uri.clone()
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}://{}",
            self.scheme,
            self.host_header.to_str().unwrap_or(&self.host)
        )
    }
}

fn parse_origin_uri(raw: &str) -> Result<Uri, InvalidOrigin> {
    if raw.contains('#')
        || shared_rules::uri::has_whitespace_or_control(raw)
        || shared_rules::uri::authority_has_userinfo(raw)
    {
        return Err(InvalidOrigin);
    }
    let uri = Uri::try_from(raw).map_err(|_| InvalidOrigin)?;
    if uri
        .path_and_query()
        .is_some_and(|pq| pq.query().is_some() || !matches!(pq.path(), "" | "/"))
    {
        return Err(InvalidOrigin);
    }
    Ok(uri)
}

#[cfg(feature = "test-support")]
pub(crate) fn admit_test_http_origin(raw: &str) -> Result<Origin, InvalidOrigin> {
    use std::net::{Ipv4Addr, Ipv6Addr};

    let uri = parse_origin_uri(raw)?;
    let authority = uri.authority().ok_or(InvalidOrigin)?;
    let raw_host = authority.host();
    let loopback = if let Some(v6) = raw_host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        v6.parse::<Ipv6Addr>().is_ok_and(|ip| ip.is_loopback())
    } else {
        raw_host
            .parse::<Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
    };
    if !loopback {
        return Err(InvalidOrigin);
    }
    Origin::from_uri_with_scheme(&uri, Scheme::HTTP)
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
    if length > 0 {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    } else if expects_content {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    } else {
        headers.remove(header::CONTENT_LENGTH);
    }
}

#[cfg(test)]
mod tests;
