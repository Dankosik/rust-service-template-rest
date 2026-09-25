//! Generated TLS material for the egress consumers' real-TLS tests.

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};

/// One generated CA, a matching DNS-SAN leaf and key, and an unrelated CA.
///
/// Every certificate keeps rcgen's default validity window, so nothing
/// checked in or computed from the test clock can expire.
#[derive(Debug)]
pub struct TlsMaterial {
    /// Leaf certificate DER for the fixture server.
    pub cert: Vec<u8>,
    /// PKCS #8 DER private key of the leaf.
    pub key: Vec<u8>,
    /// DER of the CA that signed the leaf.
    pub root: Vec<u8>,
    /// DER of a CA that did not sign the leaf.
    pub untrusted_root: Vec<u8>,
}

impl TlsMaterial {
    /// Generates fresh material whose leaf names `host`.
    ///
    /// # Panics
    ///
    /// Panics when `host` is empty or key generation or signing fails; a TLS
    /// test cannot run without its material.
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
        let unrelated = new_issuer();

        Self {
            cert: certificate.der().to_vec(),
            key: leaf_key.serialize_der(),
            root: issuer.der().to_vec(),
            untrusted_root: unrelated.der().to_vec(),
        }
    }
}

#[allow(
    clippy::expect_used,
    reason = "fixture setup failures are test failures"
)]
fn new_issuer() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    CertifiedIssuer::self_signed(
        params,
        KeyPair::generate().expect("generate TLS fixture CA key"),
    )
    .expect("create TLS fixture CA")
}
