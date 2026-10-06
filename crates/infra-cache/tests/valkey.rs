//! Valkey proof for the cache client. `CACHE_URL` is a plaintext `redis://`
//! URL of a disposable real server with fixture ACL administration rights.
//! `scripts/ci/test-integration-cache.sh` starts that server; this file does not.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests assert Valkey behavior and own their fixture setup"
)]

#[path = "../../../test/fixtures/tls.rs"]
mod tls;

#[path = "support/valkey_auth.rs"]
mod valkey_auth;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use health::Probe;
use infra_cache::{Cache, CacheOptions, ClientCertificate, Unavailable};
use secrecy::SecretString;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};

const COMMAND_TIMEOUT: Duration = Duration::from_millis(200);
const TIMEOUT_BOUND: Duration = Duration::from_millis(450);
const RECOVERY_DEADLINE: Duration = Duration::from_secs(5);

static KEYS: AtomicU64 = AtomicU64::new(0);

fn cache_url() -> String {
    std::env::var("CACHE_URL")
        .unwrap_or_else(|_| panic!("CACHE_URL is unset; run scripts/ci/test-integration-cache.sh"))
}

fn upstream_addr(url: &str) -> SocketAddr {
    let host_port = url
        .strip_prefix("redis://")
        .and_then(|rest| rest.split(['/', '?']).next())
        .filter(|host_port| !host_port.is_empty() && !host_port.contains('@'));
    let Some(host_port) = host_port else {
        panic!(
            "CACHE_URL must be a plaintext redis://host:port with no password; run scripts/ci/test-integration-cache.sh"
        );
    };
    host_port.parse().unwrap_or_else(|_| {
        panic!("CACHE_URL host:port could not be parsed; run scripts/ci/test-integration-cache.sh")
    })
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
    }
}

fn connect_url(url: &str, command_timeout: Duration) -> Cache {
    Cache::connect_lazy(options(url, command_timeout, None)).expect("admit cache DSN")
}

struct Proxy {
    port: u16,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

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
        let task = tokio::spawn(async move {
            run_proxy(listener, upstream, acceptor, stop_rx).await;
        });
        Self {
            port,
            stop: Some(stop),
            task,
        }
    }

    async fn stop(&mut self) {
        // A second stop (restart after an outage) must not poll the finished task again.
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut self.task).await;
        }
        self.task.abort();
    }

    async fn restart_plain(&mut self, upstream: SocketAddr) {
        self.stop().await;
        let listener = bind_port(self.port).await;
        *self = Self::spawn(listener, self.port, upstream, None);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn bind_port(port: u16) -> TcpListener {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => return listener,
            Err(error) if tokio::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => panic!("rebind {port}: {error}"),
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
    connections.abort_all();
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
            let _ = copy_both(&mut inbound, &mut outbound).await;
        }
    }
}

async fn copy_both<A, B>(left: &mut A, right: &mut B) -> std::io::Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(left, right).await.map(|_| ())
}

fn tls_acceptor(material: &tls::TlsMaterial) -> TlsAcceptor {
    acceptor(material, None)
}

/// A TLS endpoint that, given `client_ca`, refuses a client that presents no
/// certificate signed by it, as a server with `tls-auth-clients yes` does.
fn acceptor(material: &tls::TlsMaterial, client_ca: Option<&[u8]>) -> TlsAcceptor {
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
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut body = String::new();
    for line in encoded.as_bytes().chunks(64) {
        body.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
        body.push('\n');
    }
    let pem = format!("-----BEGIN {label}-----\n{body}-----END {label}-----\n");
    let mut file = tempfile::NamedTempFile::new().expect("pem tempfile");
    std::io::Write::write_all(&mut file, pem.as_bytes()).expect("write pem");
    file
}

/// A client CA and a client-auth leaf it signed, the leaf and its key as PEM files.
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
    let cache = connect_url(&cache_url(), Duration::from_secs(1));
    let alpha = cache.namespace("alpha");
    let beta = cache.namespace("beta");
    for key in [unique_key(), format!("{}:\0\r\nюникод", unique_key())] {
        assert_eq!(alpha.get(&key).await.unwrap(), None);
        alpha
            .set(&key, b"value", Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(
            alpha.get(&key).await.unwrap().as_deref(),
            Some(&b"value"[..])
        );
        assert_eq!(beta.get(&key).await.unwrap(), None);
        alpha.delete(&key).await.unwrap();
        assert_eq!(alpha.get(&key).await.unwrap(), None);
        alpha.delete(&key).await.unwrap();
    }
}

#[tokio::test]
async fn a_short_ttl_expires() {
    let cache = connect_url(&cache_url(), Duration::from_secs(1));
    let namespace = cache.namespace("ttl");
    let key = unique_key();
    namespace
        .set(&key, b"soon", Duration::from_millis(200))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&key).await.unwrap().as_deref(),
        Some(&b"soon"[..])
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if namespace.get(&key).await.unwrap().is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "ttl did not expire");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn probe_succeeds_against_valkey() {
    let cache = connect_url(&cache_url(), Duration::from_secs(1));
    cache.probe().check().await.expect("cache ping");
}

#[test]
fn an_unreachable_port_is_unavailable_within_the_command_bound() {
    let recorder = observation_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            #[allow(
                clippy::disallowed_methods,
                clippy::disallowed_types,
                reason = "this fixture briefly reserves a loopback port and closes it before testing refused connections"
            )]
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("closed port");
            let port = listener.local_addr().expect("port").port();
            drop(listener);
            let cache = connect_url(&format!("redis://127.0.0.1:{port}"), COMMAND_TIMEOUT);
            let namespace = cache.namespace("down");
            let started = Instant::now();
            let err = namespace.get(&unique_key()).await.unwrap_err();
            assert!(matches!(err, Unavailable));
            assert!(started.elapsed() < TIMEOUT_BOUND);
        });
    });
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains("outcome=\"error\"") || scrape.contains("outcome=\"timeout\""),
        "{scrape}"
    );
}

#[test]
fn a_silent_server_times_out_within_the_command_bound() {
    let recorder = observation_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("silent bind");
            let port = listener.local_addr().expect("port").port();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let _stream = stream;
                        std::future::pending::<()>().await;
                    });
                }
            });
            let cache = connect_url(&format!("redis://127.0.0.1:{port}"), COMMAND_TIMEOUT);
            let namespace = cache.namespace("silent");
            let started = Instant::now();
            let err = namespace.get(&unique_key()).await.unwrap_err();
            assert!(matches!(err, Unavailable));
            assert!(started.elapsed() < TIMEOUT_BOUND);
        });
    });
    let scrape = recorder.handle().render();
    assert!(scrape.contains("outcome=\"timeout\""), "{scrape}");
}

#[tokio::test]
async fn a_proxy_outage_degrades_and_recovery_hits_again() {
    let upstream = upstream_addr(&cache_url());
    let mut proxy = Proxy::plain(upstream).await;
    let cache = connect_url(
        &format!("redis://127.0.0.1:{}", proxy.port),
        Duration::from_millis(500),
    );
    let namespace = cache.namespace("proxy");
    let key = unique_key();
    namespace
        .set(&key, b"kept", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&key).await.unwrap().as_deref(),
        Some(&b"kept"[..])
    );
    proxy.stop().await;
    let err = namespace.get(&key).await.unwrap_err();
    assert!(matches!(err, Unavailable));
    proxy.restart_plain(upstream).await;
    let deadline = Instant::now() + RECOVERY_DEADLINE;
    loop {
        if namespace.get(&key).await.unwrap_or(None).as_deref() == Some(&b"kept"[..]) {
            break;
        }
        assert!(Instant::now() < deadline, "cache did not recover");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn a_trusted_ca_roundtrips_through_a_terminating_proxy() {
    let material = tls::TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, tls_acceptor(&material)).await;
    let cache = Cache::connect_lazy(options(
        format!("rediss://localhost:{}", proxy.port),
        Duration::from_secs(1),
        Some(ca.path().to_path_buf()),
    ))
    .expect("admit trusted TLS");
    let namespace = cache.namespace("tls");
    let key = unique_key();
    namespace
        .set(&key, b"sealed", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&key).await.unwrap().as_deref(),
        Some(&b"sealed"[..])
    );
}

#[tokio::test]
async fn an_untrusted_ca_is_unavailable() {
    let material = tls::TlsMaterial::new("localhost");
    let ca = write_pem(&material.untrusted_root);
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, tls_acceptor(&material)).await;
    let cache = Cache::connect_lazy(options(
        format!("rediss://localhost:{}", proxy.port),
        COMMAND_TIMEOUT,
        Some(ca.path().to_path_buf()),
    ))
    .expect("admit untrusted CA file");
    let err = cache.namespace("tls").get(&unique_key()).await.unwrap_err();
    assert!(matches!(err, Unavailable));
}

#[tokio::test]
async fn a_client_certificate_roundtrips_through_a_proxy_that_requires_one() {
    let material = tls::TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let identity = ClientIdentity::new();
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, acceptor(&material, Some(&identity.ca))).await;
    let cache = Cache::connect_lazy(CacheOptions {
        client_certificate: Some(identity.paths()),
        ..options(
            format!("rediss://localhost:{}", proxy.port),
            Duration::from_secs(1),
            Some(ca.path().to_path_buf()),
        )
    })
    .expect("admit a client certificate");
    let namespace = cache.namespace("mtls");
    let key = unique_key();
    namespace
        .set(&key, b"mutual", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(
        namespace.get(&key).await.unwrap().as_deref(),
        Some(&b"mutual"[..])
    );
}

#[tokio::test]
async fn a_proxy_that_requires_a_client_certificate_refuses_a_client_without_one() {
    let material = tls::TlsMaterial::new("localhost");
    let ca = write_pem(&material.root);
    let identity = ClientIdentity::new();
    let upstream = upstream_addr(&cache_url());
    let proxy = Proxy::tls(upstream, acceptor(&material, Some(&identity.ca))).await;
    let cache = Cache::connect_lazy(options(
        format!("rediss://localhost:{}", proxy.port),
        COMMAND_TIMEOUT,
        Some(ca.path().to_path_buf()),
    ))
    .expect("admit TLS without a client certificate");
    let err = cache
        .namespace("mtls")
        .get(&unique_key())
        .await
        .unwrap_err();
    assert!(matches!(err, Unavailable));
}

fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    metrics_exporter_prometheus::PrometheusBuilder::new()
        .set_buckets_for_metric(
            metrics_exporter_prometheus::Matcher::Full(
                infra_cache::OPERATION_DURATION_METRIC.to_owned(),
            ),
            infra_cache::OPERATION_DURATION_BUCKETS,
        )
        .expect("observation buckets are valid")
        .build_recorder()
}
