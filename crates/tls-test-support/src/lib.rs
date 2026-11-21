#![expect(
    clippy::expect_used,
    reason = "fixture setup failures are test failures"
)]

use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, ClientConfig, RootCertStore, ServerConfig,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};

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
        let mut leaf = CertificateParams::new(names).expect("create TLS fixture SANs");
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = leaf
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
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()));
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("fixture TLS protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![self.cert.clone().into()], key)
            .expect("fixture server certificate")
    }

    #[must_use]
    #[track_caller]
    pub fn server_config_tls13(&self) -> ServerConfig {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()));
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .expect("TLS 1.3 fixture server")
            .with_no_client_auth()
            .with_single_cert(vec![self.cert.clone().into()], key)
            .expect("fixture server certificate")
    }

    #[must_use]
    #[track_caller]
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(Arc::new(self.server_config()))
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
