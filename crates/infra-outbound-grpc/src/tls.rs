use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use rustls::server::ParsedCertificate;
use rustls::{ClientConfig, ConfigBuilder, InconsistentKeys, RootCertStore, WantsVerifier};
use rustls_platform_verifier::Verifier;
use secrecy::ExposeSecret as _;

use crate::{ClientError, ClientIdentity, ClientTlsMaterial, TlsMaterialError};

pub(crate) fn client_tls_config(
    material: &ClientTlsMaterial,
) -> Result<Arc<ClientConfig>, ClientError> {
    if material.ca_certificate_pem.is_none() && material.identity.is_none() {
        return default_tls_config();
    }
    Ok(Arc::new(build_client_tls_config(material)?))
}

struct FallibleOnce<T> {
    value: OnceLock<T>,
    init: Mutex<()>,
}

impl<T> FallibleOnce<T> {
    const fn new() -> Self {
        Self {
            value: OnceLock::new(),
            init: Mutex::new(()),
        }
    }

    fn get_or_try_init<E>(&self, init: impl FnOnce() -> Result<T, E>) -> Result<&T, E> {
        if let Some(value) = self.value.get() {
            return Ok(value);
        }
        let _guard = self.init.lock();
        if let Some(value) = self.value.get() {
            return Ok(value);
        }
        let value = init()?;
        Ok(self.value.get_or_init(|| value))
    }
}

fn default_tls_config() -> Result<Arc<ClientConfig>, ClientError> {
    static DEFAULT_CONFIG: FallibleOnce<Arc<ClientConfig>> = FallibleOnce::new();
    DEFAULT_CONFIG
        .get_or_try_init(|| build_client_tls_config(&ClientTlsMaterial::default()).map(Arc::new))
        .map(Arc::clone)
}

fn build_client_tls_config(material: &ClientTlsMaterial) -> Result<ClientConfig, ClientError> {
    let roots = material
        .ca_certificate_pem
        .as_deref()
        .map(parse_ca_roots)
        .transpose()?;
    let identity = material
        .identity
        .as_ref()
        .map(parse_client_identity)
        .transpose()?;
    let builder = match roots {
        Some(roots) => tls13_builder()?.with_root_certificates(roots),
        None => tls13_builder()?
            .dangerous()
            .with_custom_certificate_verifier(platform_verifier()?),
    };
    let mut config = match identity {
        Some((certs, key)) => builder
            .with_client_auth_cert(certs, key)
            .map_err(identity_setup_error)?,
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"h2".to_vec()];
    Ok(config)
}

fn platform_verifier() -> Result<Arc<Verifier>, ClientError> {
    static PLATFORM_VERIFIER: FallibleOnce<Arc<Verifier>> = FallibleOnce::new();
    PLATFORM_VERIFIER
        .get_or_try_init(|| {
            Verifier::new(crypto_provider())
                .map(Arc::new)
                .map_err(|_| ClientError::ClientVerifierSetup)
        })
        .map(Arc::clone)
}

fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

fn tls13_builder() -> Result<ConfigBuilder<ClientConfig, WantsVerifier>, ClientError> {
    ClientConfig::builder_with_provider(crypto_provider())
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
    for item in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        let cert = item.map_err(|_| TlsMaterialError::InvalidCaCertificate(None))?;
        roots
            .add(cert)
            .map_err(|error| TlsMaterialError::InvalidCaCertificate(Some(error)))?;
    }
    if roots.is_empty() {
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
    fn fallible_once_retries_errors_and_serializes_concurrent_init() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc;

        let cell = FallibleOnce::new();
        let attempts = AtomicUsize::new(0);
        assert_eq!(
            cell.get_or_try_init(|| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<usize, &'static str>("transient")
            }),
            Err("transient")
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let cell_ref = &cell;
        let attempts_ref = &attempts;
        std::thread::scope(|scope| {
            let first = scope.spawn(move || {
                *cell_ref
                    .get_or_try_init(|| -> Result<usize, &'static str> {
                        let count = attempts_ref.fetch_add(1, Ordering::SeqCst);
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(count)
                    })
                    .unwrap()
            });
            entered_rx.recv().unwrap();
            let second = scope.spawn(move || {
                started_tx.send(()).unwrap();
                let value = *cell_ref
                    .get_or_try_init(|| -> Result<usize, &'static str> {
                        Ok(attempts_ref.fetch_add(1, Ordering::SeqCst))
                    })
                    .unwrap();
                second_tx.send(value).unwrap();
                value
            });
            started_rx.recv().unwrap();
            for _ in 0..16 {
                std::thread::yield_now();
            }
            assert!(second_rx.try_recv().is_err());
            release_tx.send(()).unwrap();
            assert_eq!(first.join().unwrap(), 1);
            assert_eq!(second.join().unwrap(), 1);
            assert_eq!(second_rx.recv().unwrap(), 1);
        });
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
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
            let diagnostics = shared_rules::error_chain::causes(&error)
                .map(|cause| format!("{cause}; {cause:?}"));
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
