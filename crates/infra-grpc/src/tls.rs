use std::sync::Arc;

use infra_grpc_core::tls::{
    parse_ca_roots, parse_cert_chain, parse_private_key, tls13_server_builder,
};
use rustls::ServerConfig;
use secrecy::SecretString;

use crate::TlsMaterialError;

#[derive(Clone, Copy, derive_more::Debug)]
pub struct ServerTlsMaterial<'a> {
    #[debug(skip)]
    pub certificate_pem: &'a str,
    #[debug(skip)]
    pub private_key_pem: &'a SecretString,
    #[debug(skip)]
    pub client_ca_pem: Option<&'a str>,
}

pub fn server_tls_config(
    material: ServerTlsMaterial<'_>,
) -> Result<Arc<ServerConfig>, TlsMaterialError> {
    let certificates = parse_cert_chain(material.certificate_pem)
        .map_err(|_| TlsMaterialError::InvalidCertificate)?;
    let key = parse_private_key(material.private_key_pem)
        .map_err(|_| TlsMaterialError::InvalidPrivateKey)?;
    let builder = tls13_server_builder();
    let config = match material.client_ca_pem {
        Some(ca) => {
            let roots = parse_ca_roots(ca).map_err(|_| TlsMaterialError::InvalidCaCertificate)?;
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|_| TlsMaterialError::InvalidCaCertificate)?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    }
    .with_single_cert(certificates, key)
    .map_err(|_| TlsMaterialError::InvalidPrivateKey)?;
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_tls_material_debug_omits_pem_material() {
        let key = SecretString::from(
            "-----BEGIN PRIVATE KEY-----\nsecret-key\n-----END PRIVATE KEY-----",
        );
        let material = ServerTlsMaterial {
            certificate_pem: "-----BEGIN CERTIFICATE-----\ncert-body\n-----END CERTIFICATE-----",
            private_key_pem: &key,
            client_ca_pem: Some("-----BEGIN CERTIFICATE-----\nca-body\n-----END CERTIFICATE-----"),
        };
        let formatted = format!("{material:?}");
        assert!(!formatted.contains("-----BEGIN"), "{formatted}");
        assert!(!formatted.contains("cert-body"), "{formatted}");
        assert!(!formatted.contains("secret-key"), "{formatted}");
        assert!(!formatted.contains("ca-body"), "{formatted}");
    }
}
