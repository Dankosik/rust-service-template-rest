use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};

/// Every certificate keeps rcgen's default validity window, so nothing
/// checked in or computed from the test clock can expire.
#[derive(Debug)]
pub(crate) struct TlsMaterial {
    pub(crate) cert: Vec<u8>,
    pub(crate) key: Vec<u8>,
    pub(crate) root: Vec<u8>,
}

impl TlsMaterial {
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "fixture setup failures are test failures"
    )]
    pub(crate) fn new(host: &str) -> Self {
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
        }
    }
}

#[allow(
    clippy::expect_used,
    reason = "fixture setup failures are test failures"
)]
pub(crate) fn new_issuer() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    CertifiedIssuer::self_signed(
        params,
        KeyPair::generate().expect("generate TLS fixture CA key"),
    )
    .expect("create TLS fixture CA")
}
