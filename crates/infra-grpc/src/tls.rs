use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::server::ParsedCertificate;
use rustls::{RootCertStore, ServerConfig};
use secrecy::{ExposeSecret as _, SecretString};

use crate::Error;

/// Server certificate material admitted by configuration. The key is borrowed
/// so building a config needs no clone of its [`SecretString`];
/// [`server_tls_config`] parses it into a key that rustls then owns.
#[derive(Clone, Copy, Debug)]
pub struct ServerTlsMaterial<'a> {
    pub certificate_pem: &'a str,
    pub private_key_pem: &'a SecretString,
    /// PEM CA for client certificates; `Some` makes them mandatory.
    pub client_ca_pem: Option<&'a str>,
}

/// TLS 1.3 server config with HTTP/2 ALPN. A client CA makes client
/// certificates mandatory.
///
/// # Errors
///
/// Returns the [`Error`] variant naming the certificate, private key, or
/// client CA that cannot be used.
///
/// # Panics
///
/// Only if the bundled aws-lc-rs provider stopped supporting TLS 1.3, which
/// is a build defect rather than operator input.
pub fn server_tls_config(material: ServerTlsMaterial<'_>) -> Result<Arc<ServerConfig>, Error> {
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
                .map_err(|_| Error::InvalidCaCertificate)?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    }
    .with_single_cert(certificates, key)
    .map_err(|_| Error::InvalidPrivateKey)?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::new(config))
}

pub(crate) fn certificates(
    input: &str,
    invalid: Error,
) -> Result<Vec<CertificateDer<'static>>, Error> {
    let certificates = CertificateDer::pem_slice_iter(input.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid.clone())?;
    if certificates.is_empty() {
        return Err(invalid);
    }
    Ok(certificates)
}

/// A presented chain, each entry parsed as an X.509 certificate. Rustls
/// itself parses only the leaf, so a broken intermediate would otherwise
/// pass startup and fail the handshake of every peer that needs it. Trust
/// anchors skip this: rustls admits X.509 v1 roots that this parse refuses.
pub(crate) fn chain(input: &str) -> Result<Vec<CertificateDer<'static>>, Error> {
    let certificates = certificates(input, Error::InvalidCertificate)?;
    for certificate in &certificates {
        ParsedCertificate::try_from(certificate).map_err(|_| Error::InvalidCertificate)?;
    }
    Ok(certificates)
}

pub(crate) fn private_key(pem: &SecretString) -> Result<PrivateKeyDer<'static>, Error> {
    PrivateKeyDer::from_pem_slice(pem.expose_secret().as_bytes())
        .map_err(|_| Error::InvalidPrivateKey)
}

pub(crate) fn roots(input: &str) -> Result<RootCertStore, Error> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(input, Error::InvalidCaCertificate)? {
        roots
            .add(certificate)
            .map_err(|_| Error::InvalidCaCertificate)?;
    }
    Ok(roots)
}
