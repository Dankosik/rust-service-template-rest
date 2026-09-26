use tokio::{sync::Semaphore, time::Instant};
#[cfg(feature = "test-support")]
use url::Host;
use url::Url;

use crate::{Error, HeaderMap, Limits, header};

// http 1.5 HeaderMap supports at most 32,768 name/value entries. This also
// keeps the HTTP/1 parser header allocation within its representable range.
const MAX_HEADER_COUNT: usize = 32_768;

// Correlation fields stay with the inbound request. Compression negotiation is
// provider-owned, so callers' `Accept-Encoding` remains intact.
const REMOVED_REQUEST_HEADERS: [header::HeaderName; 4] = [
    header::HeaderName::from_static("traceparent"),
    header::HeaderName::from_static("tracestate"),
    header::HeaderName::from_static("baggage"),
    header::HeaderName::from_static("x-request-id"),
];

pub(crate) fn validate_limits(limits: &Limits) -> Result<(), Error> {
    if limits.max_active == 0
        || limits.max_active > Semaphore::MAX_PERMITS
        || limits.operation_timeout.is_zero()
        || limits.response_header_count == 0
        || limits.response_header_count > MAX_HEADER_COUNT
        || limits.response_body_bytes == 0
        || !is_representable_limit(limits.response_header_count)
        || !is_representable_limit(limits.response_body_bytes)
        || Instant::now()
            .checked_add(limits.operation_timeout)
            .is_none()
    {
        return Err(Error::InvalidConfiguration);
    }
    Ok(())
}

pub(crate) fn admit_base(raw: &str) -> Result<Url, Error> {
    reject_untrusted_url_text(raw, Error::InvalidConfiguration)?;
    let base = Url::parse(raw).map_err(|_| Error::InvalidConfiguration)?;
    // Each request path replaces the base path, so the base names only the
    // origin; a configured path would otherwise be silently ignored.
    if base.scheme() != "https"
        || base.host().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(Error::InvalidConfiguration);
    }
    Ok(base)
}

#[cfg(feature = "test-support")]
pub(crate) fn admit_test_http_base(raw: &str) -> Result<Url, Error> {
    reject_untrusted_url_text(raw, Error::InvalidConfiguration)?;
    let base = Url::parse(raw).map_err(|_| Error::InvalidConfiguration)?;
    if base.scheme() != "http"
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(Error::InvalidConfiguration);
    }
    match base.host() {
        Some(Host::Ipv4(address)) if address.is_loopback() => Ok(base),
        Some(Host::Ipv6(address)) if address.is_loopback() => Ok(base),
        _ => Err(Error::InvalidConfiguration),
    }
}

pub(crate) fn admit_target(base: &Url, uri: &http::Uri) -> Result<Url, Error> {
    if uri.scheme().is_some() || uri.authority().is_some() || uri.path() == "*" {
        return Err(Error::InvalidTarget);
    }
    let path_and_query = uri.path_and_query().ok_or(Error::InvalidTarget)?;
    if !path_and_query.path().starts_with('/') {
        return Err(Error::InvalidTarget);
    }
    let mut target = base.clone();
    target.set_path(uri.path());
    target.set_query(uri.query());
    Ok(target)
}

fn is_representable_limit(value: usize) -> bool {
    isize::try_from(value).is_ok()
}

fn reject_untrusted_url_text(raw: &str, error: Error) -> Result<(), Error> {
    if raw
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
        || raw_userinfo(raw)
    {
        return Err(error);
    }
    Ok(())
}

fn raw_userinfo(raw: &str) -> bool {
    let Some((_, after_scheme)) = raw.split_once("://") else {
        return false;
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    authority.contains('@')
}

pub(crate) fn admit_request_headers(mut headers: HeaderMap) -> Result<HeaderMap, Error> {
    if headers.contains_key(header::HOST) {
        return Err(Error::InvalidTarget);
    }
    for name in REMOVED_REQUEST_HEADERS {
        headers.remove(name);
    }
    Ok(headers)
}
