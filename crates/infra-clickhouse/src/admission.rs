use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use secrecy::ExposeSecret as _;
use shared_rules::clickhouse::{
    MAX_CONCURRENCY_CEILING, MAX_MEMORY_USAGE, MAX_RESULT_BYTES, MAX_RESULT_ROWS_CEILING,
    MAX_TIMEOUT, MIN_MEMORY_USAGE, MIN_RESULT_BYTES, MIN_TIMEOUT, strip_root_trailing_slash,
    trim_path_prefix, validate_url_lexical,
};
use shared_rules::rfc9110::has_ascii_control;
use url::Url;
use zeroize::Zeroizing;

use crate::credentials::{read_password_file, read_regular_file, read_secret_file};
use crate::error::StartErrorKind;
use crate::observe::EndpointMetadata;
use crate::{
    ClickHouseAuth, ClickHouseOptions, ClientCertificate, QuerySettings, RuntimePolicy,
    TransportSecurity,
};

const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(5);
const KEEPALIVE_TIME: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_RETRIES: u32 = 3;
#[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP1_MAX_HEADERS: usize = 100;

const MAX_PEM_CHAIN_BYTES: usize = 1024 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 64 * 1024;

pub(crate) struct Admitted {
    pub(crate) client: clickhouse::Client,
    pub(crate) endpoint: EndpointMetadata,
    pub(crate) password: Option<Zeroizing<String>>,
    pub(crate) password_file: Option<PathBuf>,
    pub(crate) max_read_concurrency: NonZeroUsize,
    pub(crate) max_write_concurrency: NonZeroUsize,
    pub(crate) policy: RuntimePolicy,
}

pub(crate) struct AdmittedUrl {
    pub(crate) normalized: String,
    pub(crate) host: String,
    pub(crate) port: u16,
}

struct AdmittedCredentials {
    username: Option<String>,
    password: Option<Zeroizing<String>>,
    password_file: Option<PathBuf>,
}

enum AdmittedTransport {
    Tls(Box<rustls::ClientConfig>),
    Plaintext,
}

struct ClientIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

pub(crate) fn admit(options: ClickHouseOptions) -> Result<Admitted, StartErrorKind> {
    validate_limits(&options)?;
    let url = validate_url(&options.url, &options.transport)?;
    validate_printable_non_blank(&options.database, StartErrorKind::InvalidDatabase)?;
    let creds = admit_credentials(options.auth)?;
    let transport = admit_transport(options.transport)?;

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

    let mut client = match transport {
        AdmittedTransport::Tls(tls_config) => {
            http.enforce_http(false);
            let connector = hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(*tls_config)
                .https_only()
                .enable_http1()
                .wrap_connector(http);
            build_clickhouse_client(connector)
        }
        AdmittedTransport::Plaintext => {
            http.enforce_http(true);
            build_clickhouse_client(http)
        }
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
            database: options.database,
            host: url.host,
            port: url.port,
        },
        password: creds.password,
        password_file: creds.password_file,
        max_read_concurrency: options.max_read_concurrency,
        max_write_concurrency: options.max_write_concurrency,
        policy: RuntimePolicy {
            query_settings: options.query_settings,
            query_timeout: options.query_timeout,
            insert_timeout: options.insert_timeout,
            max_result_rows: options.max_result_rows,
            max_result_bytes: options.max_result_bytes,
            http_response_buffer_size: options.http_response_buffer_size,
            propagate_trace_context: options.propagate_trace_context,
        },
    })
}

fn validate_limits(options: &ClickHouseOptions) -> Result<(), StartErrorKind> {
    const {
        assert!(MAX_CONCURRENCY_CEILING < tokio::sync::Semaphore::MAX_PERMITS);
    }
    if options.max_read_concurrency.get() > MAX_CONCURRENCY_CEILING
        || options.max_write_concurrency.get() > MAX_CONCURRENCY_CEILING
    {
        return Err(StartErrorKind::InvalidConcurrency);
    }
    let max_result_bytes = options.max_result_bytes.get();
    if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&options.query_timeout)
        || !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&options.insert_timeout)
        || options.max_result_rows.get() > MAX_RESULT_ROWS_CEILING
        || !(MIN_RESULT_BYTES..=MAX_RESULT_BYTES).contains(&max_result_bytes)
        || !(MIN_RESULT_BYTES..=max_result_bytes).contains(&options.http_response_buffer_size.get())
    {
        return Err(StartErrorKind::InvalidLimits);
    }
    if let QuerySettings::Client {
        max_memory_usage: Some(mem),
    } = options.query_settings
        && !(MIN_MEMORY_USAGE..=MAX_MEMORY_USAGE).contains(&mem.get())
    {
        return Err(StartErrorKind::InvalidLimits);
    }
    Ok(())
}

pub(crate) fn validate_url(
    raw: &str,
    transport: &TransportSecurity,
) -> Result<AdmittedUrl, StartErrorKind> {
    validate_url_lexical(raw).map_err(|_| StartErrorKind::InvalidUrl)?;
    let mut parsed = Url::parse(raw).map_err(|_| StartErrorKind::InvalidUrl)?;
    match (parsed.scheme(), transport) {
        ("https", TransportSecurity::Tls { .. }) | ("http", TransportSecurity::Plaintext) => {}
        ("http", TransportSecurity::Tls { .. }) => return Err(StartErrorKind::PlaintextRefused),
        ("https", TransportSecurity::Plaintext) => {
            return Err(StartErrorKind::PlaintextUrlRequired);
        }
        _ => return Err(StartErrorKind::InvalidUrl),
    }
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
        host,
        port,
    })
}

fn admit_credentials(auth: ClickHouseAuth) -> Result<AdmittedCredentials, StartErrorKind> {
    let username = match &auth {
        ClickHouseAuth::Password { username, .. }
        | ClickHouseAuth::PasswordFile { username, .. } => Some(username.as_str()),
        ClickHouseAuth::Unauthenticated { username } => username.as_deref(),
    };
    if let Some(username) = username {
        validate_header_safe_non_empty(username, StartErrorKind::InvalidCredentials)?;
    }
    match auth {
        ClickHouseAuth::Password { username, password } => {
            let exposed = password.expose_secret();
            validate_printable_non_blank(exposed, StartErrorKind::InvalidCredentials)?;
            Ok(AdmittedCredentials {
                username: Some(username),
                password: Some(Zeroizing::new(exposed.to_owned())),
                password_file: None,
            })
        }
        ClickHouseAuth::PasswordFile { username, path } => {
            let password = read_password_file(&path)?;
            Ok(AdmittedCredentials {
                username: Some(username),
                password: Some(password),
                password_file: Some(path),
            })
        }
        ClickHouseAuth::Unauthenticated { username } => Ok(AdmittedCredentials {
            username,
            password: None,
            password_file: None,
        }),
    }
}

fn admit_transport(transport: TransportSecurity) -> Result<AdmittedTransport, StartErrorKind> {
    let TransportSecurity::Tls {
        root_ca_path,
        client_certificate,
    } = transport
    else {
        return Ok(AdmittedTransport::Plaintext);
    };
    let roots = root_ca_path.as_deref().map(read_root_ca).transpose()?;
    let identity = client_certificate
        .as_ref()
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
    Ok(AdmittedTransport::Tls(Box::new(config)))
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
    let bytes = read_regular_file(path, MAX_PEM_CHAIN_BYTES)
        .map_err(|error| StartErrorKind::CaFile { kind: error.kind() })?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in pem_certificates(&bytes).ok_or(StartErrorKind::InvalidCa)? {
        roots.add(cert).map_err(|_| StartErrorKind::InvalidCa)?;
    }
    Ok(roots)
}

fn read_client_certificate(cert: &ClientCertificate) -> Result<ClientIdentity, StartErrorKind> {
    use rustls::pki_types::pem::PemObject as _;

    let cert_bytes = read_regular_file(&cert.cert_path, MAX_PEM_CHAIN_BYTES)
        .map_err(|error| StartErrorKind::ClientCertificateFile { kind: error.kind() })?;
    let key_bytes = read_secret_file(&cert.key_path, MAX_PRIVATE_KEY_BYTES)
        .map_err(|error| StartErrorKind::ClientKeyFile { kind: error.kind() })?;
    let chain = pem_certificates(&cert_bytes).ok_or(StartErrorKind::InvalidClientCertificate)?;
    let key =
        PrivateKeyDer::from_pem_slice(&key_bytes).map_err(|_| StartErrorKind::InvalidClientKey)?;
    Ok(ClientIdentity { chain, key })
}

fn pem_certificates(bytes: &[u8]) -> Option<Vec<CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject as _;

    CertificateDer::pem_slice_iter(bytes)
        .collect::<Result<Vec<_>, _>>()
        .ok()
        .filter(|certs| !certs.is_empty())
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
