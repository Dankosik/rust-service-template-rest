#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests assert Valkey behavior and own their fixture setup"
)]

#[path = "support/valkey_auth.rs"]
mod valkey_auth;

use operation_context::OperationContext;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use infra_cache::{Cache, CacheOptions, ClientCertificate, HISTOGRAMS, OperationError, namespace};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use secrecy::SecretString;
use tls_test_support::{TlsMaterial, new_issuer};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_util::task::AbortOnDropHandle;

const COMMAND_TIMEOUT: Duration = Duration::from_millis(200);
const TIMEOUT_BOUND: Duration = Duration::from_millis(450);
const RECOVERY_DEADLINE: Duration = Duration::from_secs(5);

static KEYS: AtomicU64 = AtomicU64::new(0);

fn cache_url() -> String {
    std::env::var("CACHE_URL")
        .unwrap_or_else(|_| panic!("CACHE_URL is unset; run scripts/profiles/integration.sh cache"))
}

fn upstream_addr(url: &str) -> SocketAddr {
    let Ok(info) = redis::IntoConnectionInfo::into_connection_info(url) else {
        panic!(
            "CACHE_URL must be a plaintext redis://host:port with no password; run scripts/profiles/integration.sh cache"
        );
    };
    let settings = info.redis_settings();
    let (redis::ConnectionAddr::Tcp(host, port), None, None) =
        (info.addr(), settings.username(), settings.password())
    else {
        panic!(
            "CACHE_URL must be a plaintext redis://host:port with no password; run scripts/profiles/integration.sh cache"
        );
    };
    let ip = host
        .trim_matches(['[', ']'])
        .parse::<IpAddr>()
        .unwrap_or_else(|_| {
            panic!(
                "CACHE_URL host:port could not be parsed; run scripts/profiles/integration.sh cache"
            )
        });
    SocketAddr::new(ip, *port)
}

fn unique_key() -> String {
    let n = KEYS.fetch_add(1, Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

fn options(
    dsn: impl Into<String>,
    command_timeout: Duration,
    root_ca: Option<std::path::PathBuf>,
) -> CacheOptions {
    CacheOptions {
        dsn: SecretString::from(dsn.into()),
        password_file: None,
        root_ca_path: root_ca,
        client_certificate: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        command_timeout,
        max_key_bytes: 1024,
        max_concurrency: 256,
        max_value_bytes: 64 * 1024,
    }
}

async fn connect_url(url: &str, command_timeout: Duration) -> Cache {
    Cache::start(options(url, command_timeout, None))
        .await
        .expect("admit cache DSN")
}

struct Proxy {
    port: u16,
    task: AbortOnDropHandle<()>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

#[warn(clippy::let_underscore_must_use)]
impl Proxy {
    async fn plain(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("proxy bind");
        let port = listener.local_addr().expect("proxy port").port();
        Self::spawn(listener, port, upstream, None)
    }

    async fn tls(upstream: SocketAddr, acceptor: TlsAcceptor) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("tls proxy bind");
        let port = listener.local_addr().expect("tls proxy port").port();
        Self::spawn(listener, port, upstream, Some(acceptor))
    }

    fn spawn(
        listener: TcpListener,
        port: u16,
        upstream: SocketAddr,
        acceptor: Option<TlsAcceptor>,
    ) -> Self {
        let (stop, stop_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(run_proxy(listener, upstream, acceptor, stop_rx));
        Self {
            port,
            task: AbortOnDropHandle::new(task),
            stop: Some(stop),
        }
    }

    async fn stop(&mut self) {
        if self.stop.take().is_some() {
            tokio::time::timeout(Duration::from_secs(1), &mut self.task)
                .await
                .expect("Valkey proxy did not stop")
                .expect("Valkey proxy task failed");
        }
    }

    async fn restart_plain(&mut self, upstream: SocketAddr) {
        self.stop().await;
        let listener = bind_port(Ipv4Addr::LOCALHOST.into(), self.port).await;
        *self = Self::spawn(listener, self.port, upstream, None);
    }

    async fn restart_tls(&mut self, upstream: SocketAddr, acceptor: TlsAcceptor) {
        self.stop().await;
        let listener = bind_port(Ipv4Addr::LOCALHOST.into(), self.port).await;
        *self = Self::spawn(listener, self.port, upstream, Some(acceptor));
    }
}

async fn bind_port(host: IpAddr, port: u16) -> TcpListener {
    let addr = SocketAddr::new(host, port);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match TcpListener::bind(addr).await {
            Ok(listener) => return listener,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("rebind {addr}: {error}"),
        }
    }
}

async fn run_proxy(
    listener: TcpListener,
    upstream: SocketAddr,
    acceptor: Option<TlsAcceptor>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                let Ok((inbound, _)) = accepted else { break };
                let acceptor = acceptor.clone();
                connections.spawn(async move {
                    proxy_one(inbound, upstream, acceptor).await;
                });
            }
        }
    }
}

async fn proxy_one(mut inbound: TcpStream, upstream: SocketAddr, acceptor: Option<TlsAcceptor>) {
    let Ok(mut outbound) = TcpStream::connect(upstream).await else {
        return;
    };
    match acceptor {
        None => {
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        }
        Some(acceptor) => {
            let Ok(mut inbound) = acceptor.accept(inbound).await else {
                return;
            };
            let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
        }
    }
}

fn tls_acceptor(material: &TlsMaterial) -> TlsAcceptor {
    acceptor(material, None)
}

fn acceptor(material: &TlsMaterial, client_ca: Option<&[u8]>) -> TlsAcceptor {
    let provider = Arc::new(tokio_rustls::rustls::crypto::aws_lc_rs::default_provider());
    let builder = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("fixture TLS protocol versions");
    let builder = match client_ca {
        None => builder.with_no_client_auth(),
        Some(client_ca) => {
            let mut roots = RootCertStore::empty();
            roots
                .add(CertificateDer::from(client_ca.to_vec()))
                .expect("fixture client CA");
            builder.with_client_cert_verifier(
                WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                    .build()
                    .expect("fixture client verifier"),
            )
        }
    };
    let config = builder
        .with_single_cert(
            vec![CertificateDer::from(material.cert.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key.clone())),
        )
        .expect("fixture certificate and key");
    TlsAcceptor::from(Arc::new(config))
}

fn write_pem(der: &[u8]) -> tempfile::NamedTempFile {
    write_pem_block("CERTIFICATE", der)
}

fn write_pem_block(label: &str, der: &[u8]) -> tempfile::NamedTempFile {
    let pem = pem_block(label, der);
    let mut file = tempfile::NamedTempFile::new().expect("pem tempfile");
    std::io::Write::write_all(&mut file, pem.as_bytes()).expect("write pem");
    file
}

fn pem_block(label: &str, der: &[u8]) -> String {
    pem::encode_config(
        &pem::Pem::new(label, der),
        pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
    )
}

struct ClientIdentity {
    ca: Vec<u8>,
    cert: tempfile::NamedTempFile,
    key: tempfile::NamedTempFile,
}

impl ClientIdentity {
    fn new() -> Self {
        let mut ca = rcgen::CertificateParams::default();
        ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
        let issuer = rcgen::CertifiedIssuer::self_signed(
            ca,
            rcgen::KeyPair::generate().expect("client CA key"),
        )
        .expect("client CA");
        let key = rcgen::KeyPair::generate().expect("client key");
        let mut leaf = rcgen::CertificateParams::new(vec!["cache-client".to_owned()])
            .expect("client certificate params");
        leaf.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
        let certificate = leaf.signed_by(&key, &issuer).expect("client certificate");
        Self {
            ca: issuer.der().to_vec(),
            cert: write_pem(certificate.der()),
            key: write_pem_block("PRIVATE KEY", &key.serialize_der()),
        }
    }

    fn paths(&self) -> ClientCertificate {
        ClientCertificate {
            cert_path: self.cert.path().to_path_buf(),
            key_path: self.key.path().to_path_buf(),
        }
    }
}

#[tokio::test]
async fn roundtrip_hit_miss_and_delete_are_scoped_to_the_namespace() {
    let caller = OperationContext::unbounded();
    let cache = connect_url(&cache_url(), Duration::from_secs(1)).await;
    let alpha = cache.namespace(namespace!("alpha"));
    let beta = cache.namespace(namespace!("beta"));
    for key in [unique_key(), format!("{}:\0\r\nюникод", unique_key())] {
        assert_eq!(alpha.get(&caller, &key).await.unwrap(), None);
        alpha
            .set(&caller, &key, b"value", Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(
            alpha.get(&caller, &key).await.unwrap().as_deref(),
            Some(&b"value"[..])
        );
        assert_eq!(beta.get(&caller, &key).await.unwrap(), None);
        alpha.delete(&caller, &key).await.unwrap();
        assert_eq!(alpha.get(&caller, &key).await.unwrap(), None);
        alpha.delete(&caller, &key).await.unwrap();
    }
}

#[tokio::test]
async fn bounded_reads_detect_oversized_values_written_by_another_client() {
    let caller = OperationContext::unbounded();
    let url = cache_url();
    let writer = connect_url(&url, COMMAND_TIMEOUT)
        .await
        .namespace(namespace!("bounded"));
    let reader = Cache::start(CacheOptions {
        max_value_bytes: 4,
        ..options(&url, COMMAND_TIMEOUT, None)
    })
    .await
    .unwrap()
    .namespace(namespace!("bounded"));
    let key = unique_key();
    let large_value = vec![0; 64 * 1024];
    for value in [&b""[..], b"1234", b"12345", &large_value] {
        writer
            .set(&caller, &key, value, Duration::from_secs(30))
            .await
            .unwrap();
        if value.len() <= 4 {
            assert_eq!(
                reader.get(&caller, &key).await.unwrap().as_deref(),
                Some(value)
            );
        } else {
            assert_eq!(
                reader.get(&caller, &key).await,
                Err(OperationError::ValueTooLarge)
            );
        }
    }
    reader.delete(&caller, &key).await.unwrap();
    assert_eq!(reader.get(&caller, &key).await.unwrap(), None);
}

#[tokio::test]
async fn a_ttl_that_overflows_server_time_is_invalid_and_preserves_the_value() {
    let caller = OperationContext::unbounded();
    let cache = connect_url(&cache_url(), Duration::from_secs(1)).await;
    let namespace = cache.namespace(namespace!("ttl"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"original", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace
            .set(
                &caller,
                &key,
                b"replacement",
                Duration::from_millis(i64::MAX.cast_unsigned())
            )
            .await,
        Err(infra_cache::SetError::InvalidTtl)
    );
    assert_eq!(
        namespace.get(&caller, &key).await.unwrap().as_deref(),
        Some(&b"original"[..])
    );
}

#[tokio::test]
async fn a_short_ttl_expires() {
    let caller = OperationContext::unbounded();
    let cache = connect_url(&cache_url(), Duration::from_secs(1)).await;
    let namespace = cache.namespace(namespace!("ttl"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"soon", Duration::from_millis(200))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&caller, &key).await.unwrap().as_deref(),
        Some(&b"soon"[..])
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if namespace.get(&caller, &key).await.unwrap().is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "ttl did not expire");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_unreachable_port_is_unavailable_within_the_command_bound() {
    let caller = OperationContext::unbounded();
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "this fixture briefly reserves a loopback port and closes it before testing refused connections"
    )]
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("closed port");
    let port = listener.local_addr().expect("port").port();
    drop(listener);
    let cache = connect_url(&format!("redis://127.0.0.1:{port}"), COMMAND_TIMEOUT).await;
    let namespace = cache.namespace(namespace!("down"));
    let started = Instant::now();
    let err = namespace.get(&caller, &unique_key()).await.unwrap_err();
    std::assert_matches!(err, OperationError::Unavailable);
    assert!(started.elapsed() < TIMEOUT_BOUND);
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains("outcome=\"error\"") || scrape.contains("outcome=\"timeout\""),
        "{scrape}"
    );
}

async fn wait_for_recovered_value(
    namespace: &infra_cache::CacheNamespace,
    caller: &OperationContext,
    key: &str,
    failure: &str,
) {
    let deadline = Instant::now() + RECOVERY_DEADLINE;
    loop {
        if namespace.get(caller, key).await.unwrap_or(None).as_deref() == Some(&b"kept"[..]) {
            break;
        }
        assert!(Instant::now() < deadline, "{failure}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_proxy_outage_degrades_and_recovery_hits_again() {
    let caller = OperationContext::unbounded();
    let upstream = upstream_addr(&cache_url());
    let mut proxy = Proxy::plain(upstream).await;
    let cache = connect_url(
        &format!("redis://127.0.0.1:{}", proxy.port),
        Duration::from_millis(500),
    )
    .await;
    let namespace = cache.namespace(namespace!("proxy"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"kept", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&caller, &key).await.unwrap().as_deref(),
        Some(&b"kept"[..])
    );
    proxy.stop().await;
    let err = namespace.get(&caller, &key).await.unwrap_err();
    std::assert_matches!(err, OperationError::Unavailable);
    proxy.restart_plain(upstream).await;
    wait_for_recovered_value(&namespace, &caller, &key, "cache did not recover").await;
}

#[tokio::test]
async fn a_trusted_ca_roundtrips_through_a_terminating_proxy() {
    let caller = OperationContext::unbounded();
    let material = TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, tls_acceptor(&material)).await;
    let cache = Cache::start(options(
        format!("rediss://localhost:{}", proxy.port),
        Duration::from_secs(1),
        Some(ca.path().to_path_buf()),
    ))
    .await
    .expect("admit trusted TLS");
    let namespace = cache.namespace(namespace!("tls"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"sealed", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&caller, &key).await.unwrap().as_deref(),
        Some(&b"sealed"[..])
    );
}

#[tokio::test]
async fn a_recovered_tls_dial_keeps_the_hostname_while_the_endpoint_changes() {
    let caller = OperationContext::unbounded();
    let material = TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let upstream = upstream_addr(&cache_url());
    let mut proxy = Proxy::tls(upstream, tls_acceptor(&material)).await;
    let cache = Cache::start(options(
        format!("rediss://localhost:{}", proxy.port),
        Duration::from_secs(1),
        Some(ca.path().to_path_buf()),
    ))
    .await
    .expect("admit trusted TLS");
    let namespace = cache.namespace(namespace!("tls_endpoint"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"kept", Duration::from_secs(30))
        .await
        .expect("initial TLS write");

    let port = proxy.port;
    proxy.stop().await;
    proxy = Proxy::spawn(
        bind_port(Ipv6Addr::LOCALHOST.into(), port).await,
        port,
        upstream,
        Some(tls_acceptor(&material)),
    );

    wait_for_recovered_value(
        &namespace,
        &caller,
        &key,
        "TLS recovery did not reach the IPv6 replacement",
    )
    .await;
    proxy.stop().await;
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned CA rotation replaces an existing fixture file before rebuilding the cache"
)]
async fn a_rebuilt_cache_reads_the_replaced_ca_after_recovery() {
    let caller = OperationContext::unbounded();
    let first = TlsMaterial::new("localhost");
    let ca = write_pem(&first.root);
    let upstream = upstream_addr(&cache_url());
    let mut proxy = Proxy::tls(upstream, tls_acceptor(&first)).await;
    let cache = Cache::start(options(
        format!("rediss://localhost:{}", proxy.port),
        COMMAND_TIMEOUT,
        Some(ca.path().to_path_buf()),
    ))
    .await
    .expect("admit initial CA");
    let namespace = cache.namespace(namespace!("tls_rebuild"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"kept", Duration::from_secs(30))
        .await
        .expect("initial TLS write");

    let replacement = TlsMaterial::new("localhost");
    std::fs::write(ca.path(), pem_block("CERTIFICATE", &replacement.root))
        .expect("replace CA file");
    proxy.stop().await;
    proxy
        .restart_tls(upstream, tls_acceptor(&replacement))
        .await;

    std::assert_matches!(
        namespace.get(&caller, &key).await,
        Err(OperationError::Unavailable),
        "the constructed client must retain its original CA"
    );
    let rebuilt = Cache::start(options(
        format!("rediss://localhost:{}", proxy.port),
        Duration::from_secs(1),
        Some(ca.path().to_path_buf()),
    ))
    .await
    .expect("admit replacement CA");
    assert_eq!(
        rebuilt
            .namespace(namespace!("tls_rebuild"))
            .get(&caller, &key)
            .await
            .unwrap()
            .as_deref(),
        Some(&b"kept"[..])
    );
    proxy.stop().await;
}

#[tokio::test]
async fn an_untrusted_ca_is_unavailable() {
    let caller = OperationContext::unbounded();
    let material = TlsMaterial::new("localhost");
    let ca = write_pem(new_issuer().der());
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, tls_acceptor(&material)).await;
    let cache = Cache::start(options(
        format!("rediss://localhost:{}", proxy.port),
        COMMAND_TIMEOUT,
        Some(ca.path().to_path_buf()),
    ))
    .await
    .expect("admit untrusted CA file");
    let err = cache
        .namespace(namespace!("tls"))
        .get(&caller, &unique_key())
        .await
        .unwrap_err();
    std::assert_matches!(err, OperationError::Unavailable);
}

#[tokio::test]
async fn a_client_certificate_roundtrips_through_a_proxy_that_requires_one() {
    let caller = OperationContext::unbounded();
    let material = TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let identity = ClientIdentity::new();
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, acceptor(&material, Some(&identity.ca))).await;
    let cache = Cache::start(CacheOptions {
        client_certificate: Some(identity.paths()),
        ..options(
            format!("rediss://localhost:{}", proxy.port),
            Duration::from_secs(1),
            Some(ca.path().to_path_buf()),
        )
    })
    .await
    .expect("admit a client certificate");
    let namespace = cache.namespace(namespace!("mtls"));
    let key = unique_key();
    namespace
        .set(&caller, &key, b"mutual", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&caller, &key).await.unwrap().as_deref(),
        Some(&b"mutual"[..])
    );
}

#[tokio::test]
async fn a_proxy_that_requires_a_client_certificate_refuses_a_client_without_one() {
    let caller = OperationContext::unbounded();
    let material = TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let identity = ClientIdentity::new();
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, acceptor(&material, Some(&identity.ca))).await;
    let cache = Cache::start(options(
        format!("rediss://localhost:{}", proxy.port),
        COMMAND_TIMEOUT,
        Some(ca.path().to_path_buf()),
    ))
    .await
    .expect("admit TLS without a client certificate");
    let err = cache
        .namespace(namespace!("mtls"))
        .get(&caller, &unique_key())
        .await
        .unwrap_err();
    std::assert_matches!(err, OperationError::Unavailable);
}

fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    let mut builder = PrometheusBuilder::new();
    for &(name, buckets) in HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
            .expect("observation buckets are valid");
    }
    builder.build_recorder()
}
