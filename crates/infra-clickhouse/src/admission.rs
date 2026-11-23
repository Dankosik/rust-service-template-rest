use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use secrecy::{ExposeSecret as _, SecretString};
use shared_rules::clickhouse::{
    MAX_CONCURRENCY_CEILING, MAX_MEMORY_USAGE, MAX_RESULT_BYTES, MAX_RESULT_ROWS_CEILING,
    MAX_TIMEOUT, MIN_MEMORY_USAGE, MIN_RESULT_BYTES, MIN_TIMEOUT, strip_root_trailing_slash,
    trim_path_prefix, validate_url_lexical,
};
use shared_rules::rfc9110::has_ascii_control;
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

pub(crate) struct Admitted {
    pub(crate) client: clickhouse::Client,
    pub(crate) endpoint: EndpointMetadata,
    pub(crate) password: Option<Zeroizing<String>>,
    pub(crate) password_file: Option<PathBuf>,
    pub(crate) query_settings: QuerySettings,
    pub(crate) query_timeout: Duration,
    pub(crate) insert_timeout: Duration,
    pub(crate) max_read_concurrency: usize,
    pub(crate) max_write_concurrency: usize,
    pub(crate) max_result_rows: u64,
    pub(crate) max_result_bytes: usize,
    pub(crate) http_response_buffer_size: usize,
    pub(crate) max_memory_usage: Option<u64>,
    pub(crate) propagate_trace_context: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct AdmittedUrl {
    pub(crate) normalized: String,
    pub(crate) is_https: bool,
    pub(crate) host: String,
    pub(crate) port: u16,
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
    http.set_connect_timeout(Some(connect_timeout));
    http.set_nodelay(true);
    http.set_keepalive(Some(KEEPALIVE_TIME));
    http.set_keepalive_interval(Some(KEEPALIVE_INTERVAL));
    http.set_keepalive_retries(Some(KEEPALIVE_RETRIES));
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    http.set_tcp_user_timeout(Some(TCP_USER_TIMEOUT));

    let mut client = if let Some(tls_config) = tls {
        http.enforce_http(false);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_only()
            .enable_http1()
            .wrap_connector(http);
        build_clickhouse_client(connector)
    } else {
        http.enforce_http(true);
        build_clickhouse_client(http)
    }
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
        max_read_concurrency: options.max_read_concurrency,
        max_write_concurrency: options.max_write_concurrency,
        max_result_rows: options.max_result_rows,
        max_result_bytes: options.max_result_bytes,
        http_response_buffer_size: options.http_response_buffer_size,
        max_memory_usage: options.max_memory_usage,
        propagate_trace_context: options.propagate_trace_context,
    })
}

fn validate_limits(options: &ClickHouseOptions) -> Result<(), StartErrorKind> {
    const {
        assert!(MAX_CONCURRENCY_CEILING < tokio::sync::Semaphore::MAX_PERMITS);
    }
    if !(1..=MAX_CONCURRENCY_CEILING).contains(&options.max_read_concurrency)
        || !(1..=MAX_CONCURRENCY_CEILING).contains(&options.max_write_concurrency)
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

pub(crate) fn validate_url(
    raw: &str,
    allow_plaintext: bool,
) -> Result<AdmittedUrl, StartErrorKind> {
    validate_url_lexical(raw).map_err(|_| StartErrorKind::InvalidUrl)?;
    let mut parsed = Url::parse(raw).map_err(|_| StartErrorKind::InvalidUrl)?;
    let is_https = match parsed.scheme() {
        "https" => true,
        "http" if allow_plaintext => false,
        "http" => return Err(StartErrorKind::PlaintextRefused),
        _ => return Err(StartErrorKind::InvalidUrl),
    };
    if parsed.cannot_be_a_base()
        || parsed.host_str().is_none_or(str::is_empty)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(StartErrorKind::InvalidUrl);
    }
    let clean_path = trim_path_prefix(parsed.path()).to_owned();
    parsed.set_path(&clean_path);
    let host = parsed
        .host_str()
        .ok_or(StartErrorKind::InvalidUrl)?
        .to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or(StartErrorKind::InvalidUrl)?;
    let normalized = strip_root_trailing_slash(parsed.into(), &clean_path);
    Ok(AdmittedUrl {
        normalized,
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

fn build_clickhouse_client<C>(connector: C) -> clickhouse::Client
where
    C: hyper_util::client::legacy::connect::Connect + Clone + Send + Sync + 'static,
{
    let hyper_client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .pool_timer(TokioTimer::new())
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .http1_max_headers(HTTP1_MAX_HEADERS)
        .build(connector);
    clickhouse::Client::with_http_client(hyper_client)
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
    if trimmed.is_empty() || has_ascii_control(trimmed.as_bytes()) {
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
    if value.trim().is_empty() || has_ascii_control(value.as_bytes()) {
        return Err(err);
    }
    Ok(())
}

fn validate_header_safe_non_empty(value: &str, err: StartErrorKind) -> Result<(), StartErrorKind> {
    if value.is_empty() || has_ascii_control(value.as_bytes()) {
        return Err(err);
    }
    Ok(())
}
