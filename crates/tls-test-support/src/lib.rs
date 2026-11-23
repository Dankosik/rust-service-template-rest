#![expect(
    clippy::expect_used,
    reason = "fixture setup failures are test failures"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, ClientConfig, RootCertStore, ServerConfig, SupportedProtocolVersion,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
#[must_use]
pub struct HttpsServer {
    local_addr: SocketAddr,
    shutdown: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl HttpsServer {
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for HttpsServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[derive(Debug)]
pub struct TlsMaterial {
    pub cert: Vec<u8>,
    pub cert_pem: String,
    pub key: Vec<u8>,
    pub key_pem: String,
    pub root: Vec<u8>,
    pub root_pem: String,
}

impl TlsMaterial {
    #[must_use]
    #[track_caller]
    pub fn new(host: &str) -> Self {
        Self::for_hosts(&[host])
    }

    #[must_use]
    #[track_caller]
    pub fn for_hosts(hosts: &[&str]) -> Self {
        assert!(!hosts.is_empty(), "TLS fixture hosts must not be empty");
        assert!(
            hosts.iter().all(|host| !host.is_empty()),
            "TLS fixture host must not be empty"
        );
        let issuer = new_issuer();
        let leaf_key = KeyPair::generate().expect("generate TLS fixture leaf key");
        let names = hosts.iter().copied().map(str::to_owned).collect::<Vec<_>>();
        let mut leaf_params = CertificateParams::new(names).expect("create TLS fixture SANs");
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = leaf_params
            .signed_by(&leaf_key, &issuer)
            .expect("sign TLS fixture leaf certificate");

        Self {
            cert: certificate.der().to_vec(),
            cert_pem: certificate.pem(),
            key: leaf_key.serialize_der(),
            key_pem: leaf_key.serialize_pem(),
            root: issuer.der().to_vec(),
            root_pem: issuer.pem(),
        }
    }

    #[must_use]
    #[track_caller]
    pub fn server_config(&self) -> ServerConfig {
        self.server_config_for(rustls::DEFAULT_VERSIONS)
    }

    #[must_use]
    #[track_caller]
    pub fn server_config_tls13(&self) -> ServerConfig {
        self.server_config_for(&[&rustls::version::TLS13])
    }

    #[track_caller]
    fn server_config_for(&self, versions: &[&'static SupportedProtocolVersion]) -> ServerConfig {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()));
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_protocol_versions(versions)
            .expect("fixture TLS protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![self.cert.clone().into()], key)
            .expect("fixture server certificate")
    }

    #[must_use]
    #[track_caller]
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::new(self.server_config()))
    }

    #[track_caller]
    pub fn serve_http1_tls13(&self, router: Router) -> HttpsServer {
        let socket = TcpSocket::new_v4().expect("create loopback fixture socket");
        socket
            .bind((Ipv4Addr::LOCALHOST, 0).into())
            .expect("bind loopback fixture socket");
        let listener = socket
            .listen(128)
            .expect("listen on loopback fixture socket");
        let local_addr = listener.local_addr().expect("fixture listener address");
        let acceptor = TlsAcceptor::from(Arc::new(self.server_config_tls13()));
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(accept_connections(
            listener,
            acceptor,
            router,
            shutdown.clone(),
        ));
        HttpsServer {
            local_addr,
            shutdown,
            task: Some(task),
        }
    }
}

async fn accept_connections(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    router: Router,
    shutdown: CancellationToken,
) {
    let mut connections = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            accepted = listener.accept() => accepted,
        };
        let Ok((stream, _)) = accepted else {
            continue;
        };
        connections.spawn(serve_tls_connection(
            stream,
            acceptor.clone(),
            router.clone(),
            shutdown.clone(),
        ));
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn serve_tls_connection(
    stream: TcpStream,
    acceptor: TlsAcceptor,
    router: Router,
    shutdown: CancellationToken,
) {
    let serve = async {
        let Ok(stream) = acceptor.accept(stream).await else {
            return;
        };
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(stream), TowerToHyperService::new(router))
            .await;
    };
    tokio::select! {
        biased;
        () = shutdown.cancelled() => {}
        () = serve => {}
    }
}

#[must_use]
#[track_caller]
pub fn new_issuer() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    CertifiedIssuer::self_signed(
        params,
        KeyPair::generate().expect("generate TLS fixture CA key"),
    )
    .expect("create TLS fixture CA")
}

#[must_use]
#[track_caller]
pub fn localhost_tls(alpn: &[u8]) -> (ServerConfig, ClientConfig) {
    let material = TlsMaterial::new("localhost");
    let mut server = material.server_config_tls13();
    server.alpn_protocols = vec![alpn.to_vec()];
    let mut roots = RootCertStore::empty();
    roots.add(material.root.into()).expect("fixture CA");
    let mut client = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("TLS 1.3 fixture client")
    .with_root_certificates(roots)
    .with_no_client_auth();
    client.alpn_protocols = vec![alpn.to_vec()];
    (server, client)
}
