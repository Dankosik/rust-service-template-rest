//! Shared public-address DNS admission and hardened HTTPS construction.
//!
//! Consumers own their request and response policy. This crate only owns the
//! common connection-address and client-construction controls.

mod address;
mod runtime;
/// Generated TLS material for the consumers' real-TLS tests.
///
/// Only the default-off `test-support` feature compiles it; it adds no
/// production trust configuration or custom-root option.
#[cfg(feature = "test-support")]
pub mod test_support;

use std::{error::Error, time::Duration};

pub use address::{admit_address, admit_answers};
pub use runtime::PublicAddressResolver;

/// DNS outcomes exposed to the transport owners.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// System resolver configuration could not be read or applied.
    #[error("egress DNS configuration failed")]
    Configuration(#[source] Box<dyn Error + Send + Sync>),
    /// DNS lookup could not produce an answer set.
    #[error("egress DNS lookup failed")]
    Lookup(#[source] Box<dyn Error + Send + Sync>),
    /// The answer set was empty or included a non-public address.
    #[error("egress DNS address denied")]
    Denied,
}

/// Applies the shared HTTPS transport hardening around a DNS resolver.
///
/// Callers own validation of their policy values and any fixture-only trust
/// configuration before building the returned client.
pub fn https_client_builder<R>(
    resolver: R,
    timeout: Duration,
    max_headers: usize,
    max_idle_per_host: usize,
) -> reqwest::ClientBuilder
where
    R: reqwest::dns::Resolve + 'static,
{
    reqwest::Client::builder()
        .tls_backend_rustls()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .http1_only()
        .timeout(timeout)
        .http1_max_headers(max_headers)
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(max_idle_per_host)
        .dns_resolver(resolver)
}

#[cfg(test)]
mod tests {
    use std::{error::Error, io};

    use super::ResolveError;

    #[test]
    fn resolver_errors_keep_their_safe_source() {
        let errors = [
            (
                ResolveError::Configuration(Box::new(io::Error::other("system resolver"))),
                "egress DNS configuration failed",
            ),
            (
                ResolveError::Lookup(Box::new(io::Error::other("DNS lookup"))),
                "egress DNS lookup failed",
            ),
        ];

        for (error, message) in errors {
            assert_eq!(error.to_string(), message);
            assert!(error.source().is_some(), "{message} must retain its source");
        }
    }
}
