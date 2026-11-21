use std::path::{Path, PathBuf};
use std::sync::Arc;

use redis::IntoConnectionInfo;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use secrecy::ExposeSecret;

use crate::credentials::{PasswordFile, read_regular_file, read_secret_file};
use crate::dial::{Tls, Transport};
use crate::error::StartErrorKind;
use crate::{CacheOptions, ClientCertificate, ServerEndpoint};

const MAX_PEM_CHAIN_BYTES: usize = 1024 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 64 * 1024;

pub(crate) struct Admitted {
    pub(crate) server: ServerEndpoint,
    pub(crate) transport: Transport,
    pub(crate) settings: redis::RedisConnectionInfo,
    pub(crate) password_file: Option<PasswordFile>,
}

enum AdmittedAddress {
    Plaintext(ServerEndpoint),
    Tls {
        server: ServerEndpoint,
        root_ca_path: Option<PathBuf>,
        client_certificate: Option<ClientCertificate>,
    },
}

pub(crate) fn admit(options: CacheOptions) -> Result<Admitted, StartErrorKind> {
    let CacheOptions {
        dsn,
        password_file,
        root_ca_path,
        client_certificate,
        allow_plaintext,
        allow_unauthenticated,
        ..
    } = options;
    let info = dsn
        .expose_secret()
        .into_connection_info()
        .map_err(|_| StartErrorKind::InvalidDsn)?;
    if info.redis_settings().db() < 0 {
        return Err(StartErrorKind::InvalidDsn);
    }
    let address = admit_address(&info, allow_plaintext, root_ca_path, client_certificate)?;
    let password_file =
        admit_password(info.redis_settings(), password_file, allow_unauthenticated)?;
    let (server, transport) = match address {
        AdmittedAddress::Plaintext(server) => (server, Transport::Plaintext),
        AdmittedAddress::Tls {
            server,
            root_ca_path,
            client_certificate,
        } => {
            let roots = root_ca_path.as_deref().map(read_root_ca).transpose()?;
            let identity = client_certificate
                .as_ref()
                .map(read_client_certificate)
                .transpose()?;
            let transport = Transport::Tls(Tls {
                connector: tls_connector(roots, identity)?,
                server_name: ServerName::try_from(server.host.clone())
                    .map_err(|_| StartErrorKind::InvalidDsn)?,
            });
            (server, transport)
        }
    };
    let settings = info
        .redis_settings()
        .clone()
        .set_protocol(redis::ProtocolVersion::RESP3);
    Ok(Admitted {
        server,
        transport,
        settings,
        password_file,
    })
}

fn admit_address(
    info: &redis::ConnectionInfo,
    allow_plaintext: bool,
    root_ca_path: Option<PathBuf>,
    client_certificate: Option<ClientCertificate>,
) -> Result<AdmittedAddress, StartErrorKind> {
    match info.addr() {
        redis::ConnectionAddr::Tcp(host, port) => {
            if !allow_plaintext {
                return Err(StartErrorKind::PlaintextRefused);
            }
            if root_ca_path.is_some() {
                return Err(StartErrorKind::CaRequiresTls);
            }
            if client_certificate.is_some() {
                return Err(StartErrorKind::ClientCertificateRequiresTls);
            }
            Ok(AdmittedAddress::Plaintext(ServerEndpoint {
                host: host.clone(),
                port: *port,
                database_index: info.redis_settings().db(),
            }))
        }
        redis::ConnectionAddr::TcpTls { insecure: true, .. } => {
            Err(StartErrorKind::InsecureTlsRefused)
        }
        redis::ConnectionAddr::TcpTls {
            host,
            port,
            insecure: false,
            ..
        } => Ok(AdmittedAddress::Tls {
            server: ServerEndpoint {
                host: host.clone(),
                port: *port,
                database_index: info.redis_settings().db(),
            },
            root_ca_path,
            client_certificate,
        }),
        _ => Err(StartErrorKind::UnsupportedAddress),
    }
}

fn admit_password(
    settings: &redis::RedisConnectionInfo,
    password_file: Option<PathBuf>,
    allow_unauthenticated: bool,
) -> Result<Option<PasswordFile>, StartErrorKind> {
    let has_password = settings
        .password()
        .is_some_and(|password| !password.is_empty());
    match password_file {
        Some(_) if has_password => Err(StartErrorKind::PasswordInDsn),
        Some(path) => PasswordFile::admit(path, settings.username()).map(Some),
        None if has_password || allow_unauthenticated => Ok(None),
        None => Err(StartErrorKind::UnauthenticatedRefused),
    }
}

fn read_root_ca(path: &Path) -> Result<rustls::RootCertStore, StartErrorKind> {
    let bytes = read_regular_file(path, MAX_PEM_CHAIN_BYTES)
        .map_err(|error| StartErrorKind::CaFile { kind: error.kind() })?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in pem_certificates(&bytes).ok_or(StartErrorKind::InvalidCa)? {
        roots
            .add(certificate)
            .map_err(|_| StartErrorKind::InvalidCa)?;
    }
    Ok(roots)
}

struct ClientIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

fn read_client_certificate(
    certificate: &ClientCertificate,
) -> Result<ClientIdentity, StartErrorKind> {
    use rustls::pki_types::pem::PemObject;

    let client_cert = read_regular_file(&certificate.cert_path, MAX_PEM_CHAIN_BYTES)
        .map_err(|error| StartErrorKind::ClientCertificateFile { kind: error.kind() })?;
    let client_key = read_secret_file(&certificate.key_path, MAX_PRIVATE_KEY_BYTES)
        .map_err(|error| StartErrorKind::ClientKeyFile { kind: error.kind() })?;
    let chain = pem_certificates(&client_cert).ok_or(StartErrorKind::InvalidClientCertificate)?;
    let key =
        PrivateKeyDer::from_pem_slice(&client_key).map_err(|_| StartErrorKind::InvalidClientKey)?;
    Ok(ClientIdentity { chain, key })
}

fn tls_connector(
    roots: Option<rustls::RootCertStore>,
    identity: Option<ClientIdentity>,
) -> Result<tokio_rustls::TlsConnector, StartErrorKind> {
    use rustls_platform_verifier::BuilderVerifierExt as _;

    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| StartErrorKind::ClientBuild)?;
    let builder = match roots {
        Some(roots) => builder.with_root_certificates(roots),
        None => builder
            .with_platform_verifier()
            .map_err(|_| StartErrorKind::ClientBuild)?,
    };
    let config =
        match identity {
            Some(ClientIdentity { chain, key }) => builder
                .with_client_auth_cert(chain, key)
                .map_err(|error| match error {
                    rustls::Error::InvalidCertificate(_) => {
                        StartErrorKind::InvalidClientCertificate
                    }
                    _ => StartErrorKind::InvalidClientKey,
                })?,
            None => builder.with_no_client_auth(),
        };
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

fn pem_certificates(bytes: &[u8]) -> Option<Vec<CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;

    CertificateDer::pem_slice_iter(bytes)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|certificates| !certificates.is_empty())
}
