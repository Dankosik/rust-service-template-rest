use infra_egress_dns::admit_address;
use reqwest::header::{self, HeaderMap};
use tokio::{sync::Semaphore, time::Instant};
use url::{Host, Url};

use crate::{Error, Limits};

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
        || limits.request_header_count == 0
        || limits.request_header_bytes == 0
        || limits.request_header_count > MAX_HEADER_COUNT
        || limits.response_header_count == 0
        || limits.response_header_count > MAX_HEADER_COUNT
        || limits.response_header_bytes == 0
        || limits.request_body_bytes == 0
        || limits.response_body_bytes == 0
        || !is_representable_limit(limits.request_header_count)
        || !is_representable_limit(limits.request_header_bytes)
        || !is_representable_limit(limits.response_header_count)
        || !is_representable_limit(limits.response_header_bytes)
        || !is_representable_limit(limits.request_body_bytes)
        || !is_representable_limit(limits.response_body_bytes)
        || Instant::now()
            .checked_add(limits.operation_timeout)
            .is_none()
    {
        return Err(Error::InvalidConfiguration);
    }
    Ok(())
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Authority {
    host: Host<String>,
    port: u16,
}

pub(crate) fn admit_base(raw: &str) -> Result<(Url, Authority), Error> {
    reject_untrusted_url_text(raw, Error::InvalidConfiguration)?;
    let base = Url::parse(raw).map_err(|_| Error::InvalidConfiguration)?;
    if base.scheme() != "https"
        || base.host().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || raw_authority_contains_userinfo(raw)
    {
        return Err(Error::InvalidConfiguration);
    }
    let authority = authority(&base).ok_or(Error::InvalidConfiguration)?;
    match &authority.host {
        Host::Ipv4(address) => admit_address((*address).into()).map_err(|_| Error::Denied)?,
        Host::Ipv6(address) => admit_address((*address).into()).map_err(|_| Error::Denied)?,
        Host::Domain(_) => {}
    }
    Ok((base, authority))
}

pub(crate) fn admit_target(base: &Url, configured: &Authority, raw: &str) -> Result<Url, Error> {
    reject_untrusted_url_text(raw, Error::InvalidTarget)?;
    let target = match Url::parse(raw) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            base.join(raw).map_err(|_| Error::InvalidTarget)?
        }
        Err(_) => return Err(Error::InvalidTarget),
    };
    if target.scheme() != "https"
        || target.host().is_none()
        || target.fragment().is_some()
        || !target.username().is_empty()
        || target.password().is_some()
        || raw_authority_contains_userinfo(raw)
    {
        return Err(Error::InvalidTarget);
    }
    if authority(&target).as_ref() != Some(configured) {
        return Err(Error::Denied);
    }
    Ok(target)
}

fn authority(url: &Url) -> Option<Authority> {
    Some(Authority {
        host: url.host()?.to_owned(),
        port: url.port_or_known_default()?,
    })
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

fn raw_authority_contains_userinfo(raw: &str) -> bool {
    raw_authority(raw).is_some_and(|authority| authority.contains('@'))
}

fn raw_authority(raw: &str) -> Option<&str> {
    let authority = if let Some(rest) = raw
        .strip_prefix("//")
        .or_else(|| raw.strip_prefix("\\\\"))
        .or_else(|| raw.strip_prefix("/\\"))
        .or_else(|| raw.strip_prefix("\\/"))
    {
        rest.trim_start_matches(['/', '\\'])
    } else if let Some((scheme, rest)) = raw.split_once(':') {
        if scheme.eq_ignore_ascii_case("https") {
            rest.trim_start_matches(['/', '\\'])
        } else {
            return None;
        }
    } else {
        return None;
    };
    Some(
        authority
            .split(['/', '\\', '?', '#'])
            .next()
            .unwrap_or_default(),
    )
}

pub(crate) fn admit_request_headers(
    mut headers: HeaderMap,
    max_count: usize,
    max_bytes: usize,
) -> Result<HeaderMap, Error> {
    if headers.contains_key(header::HOST) {
        return Err(Error::Denied);
    }
    for name in REMOVED_REQUEST_HEADERS {
        headers.remove(name);
    }
    admit_headers(
        &headers,
        max_count,
        max_bytes,
        Error::RequestHeadersTooLarge,
    )?;
    Ok(headers)
}

pub(crate) fn admit_response_headers(
    headers: &HeaderMap,
    max_count: usize,
    max_bytes: usize,
) -> Result<(), Error> {
    admit_headers(
        headers,
        max_count,
        max_bytes,
        Error::ResponseHeadersTooLarge,
    )
}

fn admit_headers(
    headers: &HeaderMap,
    max_count: usize,
    max_bytes: usize,
    too_large: Error,
) -> Result<(), Error> {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for (name, value) in headers {
        count = count.checked_add(1).ok_or(too_large)?;
        let field_bytes = name
            .as_str()
            .len()
            .checked_add(value.as_bytes().len())
            .and_then(|size| size.checked_add(4))
            .ok_or(too_large)?;
        bytes = bytes.checked_add(field_bytes).ok_or(too_large)?;
        if count > max_count || bytes > max_bytes {
            return Err(too_large);
        }
    }
    Ok(())
}
