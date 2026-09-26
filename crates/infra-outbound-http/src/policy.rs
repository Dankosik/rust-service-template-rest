use infra_egress_dns::admit_address;
use tokio::{sync::Semaphore, time::Instant};
use url::{Host, Url};

use crate::{Error, HeaderMap, Limits, header};

// http 1.5 HeaderMap supports at most 32,768 name/value entries. This also
// keeps the HTTP/1 parser header allocation within its representable range.
const MAX_HEADER_COUNT: usize = 32_768;

// Correlation fields stay with the inbound request; callers cannot negotiate
// response encoding here because decoding belongs to the provider adapter.
const REMOVED_REQUEST_HEADERS: [header::HeaderName; 5] = [
    header::HeaderName::from_static("traceparent"),
    header::HeaderName::from_static("tracestate"),
    header::HeaderName::from_static("baggage"),
    header::HeaderName::from_static("x-request-id"),
    header::ACCEPT_ENCODING,
];

pub(crate) fn validate_limits(limits: &Limits) -> Result<(), Error> {
    if limits.max_active == 0
        || limits.max_active > Semaphore::MAX_PERMITS
        || limits.operation_timeout.is_zero()
        || limits.response_header_count == 0
        || limits.response_header_count > MAX_HEADER_COUNT
        || limits.response_header_bytes == 0
        || limits.response_body_bytes == 0
        || !is_representable_limit(limits.response_header_count)
        || !is_representable_limit(limits.response_header_bytes)
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
    match base.host() {
        Some(Host::Ipv4(address)) => admit_address(address.into()).map_err(|_| Error::Denied)?,
        Some(Host::Ipv6(address)) => admit_address(address.into()).map_err(|_| Error::Denied)?,
        Some(Host::Domain(_)) => {}
        None => return Err(Error::InvalidConfiguration),
    }
    Ok(base)
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
    if raw != raw.trim() || raw.chars().any(char::is_control) {
        return Err(error);
    }
    Ok(())
}

pub(crate) fn admit_request_headers(mut headers: HeaderMap) -> Result<HeaderMap, Error> {
    if headers.contains_key(header::HOST) {
        return Err(Error::Denied);
    }
    for name in REMOVED_REQUEST_HEADERS {
        headers.remove(name);
    }
    Ok(headers)
}

pub(crate) fn admit_response_headers(
    headers: &HeaderMap,
    max_count: usize,
    max_bytes: usize,
) -> Result<(), Error> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (name, value) in headers {
        count = count.checked_add(1).ok_or(Error::ResponseHeadersTooLarge)?;
        let field_bytes = name
            .as_str()
            .len()
            .checked_add(value.as_bytes().len())
            .and_then(|size| size.checked_add(4))
            .ok_or(Error::ResponseHeadersTooLarge)?;
        bytes = bytes
            .checked_add(field_bytes)
            .ok_or(Error::ResponseHeadersTooLarge)?;
        if count > max_count || bytes > max_bytes {
            return Err(Error::ResponseHeadersTooLarge);
        }
    }
    Ok(())
}
