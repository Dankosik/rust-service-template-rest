//! Whom the exporter trusts: a collector behind a private certificate
//! authority, and one that requires a client certificate.

use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};

use super::*;

/// A collector that speaks TLS. A refused handshake is a connection without
/// a request.
fn tls_collector(config: Arc<ServerConfig>) -> Collector {
    Collector::start(move |stream| {
        let tls = ServerConnection::new(Arc::clone(&config)).ok()?;
        exchange(StreamOwned::new(tls, stream))
    })
}

/// A private certificate authority, the collector certificate it signed for
/// `127.0.0.1`, and a client certificate, with the PEM files the standard
/// variables would name.
struct Pki {
    issuer: CertifiedIssuer<'static, KeyPair>,
    files: tempfile::TempDir,
}

impl Pki {
    fn new() -> Self {
        let mut ca = CertificateParams::default();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        Self {
            issuer: CertifiedIssuer::self_signed(ca, KeyPair::generate().expect("CA key"))
                .expect("self-signed CA"),
            files: tempfile::tempdir().expect("directory for PEM files"),
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    fn file(&self, variable: &'static str, name: &str, pem: &str) -> TrustFile {
        let path = self.files.path().join(name);
        std::fs::write(&path, pem).expect("write a PEM file");
        TrustFile { variable, path }
    }

    fn certificate_file(&self) -> TrustFile {
        self.file(CERTIFICATE_VARS[1], "ca.pem", &self.issuer.pem())
    }

    fn client_files(&self) -> (TrustFile, TrustFile) {
        let key = KeyPair::generate().expect("client key");
        let mut params = CertificateParams::new(vec!["exporter".to_owned()]).expect("client names");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let certificate = params.signed_by(&key, &self.issuer).expect("client leaf");
        (
            self.file(CLIENT_CERTIFICATE_VARS[1], "client.pem", &certificate.pem()),
            self.file(CLIENT_KEY_VARS[1], "client.key", &key.serialize_pem()),
        )
    }

    fn collector(&self, require_client_certificate: bool) -> Arc<ServerConfig> {
        let key = KeyPair::generate().expect("collector key");
        let mut params =
            CertificateParams::new(vec!["127.0.0.1".to_owned()]).expect("collector names");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params
            .signed_by(&key, &self.issuer)
            .expect("collector leaf");
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("protocol versions");
        let builder = if require_client_certificate {
            let mut roots = RootCertStore::empty();
            roots.add(self.issuer.der().clone()).expect("CA root");
            builder.with_client_cert_verifier(
                WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                    .build()
                    .expect("client verifier"),
            )
        } else {
            builder.with_no_client_auth()
        };
        Arc::new(
            builder
                .with_single_cert(
                    vec![certificate.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("collector certificate"),
        )
    }
}

/// One export through the exporter the trust material builds. The batch
/// processor runs the blocking client on its own thread; here it finishes
/// in one poll.
fn export(collector: &Collector, trust: &CollectorTrust) -> OTelSdkResult {
    span_exporter(
        Some(&format!("https://{}/v1/traces", collector.addr)),
        HashMap::new(),
        trust,
    )
    .expect("the exporter builds")
    .export(Vec::new())
    .now_or_never()
    .expect("the blocking client finishes in one poll")
}

#[test]
fn a_certificate_file_is_what_makes_a_private_collector_trusted() {
    let pki = Pki::new();
    let collector = tls_collector(pki.collector(false));

    export(&collector, &CollectorTrust::default())
        .expect_err("the platform trust store does not know the private CA");

    let trust = CollectorTrust {
        certificate: Some(pki.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &trust).expect("the certificate file's CA signed the collector");
    assert_eq!(collector.received().path, "/v1/traces");

    // Only the file's certificates are trusted: another CA's file does not
    // fall back to anything that would accept this collector.
    let other = Pki::new();
    let stranger = CollectorTrust {
        certificate: Some(other.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &stranger).expect_err("another CA did not sign the collector");
}

#[test]
fn a_client_certificate_is_presented_to_a_collector_that_requires_one() {
    let pki = Pki::new();
    let collector = tls_collector(pki.collector(true));
    let anonymous = CollectorTrust {
        certificate: Some(pki.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &anonymous).expect_err("the collector requires a client certificate");

    let (client_certificate, client_key) = pki.client_files();
    let identified = CollectorTrust {
        client_certificate: Some(client_certificate),
        client_key: Some(client_key),
        ..anonymous
    };
    export(&collector, &identified).expect("the client certificate is accepted");
    assert_eq!(collector.received().path, "/v1/traces");
}
