use std::fmt;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
use rustls::server::ParsedCertificate;
use rustls::{ClientConfig, ConfigBuilder, RootCertStore, ServerConfig, WantsVerifier};
use secrecy::{ExposeSecret as _, SecretString};

#[derive(Clone, Debug, PartialEq)]
pub enum CertParseError {
    MalformedPem,
    InvalidDer(rustls::Error),
}

impl fmt::Display for CertParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedPem => formatter.write_str("malformed PEM certificate data"),
            Self::InvalidDer(_) => formatter.write_str("invalid DER certificate"),
        }
    }
}

impl std::error::Error for CertParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MalformedPem => None,
            Self::InvalidDer(source) => Some(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidPrivateKey;

impl fmt::Display for InvalidPrivateKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid PEM private key")
    }
}

impl std::error::Error for InvalidPrivateKey {}

pub fn parse_cert_chain(pem: &str) -> Result<Vec<CertificateDer<'static>>, CertParseError> {
    let mut chain = Vec::new();
    for item in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = item.map_err(|_| CertParseError::MalformedPem)?;
        ParsedCertificate::try_from(&cert).map_err(CertParseError::InvalidDer)?;
        chain.push(cert);
    }
    if chain.is_empty() {
        return Err(CertParseError::MalformedPem);
    }
    Ok(chain)
}

pub fn parse_ca_roots(pem: &str) -> Result<RootCertStore, CertParseError> {
    let mut roots = RootCertStore::empty();
    for item in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = item.map_err(|_| CertParseError::MalformedPem)?;
        roots.add(cert).map_err(CertParseError::InvalidDer)?;
    }
    if roots.is_empty() {
        return Err(CertParseError::MalformedPem);
    }
    Ok(roots)
}

pub fn parse_private_key(pem: &SecretString) -> Result<PrivateKeyDer<'static>, InvalidPrivateKey> {
    PrivateKeyDer::from_pem_slice(pem.expose_secret().as_bytes()).map_err(|_| InvalidPrivateKey)
}

#[must_use]
pub fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

#[must_use]
pub fn tls13_server_builder() -> ConfigBuilder<ServerConfig, WantsVerifier> {
    #[expect(
        clippy::expect_used,
        reason = "a fixed provider and version list; failure is a build defect, not operator input"
    )]
    ServerConfig::builder_with_provider(crypto_provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("the aws-lc-rs provider supports TLS 1.3")
}

#[must_use]
pub fn tls13_client_builder() -> ConfigBuilder<ClientConfig, WantsVerifier> {
    #[expect(
        clippy::expect_used,
        reason = "a fixed provider and version list; failure is a build defect, not operator input"
    )]
    ClientConfig::builder_with_provider(crypto_provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("the aws-lc-rs provider supports TLS 1.3")
}
