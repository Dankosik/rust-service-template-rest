use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::server::ParsedCertificate;
use rustls::{RootCertStore, ServerConfig};
use secrecy::{ExposeSecret as _, SecretString};

use crate::TlsMaterialError;

#[derive(Clone, Copy, Debug)]
pub struct ServerTlsMaterial<'a> {
    pub certificate_pem: &'a str,
    pub private_key_pem: &'a SecretString,
    pub client_ca_pem: Option<&'a str>,
}

pub fn server_tls_config(
    material: ServerTlsMaterial<'_>,
) -> Result<Arc<ServerConfig>, TlsMaterialError> {
    let certificates = chain(material.certificate_pem)?;
    let key = private_key(material.private_key_pem)?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    #[expect(
        clippy::expect_used,
        reason = "a fixed provider and version list; failure is a build defect, not operator input"
    )]
    let builder = ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("the aws-lc-rs provider supports TLS 1.3");
    let mut config = match material.client_ca_pem {
        Some(ca) => {
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots(ca)?))
                .build()
                .map_err(|_| TlsMaterialError::InvalidCaCertificate)?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    }
    .with_single_cert(certificates, key)
    .map_err(|_| TlsMaterialError::InvalidPrivateKey)?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::new(config))
}

fn certificates(input: &str) -> Option<Vec<CertificateDer<'static>>> {
    let certificates = CertificateDer::pem_slice_iter(input.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!certificates.is_empty()).then_some(certificates)
}

fn chain(input: &str) -> Result<Vec<CertificateDer<'static>>, TlsMaterialError> {
    let certificates = certificates(input).ok_or(TlsMaterialError::InvalidCertificate)?;
    for certificate in &certificates {
        ParsedCertificate::try_from(certificate)
            .map_err(|_| TlsMaterialError::InvalidCertificate)?;
    }
    Ok(certificates)
}

fn private_key(pem: &SecretString) -> Result<PrivateKeyDer<'static>, TlsMaterialError> {
    PrivateKeyDer::from_pem_slice(pem.expose_secret().as_bytes())
        .map_err(|_| TlsMaterialError::InvalidPrivateKey)
}

fn roots(input: &str) -> Result<RootCertStore, TlsMaterialError> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(input).ok_or(TlsMaterialError::InvalidCaCertificate)? {
        roots
            .add(certificate)
            .map_err(|_| TlsMaterialError::InvalidCaCertificate)?;
    }
    Ok(roots)
}
