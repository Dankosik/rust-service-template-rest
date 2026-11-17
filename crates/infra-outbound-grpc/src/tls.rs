use std::sync::{Arc, OnceLock};

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use rustls::server::ParsedCertificate;
use rustls::{ClientConfig, ConfigBuilder, RootCertStore, WantsVerifier};
use rustls_platform_verifier::{BuilderVerifierExt as _, Verifier};
use secrecy::ExposeSecret as _;

use crate::{ClientError, ClientIdentity, ClientTlsMaterial, TlsMaterialError};

pub(crate) fn client_tls_config(
    material: &ClientTlsMaterial,
) -> Result<Arc<ClientConfig>, ClientError> {
    match (&material.ca_certificate_pem, &material.identity) {
        (None, None) => default_tls_config(),
        (None, Some(identity)) => {
            let (certs, key) = parse_client_identity(identity)?;
            let verifier = platform_verifier()?;
            let mut config = tls13_builder()?
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_client_auth_cert(certs, key)
                .map_err(|_| ClientError::InvalidClientTls)?;
            config.alpn_protocols = vec![b"h2".to_vec()];
            Ok(Arc::new(config))
        }
        (Some(ca_pem), identity) => {
            let roots = parse_ca_roots(ca_pem)?;
            let builder = tls13_builder()?.with_root_certificates(roots);
            let mut config = match identity {
                Some(identity) => {
                    let (certs, key) = parse_client_identity(identity)?;
                    builder
                        .with_client_auth_cert(certs, key)
                        .map_err(|_| ClientError::InvalidClientTls)?
                }
                None => builder.with_no_client_auth(),
            };
            config.alpn_protocols = vec![b"h2".to_vec()];
            Ok(Arc::new(config))
        }
    }
}

fn default_tls_config() -> Result<Arc<ClientConfig>, ClientError> {
    static DEFAULT_CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(config) = DEFAULT_CONFIG.get() {
        return Ok(Arc::clone(config));
    }
    let mut config = tls13_builder()?
        .with_platform_verifier()
        .map_err(|_| ClientError::InvalidClientTls)?
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::clone(DEFAULT_CONFIG.get_or_init(|| Arc::new(config))))
}

fn platform_verifier() -> Result<Arc<Verifier>, ClientError> {
    static PLATFORM_VERIFIER: OnceLock<Arc<Verifier>> = OnceLock::new();
    if let Some(verifier) = PLATFORM_VERIFIER.get() {
        return Ok(Arc::clone(verifier));
    }
    let verifier = Verifier::new(tls13_provider()).map_err(|_| ClientError::InvalidClientTls)?;
    Ok(Arc::clone(
        PLATFORM_VERIFIER.get_or_init(|| Arc::new(verifier)),
    ))
}

fn tls13_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn tls13_builder() -> Result<ConfigBuilder<ClientConfig, WantsVerifier>, ClientError> {
    ClientConfig::builder_with_provider(tls13_provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| ClientError::InvalidClientTls)
}

fn parse_ca_roots(ca_pem: &str) -> Result<RootCertStore, TlsMaterialError> {
    let mut roots = RootCertStore::empty();
    let mut count = 0_usize;
    for item in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        let cert = item.map_err(|_| TlsMaterialError::InvalidCaCertificate)?;
        roots
            .add(cert)
            .map_err(|_| TlsMaterialError::InvalidCaCertificate)?;
        count += 1;
    }
    if count == 0 {
        return Err(TlsMaterialError::InvalidCaCertificate);
    }
    Ok(roots)
}

fn parse_client_identity(
    identity: &ClientIdentity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsMaterialError> {
    let mut chain = Vec::new();
    for item in CertificateDer::pem_slice_iter(identity.certificate_pem.as_bytes()) {
        let cert = item.map_err(|_| TlsMaterialError::InvalidCertificate)?;
        ParsedCertificate::try_from(&cert).map_err(|_| TlsMaterialError::InvalidCertificate)?;
        chain.push(cert);
    }
    if chain.is_empty() {
        return Err(TlsMaterialError::InvalidCertificate);
    }
    let key = PrivateKeyDer::from_pem_slice(identity.private_key_pem.expose_secret().as_bytes())
        .map_err(|_| TlsMaterialError::InvalidPrivateKey)?;
    Ok((chain, key))
}

#[cfg(test)]
mod tests {
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use secrecy::SecretString;

    use super::*;

    fn test_pki() -> (String, ClientIdentity, SecretString) {
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
        let client_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(vec!["client.test".to_owned()]).unwrap();
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let leaf = leaf_params.signed_by(&client_key, &issuer).unwrap();
        let other_key = KeyPair::generate().unwrap();
        (
            issuer.pem(),
            ClientIdentity {
                certificate_pem: leaf.pem(),
                private_key_pem: SecretString::from(client_key.serialize_pem()),
            },
            SecretString::from(other_key.serialize_pem()),
        )
    }

    #[test]
    fn default_tls_config_is_cached_and_advertises_h2() {
        let first = client_tls_config(&ClientTlsMaterial::default()).unwrap();
        let second = client_tls_config(&ClientTlsMaterial::default()).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.alpn_protocols, vec![b"h2".to_vec()]);
    }

    #[test]
    fn custom_ca_and_identity_validate_material_and_pairing() {
        let (ca_pem, identity, other_key) = test_pki();
        assert!(
            !format!("{identity:?}").contains("PRIVATE KEY"),
            "Debug must redact private key PEM"
        );

        let with_ca = client_tls_config(&ClientTlsMaterial {
            ca_certificate_pem: Some(ca_pem.clone()),
            identity: Some(identity.clone()),
        })
        .unwrap();
        assert_eq!(with_ca.alpn_protocols, vec![b"h2".to_vec()]);

        for _ in 0..2 {
            let platform_mtls = client_tls_config(&ClientTlsMaterial {
                ca_certificate_pem: None,
                identity: Some(identity.clone()),
            })
            .unwrap();
            assert_eq!(platform_mtls.alpn_protocols, vec![b"h2".to_vec()]);
        }

        let mismatched = ClientTlsMaterial {
            ca_certificate_pem: Some(ca_pem.clone()),
            identity: Some(ClientIdentity {
                certificate_pem: identity.certificate_pem.clone(),
                private_key_pem: other_key,
            }),
        };
        assert_eq!(
            client_tls_config(&mismatched).unwrap_err(),
            ClientError::InvalidClientTls
        );

        assert_eq!(
            client_tls_config(&ClientTlsMaterial {
                ca_certificate_pem: Some(String::new()),
                identity: None,
            })
            .unwrap_err(),
            ClientError::Tls(TlsMaterialError::InvalidCaCertificate)
        );
        assert_eq!(
            client_tls_config(&ClientTlsMaterial {
                ca_certificate_pem: Some(ca_pem.clone()),
                identity: Some(ClientIdentity {
                    certificate_pem:
                        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".to_owned(),
                    private_key_pem: identity.private_key_pem.clone(),
                }),
            })
            .unwrap_err(),
            ClientError::Tls(TlsMaterialError::InvalidCertificate)
        );
        assert_eq!(
            client_tls_config(&ClientTlsMaterial {
                ca_certificate_pem: Some(ca_pem),
                identity: Some(ClientIdentity {
                    certificate_pem: identity.certificate_pem,
                    private_key_pem: SecretString::from("not a key"),
                }),
            })
            .unwrap_err(),
            ClientError::Tls(TlsMaterialError::InvalidPrivateKey)
        );
    }
}
