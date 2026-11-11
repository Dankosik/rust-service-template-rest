use std::{sync::OnceLock, time::Duration};

use bytes::Bytes;
use http_body_util::Full;
use hyper_util::client::legacy::connect::HttpConnector;

use crate::{BuildError, Limits, policy::Origin};

/// The longest TCP connect budget, whatever the operation timeout.
const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(10);

pub(crate) type Transport = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<HttpConnector<Resolver>>,
    Full<Bytes>,
>;

// System DNS; tests route fixture names to loopback listeners instead.
#[cfg(not(test))]
pub(crate) type Resolver = hyper_util::client::legacy::connect::dns::GaiResolver;
#[cfg(test)]
pub(crate) type Resolver = crate::test_support::FixtureResolver;

/// The process-wide TLS client configuration, shared because building the
/// platform verifier is costly; when first builds race, the losers are
/// discarded. On Linux, its platform verifier snapshots system roots at
/// construction; other platforms use OS-specific verification. Clients share
/// its session cache, keyed by server name.
pub(crate) fn tls_config() -> Result<&'static rustls::ClientConfig, BuildError> {
    use rustls_platform_verifier::BuilderVerifierExt as _;

    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(config);
    }
    let build_error = |source: rustls::Error| BuildError::Tls {
        source: Box::new(source),
    };
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(build_error)?
    .with_platform_verifier()
    .map_err(build_error)?
    .with_no_client_auth();
    Ok(CONFIG.get_or_init(|| config))
}

pub(crate) fn build_transport(
    limits: &Limits,
    origin: &Origin,
    tls: rustls::ClientConfig,
    resolver: Resolver,
) -> Transport {
    let mut http = HttpConnector::new_with_resolver(resolver);
    http.enforce_http(false);
    // Half the operation budget, so a stalled connect is reported as `connect`
    // while time remains for TLS, the request, and the response.
    // Hyper-util divides this among the resolved addresses of one family. With
    // Happy Eyeballs on, its default, the vendored patch races those addresses
    // (vendor/hyper-util/PATCHES.md), so one that never answers does not hold
    // back the rest.
    http.set_connect_timeout(Some(
        (limits.operation_timeout / 2).min(CONNECT_TIMEOUT_CAP),
    ));
    // The socket options keep the reqwest 0.12 defaults this client replaced.
    http.set_nodelay(true);
    http.set_keepalive(Some(Duration::from_secs(15)));
    http.set_keepalive_interval(Some(Duration::from_secs(15)));
    http.set_keepalive_retries(Some(3));
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    http.set_tcp_user_timeout(Some(Duration::from_secs(30)));
    let https = hyper_rustls::HttpsConnectorBuilder::new().with_tls_config(tls);
    let https = if origin.is_https() {
        https.https_only()
    } else {
        https.https_or_http()
    };
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_timer(hyper_util::rt::TokioTimer::new())
        // Below common server and load-balancer keep-alive idle timeouts
        // (60 s and more), so the client retires a connection before the
        // provider closes it under a new request.
        .pool_idle_timeout(Duration::from_secs(30))
        .http1_max_headers(limits.response_header_count)
        .build(https.enable_http1().wrap_connector(http))
}
