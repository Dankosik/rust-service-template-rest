use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::Full;
use hyper_util::client::legacy::connect::{
    HttpConnector,
    dns::{GaiAddrs, GaiFuture, GaiResolver, Name},
};
use parking_lot::Mutex;
use tower::Service;

use crate::{BuildError, Limits, policy::TargetOrigin};

const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(10);
static TLS_CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
static TLS_CONFIG_INIT: Mutex<()> = Mutex::new(());

pub(crate) type Transport = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<HttpConnector<Resolver>>,
    Full<Bytes>,
>;

#[derive(Clone)]
pub(crate) enum Resolver {
    Gai(GaiResolver),
    #[cfg(any(test, feature = "test-support"))]
    Fixture(FixtureResolver),
}

impl Resolver {
    pub(crate) fn new() -> Self {
        Self::Gai(GaiResolver::new())
    }
}

pub(crate) enum ResolvedAddrs {
    Gai(GaiAddrs),
    #[cfg(any(test, feature = "test-support"))]
    Fixture(std::vec::IntoIter<SocketAddr>),
}

impl Iterator for ResolvedAddrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Gai(addrs) => addrs.next(),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixture(addrs) => addrs.next(),
        }
    }
}

pub(crate) enum ResolveFuture {
    Gai(GaiFuture),
    #[cfg(any(test, feature = "test-support"))]
    Fixture(std::future::Ready<Result<std::vec::IntoIter<SocketAddr>, io::Error>>),
}

impl Future for ResolveFuture {
    type Output = Result<ResolvedAddrs, io::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut *self {
            Self::Gai(future) => Pin::new(future).poll(cx).map_ok(ResolvedAddrs::Gai),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixture(future) => Pin::new(future).poll(cx).map_ok(ResolvedAddrs::Fixture),
        }
    }
}

impl Service<Name> for Resolver {
    type Response = ResolvedAddrs;
    type Error = io::Error;
    type Future = ResolveFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self {
            Self::Gai(resolver) => resolver.poll_ready(cx),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixture(resolver) => resolver.poll_ready(cx),
        }
    }

    fn call(&mut self, name: Name) -> Self::Future {
        match self {
            Self::Gai(resolver) => ResolveFuture::Gai(resolver.call(name)),
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixture(resolver) => ResolveFuture::Fixture(resolver.call(name)),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub(crate) struct FixtureResolver(Arc<Mutex<Vec<SocketAddr>>>);

#[cfg(any(test, feature = "test-support"))]
impl FixtureResolver {
    pub(crate) fn with_addresses(addresses: Vec<SocketAddr>) -> Self {
        Self(Arc::new(Mutex::new(addresses)))
    }

    #[cfg(test)]
    pub(crate) fn replace(&self, addresses: Vec<SocketAddr>) {
        *self.0.lock() = addresses;
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Service<Name> for FixtureResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Name) -> Self::Future {
        let addresses = self.0.lock().clone();
        std::future::ready(if addresses.is_empty() {
            Err(io::Error::other("no fixture listener"))
        } else {
            Ok(addresses.into_iter())
        })
    }
}

pub async fn prepare_tls() -> Result<(), BuildError> {
    if TLS_CONFIG.get().is_none() {
        tokio::task::spawn_blocking(tls_config)
            .await
            .map_err(|source| BuildError::Tls {
                source: Box::new(source),
            })??;
    }
    Ok(())
}

pub(crate) fn tls_config() -> Result<&'static rustls::ClientConfig, BuildError> {
    if let Some(config) = TLS_CONFIG.get() {
        return Ok(config);
    }
    let _guard = TLS_CONFIG_INIT.lock();
    if let Some(config) = TLS_CONFIG.get() {
        return Ok(config);
    }
    let config = platform_tls_config().map_err(|source| BuildError::Tls {
        source: Box::new(source),
    })?;
    Ok(TLS_CONFIG.get_or_init(|| config))
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn tls_config_with_root(root_der: &[u8]) -> Result<rustls::ClientConfig, BuildError> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(root_der.to_vec()))
        .map_err(|source| BuildError::Tls {
            source: Box::new(source),
        })?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|source| BuildError::Tls {
            source: Box::new(source),
        })
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

fn platform_tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    use rustls_platform_verifier::BuilderVerifierExt as _;

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth())
}

pub(crate) fn build_transport(
    limits: &Limits,
    origin: &TargetOrigin,
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
    let https = match origin {
        TargetOrigin::Https(_) => https.https_only(),
        #[cfg(feature = "test-support")]
        TargetOrigin::TestHttp(_) => https.https_or_http(),
    };
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_timer(hyper_util::rt::TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .http1_max_headers(limits.response_header_count)
        .build(https.enable_http1().wrap_connector(http))
}
