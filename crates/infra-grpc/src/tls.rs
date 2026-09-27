use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::{RootCertStore, ServerConfig};

use crate::Error;

/// Server certificate material admitted by configuration. The private key is
/// redacted by configuration; this type's `Debug` reports only material presence.
#[derive(Clone)]
pub struct ServerTlsMaterial {
    pub certificate_pem: Vec<u8>,
    pub private_key_pem: Vec<u8>,
    pub client_ca_pem: Option<Vec<u8>>,
}

impl std::fmt::Debug for ServerTlsMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerTlsMaterial")
            .field("has_certificate", &!self.certificate_pem.is_empty())
            .field("has_private_key", &!self.private_key_pem.is_empty())
            .field("has_client_ca", &self.client_ca_pem.is_some())
            .finish()
    }
}

/// TLS 1.3 server config with HTTP/2 ALPN. A client CA makes client
/// certificates mandatory.
///
/// # Errors
///
/// Returns [`Error::InvalidConfiguration`] when the certificate, private key,
/// or client CA cannot be used.
pub fn server_tls_config(material: &ServerTlsMaterial) -> Result<Arc<ServerConfig>, Error> {
    let certificates = certificates(&material.certificate_pem)?;
    let key = PrivateKeyDer::from_pem_slice(&material.private_key_pem)
        .map_err(|_| Error::InvalidConfiguration)?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let builder = ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| Error::InvalidConfiguration)?;
    let mut config = match &material.client_ca_pem {
        Some(ca) => {
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots(ca)?))
                .build()
                .map_err(|_| Error::InvalidConfiguration)?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    }
    .with_single_cert(certificates, key)
    .map_err(|_| Error::InvalidConfiguration)?;
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::new(config))
}

fn certificates(input: &[u8]) -> Result<Vec<CertificateDer<'static>>, Error> {
    let certificates = CertificateDer::pem_slice_iter(input)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Error::InvalidConfiguration)?;
    if certificates.is_empty() {
        return Err(Error::InvalidConfiguration);
    }
    Ok(certificates)
}

fn roots(input: &[u8]) -> Result<RootCertStore, Error> {
    let mut roots = RootCertStore::empty();
    for certificate in certificates(input)? {
        roots
            .add(certificate)
            .map_err(|_| Error::InvalidConfiguration)?;
    }
    Ok(roots)
}
