use std::path::{Path, PathBuf};
use std::time::Duration;

use redis::IntoConnectionInfo;
use secrecy::ExposeSecret;
use socket2::{Domain, Protocol, Socket, Type};

use crate::credentials::PasswordFile;
use crate::{CacheError, CacheOptions, ClientCertificate, ServerEndpoint};

const KEEPALIVE_TIME: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const USER_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Admitted {
    pub(crate) server: ServerEndpoint,
    pub(crate) client: redis::Client,
    pub(crate) password_file: Option<PasswordFile>,
}

pub(crate) fn admit(options: CacheOptions) -> Result<Admitted, CacheError> {
    let CacheOptions {
        dsn,
        password_file,
        root_ca_path,
        client_certificate,
        allow_plaintext,
        allow_unauthenticated,
        command_timeout: _,
    } = options;
    let info = dsn
        .expose_secret()
        .into_connection_info()
        .map_err(|_| CacheError::InvalidDsn)?;
    let server = admit_address(
        &info,
        allow_plaintext,
        root_ca_path.is_some(),
        client_certificate.is_some(),
    )?;
    let password_file =
        admit_password(info.redis_settings(), password_file, allow_unauthenticated)?;
    let tls_certificates =
        read_tls_certificates(root_ca_path.as_deref(), client_certificate.as_ref())?;
    let settings = info
        .redis_settings()
        .clone()
        .set_protocol(redis::ProtocolVersion::RESP3);
    let info = info
        .set_redis_settings(settings)
        .set_tcp_settings(tcp_settings());
    if server.tls {
        install_tls_provider();
    }
    let client = match tls_certificates {
        Some(certificates) => redis::Client::build_with_tls(info, certificates),
        None => redis::Client::open(info),
    }
    .map_err(|_| CacheError::ClientBuild)?;
    Ok(Admitted {
        server,
        client,
        password_file,
    })
}

pub(crate) fn check_runtime() -> Result<(), CacheError> {
    if tokio::runtime::Handle::try_current().is_err() {
        return Err(CacheError::NoRuntime);
    }
    std::panic::catch_unwind(|| drop(tokio::time::sleep(Duration::ZERO)))
        .map_err(|_| CacheError::NoTimeDriver)?;

    let io_error = |error: std::io::Error| CacheError::IoDriverCheck { kind: error.kind() };
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).map_err(io_error)?;
    socket.set_nonblocking(true).map_err(io_error)?;
    std::panic::catch_unwind(|| tokio::net::TcpStream::from_std(socket.into()))
        .map_err(|_| CacheError::NoIoDriver)?
        .map(drop)
        .map_err(io_error)
}

fn admit_address(
    info: &redis::ConnectionInfo,
    allow_plaintext: bool,
    has_root_ca: bool,
    has_client_certificate: bool,
) -> Result<ServerEndpoint, CacheError> {
    match info.addr() {
        redis::ConnectionAddr::Tcp(host, port) => {
            if !allow_plaintext {
                return Err(CacheError::PlaintextRefused);
            }
            if has_root_ca {
                return Err(CacheError::CaRequiresTls);
            }
            if has_client_certificate {
                return Err(CacheError::ClientCertificateRequiresTls);
            }
            Ok(ServerEndpoint {
                host: host.clone(),
                port: *port,
                tls: false,
            })
        }
        redis::ConnectionAddr::TcpTls { insecure: true, .. } => Err(CacheError::InsecureTlsRefused),
        redis::ConnectionAddr::TcpTls {
            host,
            port,
            insecure: false,
            ..
        } => Ok(ServerEndpoint {
            host: host.clone(),
            port: *port,
            tls: true,
        }),
        _ => Err(CacheError::UnsupportedAddress),
    }
}

fn admit_password(
    settings: &redis::RedisConnectionInfo,
    password_file: Option<PathBuf>,
    allow_unauthenticated: bool,
) -> Result<Option<PasswordFile>, CacheError> {
    let has_password = settings
        .password()
        .is_some_and(|password| !password.is_empty());
    match password_file {
        Some(_) if has_password => Err(CacheError::PasswordInDsn),
        Some(path) => PasswordFile::admit(path, settings.username()).map(Some),
        None if has_password || allow_unauthenticated => Ok(None),
        None => Err(CacheError::UnauthenticatedRefused),
    }
}

fn read_tls_certificates(
    root_ca_path: Option<&Path>,
    client_certificate: Option<&ClientCertificate>,
) -> Result<Option<redis::TlsCertificates>, CacheError> {
    let root_cert = root_ca_path.map(read_root_ca).transpose()?;
    let client_tls = client_certificate
        .map(read_client_certificate)
        .transpose()?;
    if root_cert.is_none() && client_tls.is_none() {
        return Ok(None);
    }
    Ok(Some(redis::TlsCertificates {
        client_tls,
        root_cert,
    }))
}

#[expect(
    clippy::disallowed_methods,
    reason = "startup cache admission owns finite TLS material reads before serving"
)]
fn read_root_ca(path: &Path) -> Result<Vec<u8>, CacheError> {
    let bytes = std::fs::read(path).map_err(|error| CacheError::CaFile { kind: error.kind() })?;
    let mut roots = rustls::RootCertStore::empty();
    for certificate in pem_certificates(&bytes).ok_or(CacheError::InvalidCa)? {
        roots.add(certificate).map_err(|_| CacheError::InvalidCa)?;
    }
    Ok(bytes)
}

#[expect(
    clippy::disallowed_methods,
    reason = "startup cache admission owns finite TLS material reads before serving"
)]
fn read_client_certificate(
    certificate: &ClientCertificate,
) -> Result<redis::ClientTlsConfig, CacheError> {
    use rustls::pki_types::PrivateKeyDer;
    use rustls::pki_types::pem::PemObject;

    let client_cert = std::fs::read(&certificate.cert_path)
        .map_err(|error| CacheError::ClientCertificateFile { kind: error.kind() })?;
    let client_key = std::fs::read(&certificate.key_path)
        .map_err(|error| CacheError::ClientKeyFile { kind: error.kind() })?;
    let chain = pem_certificates(&client_cert).ok_or(CacheError::InvalidClientCertificate)?;
    let key =
        PrivateKeyDer::from_pem_slice(&client_key).map_err(|_| CacheError::InvalidClientKey)?;
    rustls::sign::CertifiedKey::from_der(
        chain,
        key,
        &rustls::crypto::aws_lc_rs::default_provider(),
    )
    .map_err(|error| match error {
        rustls::Error::InvalidCertificate(_) => CacheError::InvalidClientCertificate,
        _ => CacheError::InvalidClientKey,
    })?;
    Ok(redis::ClientTlsConfig {
        client_cert,
        client_key,
    })
}

fn pem_certificates(bytes: &[u8]) -> Option<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;

    CertificateDer::pem_slice_iter(bytes)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|certificates| !certificates.is_empty())
}

fn tcp_settings() -> redis::io::tcp::TcpSettings {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_TIME)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_RETRIES);
    let settings = redis::io::tcp::TcpSettings::default()
        .set_nodelay(true)
        .set_keepalive(keepalive);
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    let settings = settings.set_user_timeout(USER_TIMEOUT);
    settings
}

fn install_tls_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
