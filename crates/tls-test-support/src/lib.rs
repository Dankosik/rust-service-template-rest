use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
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
    #[expect(
        clippy::expect_used,
        reason = "fixture setup failures are test failures"
    )]
    pub fn new(host: &str) -> Self {
        assert!(!host.is_empty(), "TLS fixture host must not be empty");
        let issuer = new_issuer();
        let leaf_key = KeyPair::generate().expect("generate TLS fixture leaf key");
        let mut leaf =
            CertificateParams::new(vec![host.to_owned()]).expect("create TLS fixture DNS SAN");
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
}

#[must_use]
#[expect(
    clippy::expect_used,
    reason = "fixture setup failures are test failures"
)]
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

// template:begin grpc:tls-fixtures-http-listener-config
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "fixture setup failures are test failures"
)]
pub fn localhost_tls(alpn: &[u8]) -> (rustls::ServerConfig, rustls::ClientConfig) {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let material = TlsMaterial::new("localhost");
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key));
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut server = rustls::ServerConfig::builder_with_provider(std::sync::Arc::clone(&provider))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 server")
        .with_no_client_auth()
        .with_single_cert(vec![material.cert.into()], key)
        .expect("fixture server certificate");
    server.alpn_protocols = vec![alpn.to_vec()];
    let mut roots = rustls::RootCertStore::empty();
    roots.add(material.root.into()).expect("fixture CA");
    let mut client = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS 1.3 client")
        .with_root_certificates(roots)
        .with_no_client_auth();
    client.alpn_protocols = vec![alpn.to_vec()];
    (server, client)
}
// template:end grpc:tls-fixtures-http-listener-config
