use std::{sync::OnceLock, time::Duration};

use bytes::Bytes;
use http_body_util::Full;
use hyper_util::client::legacy::connect::HttpConnector;

use crate::{BuildError, Limits, policy::Origin};

const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(10);

pub(crate) type Transport = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<HttpConnector<Resolver>>,
    Full<Bytes>,
>;

#[cfg(not(test))]
#[expect(
    clippy::cfg_not_test,
    reason = "unit tests substitute the deterministic FixtureResolver without altering the Client type"
)]
pub(crate) type Resolver = hyper_util::client::legacy::connect::dns::GaiResolver;
#[cfg(test)]
pub(crate) type Resolver = crate::test_support::FixtureResolver;

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
    http.set_connect_timeout(Some(
        (limits.operation_timeout / 2).min(CONNECT_TIMEOUT_CAP),
    ));
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
        .pool_idle_timeout(Duration::from_secs(30))
        .http1_max_headers(limits.response_header_count)
        .build(https.enable_http1().wrap_connector(http))
}
