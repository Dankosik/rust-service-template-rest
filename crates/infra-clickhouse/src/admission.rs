use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use secrecy::{ExposeSecret as _, SecretString};
use url::Url;
use zeroize::{Zeroize as _, Zeroizing};

use crate::error::StartErrorKind;
use crate::observe::EndpointMetadata;
use crate::{ClickHouseOptions, ClientCertificate, QuerySettings};

const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(5);
const KEEPALIVE_TIME: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_RETRIES: u32 = 3;
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP1_MAX_HEADERS: usize = 100;

const MAX_PASSWORD_FILE_BYTES: usize = 16 * 1024;
const MAX_PEM_CHAIN_BYTES: usize = 1024 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 64 * 1024;

const MIN_TIMEOUT: Duration = Duration::from_millis(10);
const MAX_TIMEOUT: Duration = Duration::from_mins(5);
const MAX_CONCURRENCY_CEILING: usize = 1024;
const MAX_RESULT_ROWS_CEILING: u64 = 10_000_000;
const MIN_RESULT_BYTES: usize = 64 * 1024;
const MAX_RESULT_BYTES: usize = 128 * 1024 * 1024;
const MIN_MEMORY_USAGE: u64 = 16 * 1024 * 1024;
const MAX_MEMORY_USAGE: u64 = 64 * 1024 * 1024 * 1024;

pub(crate) struct Admitted {
    pub(crate) client: clickhouse::Client,
    pub(crate) endpoint: EndpointMetadata,
    pub(crate) password: Option<Zeroizing<String>>,
    pub(crate) password_file: Option<PathBuf>,
    pub(crate) query_settings: QuerySettings,
    pub(crate) query_timeout: Duration,
    pub(crate) insert_timeout: Duration,
    pub(crate) max_concurrency: usize,
    pub(crate) max_result_rows: u64,
    pub(crate) max_result_bytes: usize,
    pub(crate) http_response_buffer_size: usize,
    pub(crate) max_memory_usage: Option<u64>,
    pub(crate) propagate_trace_context: bool,
}

struct AdmittedUrl {
    normalized: String,
    is_https: bool,
    host: String,
    port: u16,
}

struct AdmittedCredentials {
    username: Option<String>,
    password: Option<Zeroizing<String>>,
    password_file: Option<PathBuf>,
}

pub(crate) fn admit(options: &ClickHouseOptions) -> Result<Admitted, StartErrorKind> {
    validate_limits(options)?;
    let url = validate_url(&options.url, options.allow_plaintext)?;
    validate_printable_non_blank(&options.database, StartErrorKind::InvalidDatabase)?;
    let creds = admit_credentials(options)?;
    let tls = admit_tls(
        url.is_https,
        options.root_ca_path.as_deref(),
        options.client_certificate.as_ref(),
    )?;

    let connect_timeout =
        (options.query_timeout.min(options.insert_timeout) / 2).min(CONNECT_TIMEOUT_CAP);
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(connect_timeout));
    http.set_nodelay(true);
    http.set_keepalive(Some(KEEPALIVE_TIME));
    http.set_keepalive_interval(Some(KEEPALIVE_INTERVAL));
    http.set_keepalive_retries(Some(KEEPALIVE_RETRIES));
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    http.set_tcp_user_timeout(Some(TCP_USER_TIMEOUT));

    let connector = if let Some(tls_config) = tls {
        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_only();
        builder.enable_http1().wrap_connector(http)
    } else {
        let tls_config = platform_tls_config()?;
        let builder = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http();
        builder.enable_http1().wrap_connector(http)
    };

    let hyper_client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .pool_timer(TokioTimer::new())
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .http1_max_headers(HTTP1_MAX_HEADERS)
        .build(connector);

    let mut client = clickhouse::Client::with_http_client(hyper_client)
        .with_url(url.normalized)
        .with_database(&options.database);
    if let Some(user) = creds.username {
        client = client.with_user(user);
    }
    if let Some(ref secret) = creds.password {
        client = client.with_password(secret.as_str());
    }

    Ok(Admitted {
        client,
        endpoint: EndpointMetadata {
            database: options.database.clone(),
            host: url.host,
            port: url.port,
        },
        password: creds.password,
        password_file: creds.password_file,
        query_settings: options.query_settings,
        query_timeout: options.query_timeout,
        insert_timeout: options.insert_timeout,
        max_concurrency: options.max_concurrency,
        max_result_rows: options.max_result_rows,
        max_result_bytes: options.max_result_bytes,
        http_response_buffer_size: options.http_response_buffer_size,
        max_memory_usage: options.max_memory_usage,
        propagate_trace_context: options.propagate_trace_context,
    })
}

fn validate_limits(options: &ClickHouseOptions) -> Result<(), StartErrorKind> {
    if !(1..=MAX_CONCURRENCY_CEILING).contains(&options.max_concurrency)
        || options.max_concurrency >= tokio::sync::Semaphore::MAX_PERMITS
    {
        return Err(StartErrorKind::InvalidConcurrency);
    }
    if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&options.query_timeout)
        || !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&options.insert_timeout)
        || !(1..=MAX_RESULT_ROWS_CEILING).contains(&options.max_result_rows)
        || !(MIN_RESULT_BYTES..=MAX_RESULT_BYTES).contains(&options.max_result_bytes)
        || !(MIN_RESULT_BYTES..=options.max_result_bytes)
            .contains(&options.http_response_buffer_size)
    {
        return Err(StartErrorKind::InvalidLimits);
    }
    if let Some(mem) = options.max_memory_usage
        && !(MIN_MEMORY_USAGE..=MAX_MEMORY_USAGE).contains(&mem)
    {
        return Err(StartErrorKind::InvalidLimits);
    }
    Ok(())
}

fn validate_url(raw: &str, allow_plaintext: bool) -> Result<AdmittedUrl, StartErrorKind> {
    if shared_rules::uri::has_whitespace_or_control(raw)
        || shared_rules::uri::authority_has_userinfo(raw)
        || raw.contains('?')
        || raw.contains('#')
    {
        return Err(StartErrorKind::InvalidUrl);
    }
    let parsed = Url::parse(raw).map_err(|_| StartErrorKind::InvalidUrl)?;
    let is_https = match parsed.scheme() {
        "https" => true,
        "http" if allow_plaintext => false,
        "http" => return Err(StartErrorKind::PlaintextRefused),
        _ => return Err(StartErrorKind::InvalidUrl),
    };
    if parsed.cannot_be_a_base() || parsed.host_str().is_none_or(str::is_empty) {
        return Err(StartErrorKind::InvalidUrl);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(StartErrorKind::InvalidUrl);
    }
    let after_scheme = raw.split_once("://").map_or("", |(_, rest)| rest);
    let authority = after_scheme.split('/').next().unwrap_or("");
    if authority.contains('@') {
        return Err(StartErrorKind::InvalidUrl);
    }
    if let Some((_, raw_path)) = after_scheme.split_once('/') {
        let path_with_slash = format!("/{raw_path}");
        let trimmed = path_with_slash
            .strip_suffix('/')
            .unwrap_or(&path_with_slash);
        if !trimmed.is_empty() {
            if trimmed.contains("//") {
                return Err(StartErrorKind::InvalidUrl);
            }
            for segment in trimmed.trim_start_matches('/').split('/') {
                if segment.is_empty()
                    || segment == "."
                    || segment == ".."
                    || segment.eq_ignore_ascii_case("%2e")
                    || segment.eq_ignore_ascii_case("%2e%2e")
                {
                    return Err(StartErrorKind::InvalidUrl);
                }
            }
        }
    }
    let mut normalized = parsed;
    let clean_path = normalized.path().trim_end_matches('/').to_owned();
    normalized.set_path(&clean_path);
    let host = normalized
        .host_str()
        .ok_or(StartErrorKind::InvalidUrl)?
        .to_owned();
    let port = normalized
        .port_or_known_default()
        .ok_or(StartErrorKind::InvalidUrl)?;
    Ok(AdmittedUrl {
        normalized: normalized.into(),
        is_https,
        host,
        port,
    })
}

fn admit_credentials(options: &ClickHouseOptions) -> Result<AdmittedCredentials, StartErrorKind> {
    if let Some(username) = options.username.as_deref() {
        validate_header_safe_non_empty(username, StartErrorKind::InvalidCredentials)?;
    }
    let inline_password = options
        .password
        .as_ref()
        .map(SecretString::expose_secret)
        .filter(|value| !value.trim().is_empty());
    if let Some(password) = inline_password {
        validate_header_safe_non_empty(password, StartErrorKind::InvalidCredentials)?;
    }

    if options.allow_unauthenticated {
        if inline_password.is_some() || options.password_file.is_some() {
            return Err(StartErrorKind::InvalidCredentials);
        }
        return Ok(AdmittedCredentials {
            username: options.username.clone(),
            password: None,
            password_file: None,
        });
    }

    let Some(username) = options.username.clone() else {
        return Err(StartErrorKind::UnauthenticatedRefused);
    };
    match (inline_password, &options.password_file) {
        (Some(_), Some(_)) => Err(StartErrorKind::PasswordConflict),
        (Some(password), None) => Ok(AdmittedCredentials {
            username: Some(username),
            password: Some(Zeroizing::new(password.to_owned())),
            password_file: None,
        }),
        (None, Some(path)) => {
            let password = read_password_file(path)?;
            Ok(AdmittedCredentials {
                username: Some(username),
                password: Some(password),
                password_file: Some(path.clone()),
            })
        }
        (None, None) => Err(StartErrorKind::UnauthenticatedRefused),
    }
}

fn admit_tls(
    is_https: bool,
    root_ca_path: Option<&Path>,
    client_certificate: Option<&ClientCertificate>,
) -> Result<Option<rustls::ClientConfig>, StartErrorKind> {
    if !is_https {
        if root_ca_path.is_some() {
            return Err(StartErrorKind::CaRequiresTls);
        }
        if client_certificate.is_some() {
            return Err(StartErrorKind::ClientCertificateRequiresTls);
        }
        return Ok(None);
    }
    let roots = root_ca_path.map(read_root_ca).transpose()?;
    let identity = client_certificate
        .map(read_client_certificate)
        .transpose()?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| StartErrorKind::ClientBuild)?;
    let builder = if let Some(roots) = roots {
        builder.with_root_certificates(roots)
    } else {
        use rustls_platform_verifier::BuilderVerifierExt as _;
        builder
            .with_platform_verifier()
            .map_err(|_| StartErrorKind::ClientBuild)?
    };
    let config = match identity {
        Some((chain, key)) => {
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|error| match error {
                    rustls::Error::InvalidCertificate(_) => {
                        StartErrorKind::InvalidClientCertificate
                    }
                    _ => StartErrorKind::InvalidClientKey,
                })?
        }
        None => builder.with_no_client_auth(),
    };
    Ok(Some(config))
}

fn platform_tls_config() -> Result<rustls::ClientConfig, StartErrorKind> {
    use rustls_platform_verifier::BuilderVerifierExt as _;

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| StartErrorKind::ClientBuild)?
        .with_platform_verifier()
        .map_err(|_| StartErrorKind::ClientBuild)?;
    Ok(builder.with_no_client_auth())
}

fn read_root_ca(path: &Path) -> Result<rustls::RootCertStore, StartErrorKind> {
    use rustls::pki_types::pem::PemObject as _;

    let bytes = read_regular_file(path, MAX_PEM_CHAIN_BYTES)
        .map_err(|error| StartErrorKind::CaFile { kind: error.kind() })?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StartErrorKind::InvalidCa)?;
    if certs.is_empty() {
        return Err(StartErrorKind::InvalidCa);
    }
    let mut roots = rustls::RootCertStore::empty();
    for cert in certs {
        roots.add(cert).map_err(|_| StartErrorKind::InvalidCa)?;
    }
    Ok(roots)
}

fn read_client_certificate(
    cert: &ClientCertificate,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), StartErrorKind> {
    use rustls::pki_types::pem::PemObject as _;

    let cert_bytes = read_regular_file(&cert.cert_path, MAX_PEM_CHAIN_BYTES)
        .map_err(|error| StartErrorKind::ClientCertificateFile { kind: error.kind() })?;
    let key_bytes = read_secret_file(&cert.key_path, MAX_PRIVATE_KEY_BYTES)
        .map_err(|error| StartErrorKind::ClientKeyFile { kind: error.kind() })?;
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StartErrorKind::InvalidClientCertificate)?;
    if chain.is_empty() {
        return Err(StartErrorKind::InvalidClientCertificate);
    }
    let key =
        PrivateKeyDer::from_pem_slice(&key_bytes).map_err(|_| StartErrorKind::InvalidClientKey)?;
    Ok((chain, key))
}

pub(crate) fn read_password_file(path: &Path) -> Result<Zeroizing<String>, StartErrorKind> {
    let mut bytes = read_secret_file(path, MAX_PASSWORD_FILE_BYTES)
        .map_err(|error| StartErrorKind::PasswordFile { kind: error.kind() })?;
    let raw = std::mem::take(&mut *bytes);
    let mut content = String::from_utf8(raw)
        .map(Zeroizing::new)
        .map_err(|error| {
            error.into_bytes().zeroize();
            StartErrorKind::PasswordFile {
                kind: ErrorKind::InvalidData,
            }
        })?;
    let trimmed = Zeroizing::new(shared_rules::secret_file::value(std::mem::take(
        &mut *content,
    )));
    if trimmed.is_empty() || trimmed.bytes().any(is_ascii_ctl) {
        return Err(StartErrorKind::PasswordFileEmpty);
    }
    Ok(trimmed)
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "blocking workers must read from the same descriptor they validated as a regular file"
)]
fn read_regular_file_into(path: &Path, max_bytes: usize, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(max_bytes)
        .min(max_bytes)
        .saturating_add(1);
    bytes.reserve_exact(capacity);
    file.take(max_bytes as u64 + 1).read_to_end(bytes)?;
    if bytes.len() > max_bytes {
        return Err(ErrorKind::InvalidData.into());
    }
    Ok(())
}

fn read_regular_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    read_regular_file_into(path, max_bytes, &mut bytes)?;
    Ok(bytes)
}

fn read_secret_file(path: &Path, max_bytes: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    read_regular_file_into(path, max_bytes, &mut bytes)?;
    Ok(bytes)
}

fn validate_printable_non_blank(value: &str, err: StartErrorKind) -> Result<(), StartErrorKind> {
    if value.trim().is_empty() || value.bytes().any(is_ascii_ctl) {
        return Err(err);
    }
    Ok(())
}

fn validate_header_safe_non_empty(value: &str, err: StartErrorKind) -> Result<(), StartErrorKind> {
    if value.is_empty() || value.bytes().any(is_ascii_ctl) {
        return Err(err);
    }
    Ok(())
}

const fn is_ascii_ctl(byte: u8) -> bool {
    byte < 0x20 || byte == 0x7f
}
