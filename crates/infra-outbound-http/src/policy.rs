#[cfg(feature = "test-support")]
use url::Host;
use url::{Origin, Url};

use crate::{Bytes, Error, Limits, Request, header};

// Hyper sizes a response `HeaderMap` from the parser header count, and
// http 1.5 `HeaderMap` panics above 32,768 entries.
const MAX_HEADER_COUNT: usize = 32_768;

pub(crate) fn validate_limits(limits: &Limits) -> Result<(), Error> {
    if limits.operation_timeout.is_zero()
        || limits.response_header_count == 0
        || limits.response_header_count > MAX_HEADER_COUNT
        || limits.response_body_bytes == 0
    {
        return Err(Error::InvalidConfiguration);
    }
    Ok(())
}

pub(crate) fn admit_origin(url: &Url) -> Result<Origin, Error> {
    if url.scheme() != "https" || url.host().is_none() || has_userinfo(url) {
        return Err(Error::InvalidConfiguration);
    }
    Ok(url.origin())
}

#[cfg(feature = "test-support")]
pub(crate) fn admit_test_http_origin(url: &Url) -> Result<Origin, Error> {
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.scheme() != "http" || !loopback || has_userinfo(url) {
        return Err(Error::InvalidConfiguration);
    }
    Ok(url.origin())
}

/// Converts an absolute request on the configured origin. A caller `Host`
/// header could route the request to another virtual host, so it is refused.
pub(crate) fn admit_request(
    origin: &Origin,
    request: Request<Bytes>,
) -> Result<reqwest::Request, Error> {
    if request.headers().contains_key(header::HOST) {
        return Err(Error::InvalidTarget);
    }
    let request = reqwest::Request::try_from(request).map_err(|_| Error::InvalidTarget)?;
    if !same_origin(origin, request.url()) || has_userinfo(request.url()) {
        return Err(Error::InvalidTarget);
    }
    Ok(request)
}

fn same_origin(origin: &Origin, url: &Url) -> bool {
    match origin {
        Origin::Tuple(scheme, host, port)
            if matches!(scheme.as_str(), "http" | "https")
                && matches!(url.scheme(), "http" | "https") =>
        {
            url.scheme() == scheme
                && url.host().is_some_and(|request_host| request_host == *host)
                && url.port_or_known_default() == Some(*port)
        }
        _ => url.origin() == *origin,
    }
}

fn has_userinfo(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

#[cfg(test)]
mod tests {
    use super::same_origin;
    use crate::Url;

    #[test]
    fn non_http_url_uses_origin_fallback() {
        let origin = Url::parse("https://authn.fixture.test/")
            .expect("configured origin")
            .origin();
        let blob = Url::parse("blob:https://authn.fixture.test/items").expect("blob URL");
        assert!(same_origin(&origin, &blob));
    }
}
