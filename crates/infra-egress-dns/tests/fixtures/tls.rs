//! Test-only TLS material shared by the egress consumers.

// Cryptographic fixture setup must succeed before a test can exercise TLS.
#![allow(clippy::expect_used)]

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use time::{Duration, OffsetDateTime};

/// One generated CA, matching DNS-SAN leaf and unrelated CA for a TLS test.
#[derive(Debug)]
// Each path-including consumer uses the material needed by its own TLS scenarios.
#[allow(dead_code)]
pub(super) struct TlsMaterial {
    pub(super) cert: Vec<u8>,
    pub(super) key: Vec<u8>,
    pub(super) root: Vec<u8>,
    pub(super) untrusted_root: Vec<u8>,
}

impl TlsMaterial {
    /// Generates material that is valid around the test's execution time.
    pub(super) fn new(host: &str) -> Self {
        assert!(!host.is_empty(), "TLS fixture host must not be empty");
        let now = OffsetDateTime::now_utc();
        let not_before = now - Duration::days(1);
        let not_after = now + Duration::days(1);

        let issuer = new_issuer(not_before, not_after);
        let leaf_key = KeyPair::generate().expect("generate TLS fixture leaf key");
        let mut leaf =
            CertificateParams::new(vec![host.to_owned()]).expect("create TLS fixture DNS SAN");
        leaf.not_before = not_before;
        leaf.not_after = not_after;
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = leaf
            .signed_by(&leaf_key, &issuer)
            .expect("sign TLS fixture leaf certificate");
        let unrelated = new_issuer(not_before, not_after);

        Self {
            cert: certificate.der().to_vec(),
            key: leaf_key.serialize_der(),
            root: issuer.der().to_vec(),
            untrusted_root: unrelated.der().to_vec(),
        }
    }
}

fn new_issuer(
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
) -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::default();
    params.not_before = not_before;
    params.not_after = not_after;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    CertifiedIssuer::self_signed(
        params,
        KeyPair::generate().expect("generate TLS fixture CA key"),
    )
    .expect("create TLS fixture CA")
}
