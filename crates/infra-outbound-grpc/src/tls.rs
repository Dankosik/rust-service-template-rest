use std::sync::{Arc, OnceLock};

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use rustls::server::ParsedCertificate;
use rustls::{ClientConfig, ConfigBuilder, InconsistentKeys, RootCertStore, WantsVerifier};
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
                .map_err(identity_setup_error)?;
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
                        .map_err(identity_setup_error)?
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
        .map_err(|_| ClientError::ClientVerifierSetup)?
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(Arc::clone(DEFAULT_CONFIG.get_or_init(|| Arc::new(config))))
}

fn platform_verifier() -> Result<Arc<Verifier>, ClientError> {
    static PLATFORM_VERIFIER: OnceLock<Arc<Verifier>> = OnceLock::new();
    if let Some(verifier) = PLATFORM_VERIFIER.get() {
        return Ok(Arc::clone(verifier));
    }
    let verifier = Verifier::new(tls13_provider()).map_err(|_| ClientError::ClientVerifierSetup)?;
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
        .map_err(ClientError::ClientProtocolSetup)
}

fn identity_setup_error(error: rustls::Error) -> ClientError {
    match error {
        rustls::Error::InconsistentKeys(InconsistentKeys::KeyMismatch) => {
            ClientError::ClientIdentityMismatch(error)
        }
        _ => ClientError::ClientIdentitySetup(error),
    }
}

fn parse_ca_roots(ca_pem: &str) -> Result<RootCertStore, TlsMaterialError> {
    let mut roots = RootCertStore::empty();
    let mut count = 0_usize;
    for item in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        let cert = item.map_err(|_| TlsMaterialError::InvalidCaCertificate(None))?;
        roots
            .add(cert)
            .map_err(|error| TlsMaterialError::InvalidCaCertificate(Some(error)))?;
        count += 1;
    }
    if count == 0 {
        return Err(TlsMaterialError::InvalidCaCertificate(None));
    }
    Ok(roots)
}

fn parse_client_identity(
    identity: &ClientIdentity,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsMaterialError> {
    let mut chain = Vec::new();
    for item in CertificateDer::pem_slice_iter(identity.certificate_pem.as_bytes()) {
        let cert = item.map_err(|_| TlsMaterialError::InvalidCertificate(None))?;
        ParsedCertificate::try_from(&cert)
            .map_err(|error| TlsMaterialError::InvalidCertificate(Some(error)))?;
        chain.push(cert);
    }
    if chain.is_empty() {
        return Err(TlsMaterialError::InvalidCertificate(None));
    }
    let key = PrivateKeyDer::from_pem_slice(identity.private_key_pem.expose_secret().as_bytes())
        .map_err(|_| TlsMaterialError::InvalidPrivateKey)?;
    Ok((chain, key))
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::error::Error as _;

    use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};
    use secrecy::SecretString;
    use tls_test_support::new_issuer;

    use super::*;

    fn test_pki() -> (String, ClientIdentity, SecretString) {
        let issuer = new_issuer();
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

        for ca_certificate_pem in [Some(ca_pem.clone()), None] {
            let mismatched = ClientTlsMaterial {
                ca_certificate_pem,
                identity: Some(ClientIdentity {
                    certificate_pem: identity.certificate_pem.clone(),
                    private_key_pem: other_key.clone(),
                }),
            };
            let error = client_tls_config(&mismatched).unwrap_err();
            assert_matches!(&error, ClientError::ClientIdentityMismatch(_));
            assert!(error.to_string().contains("does not match the certificate"));
            assert_matches!(
                error.source().and_then(|source| source.downcast_ref()),
                Some(rustls::Error::InconsistentKeys(
                    InconsistentKeys::KeyMismatch
                ))
            );
            let diagnostics: Vec<_> = shared_rules::error_chain::causes(&error)
                .map(|cause| format!("{cause}; {cause:?}"))
                .collect();
            for formatted in std::iter::once(format!("{mismatched:?}")).chain(diagnostics) {
                assert!(!formatted.contains("-----BEGIN"));
                assert!(!formatted.contains(identity.certificate_pem.lines().nth(1).unwrap()));
                assert!(!formatted.contains(other_key.expose_secret().lines().nth(1).unwrap()));
            }
        }

        assert_matches!(
            client_tls_config(&ClientTlsMaterial {
                ca_certificate_pem: Some(String::new()),
                identity: None,
            })
            .unwrap_err(),
            ClientError::Tls(TlsMaterialError::InvalidCaCertificate(None))
        );
        assert_matches!(
            client_tls_config(&ClientTlsMaterial {
                ca_certificate_pem: Some(ca_pem.clone()),
                identity: Some(ClientIdentity {
                    certificate_pem:
                        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n".to_owned(),
                    private_key_pem: identity.private_key_pem.clone(),
                }),
            })
            .unwrap_err(),
            ClientError::Tls(TlsMaterialError::InvalidCertificate(Some(_)))
        );
        let malformed = client_tls_config(&ClientTlsMaterial {
            ca_certificate_pem: Some(ca_pem.clone()),
            identity: Some(ClientIdentity {
                certificate_pem: identity.certificate_pem.clone(),
                private_key_pem: SecretString::from("-----BEGIN PRIVATE KEY secret-credential"),
            }),
        })
        .unwrap_err();
        assert_matches!(
            &malformed,
            ClientError::Tls(TlsMaterialError::InvalidPrivateKey)
        );
        assert!(malformed.source().is_none());
        assert!(!format!("{malformed}; {malformed:?}").contains("secret-credential"));

        let invalid_der = client_tls_config(&ClientTlsMaterial {
            ca_certificate_pem: Some(ca_pem),
            identity: Some(ClientIdentity {
                certificate_pem: identity.certificate_pem,
                private_key_pem: SecretString::from(
                    "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
                ),
            }),
        })
        .unwrap_err();
        assert_matches!(&invalid_der, ClientError::ClientIdentitySetup(_));
        assert_matches!(
            invalid_der
                .source()
                .and_then(|source| source.downcast_ref()),
            Some(rustls::Error::General(_))
        );
        for cause in shared_rules::error_chain::causes(&invalid_der) {
            assert!(!format!("{cause}; {cause:?}").contains("AAAA"));
        }
    }
}
