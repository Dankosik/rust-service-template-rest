use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};

#[derive(Debug)]
pub struct TlsMaterial {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
    pub root: Vec<u8>,
    pub root_pem: String,
}

impl TlsMaterial {
    #[must_use]
    #[allow(
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
            key: leaf_key.serialize_der(),
            root: issuer.der().to_vec(),
            root_pem: issuer.pem(),
        }
    }
}

#[must_use]
#[allow(
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
