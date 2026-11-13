//! The OTLP span exporter, built from typed configuration and the standard
//! `OTEL_EXPORTER_OTLP_*` variables.
//!
//! The typed endpoint wins over the standard endpoint variables, which the
//! SDK reads itself. This template always builds OTLP/HTTP protobuf and does
//! not honor `OTEL_EXPORTER_OTLP_PROTOCOL`.
//! When the typed endpoint selects the collector, ambient header variables
//! are refused because the SDK would merge them into the typed request, so
//! one collector's credential is never sent to another.
//! `OTEL_EXPORTER_OTLP_COMPRESSION` and its traces variant select `gzip`
//! or `none`, and any other value degrades the exporter; the default is
//! uncompressed, as the specification has it.
//! The collector is verified with the platform trust store unless
//! `OTEL_EXPORTER_OTLP_CERTIFICATE` names a PEM file of trusted
//! certificates, which then are the only ones trusted;
//! `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` with
//! `OTEL_EXPORTER_OTLP_CLIENT_KEY` presents a client certificate. Each has
//! a traces variant that wins. The SDK's OTLP/HTTP exporter reads none of
//! them, so this module builds the exporter's HTTP client when one is set.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue, Uri};
use opentelemetry_otlp::{
    OTEL_EXPORTER_OTLP_COMPRESSION, OTEL_EXPORTER_OTLP_ENDPOINT, OTEL_EXPORTER_OTLP_HEADERS,
    OTEL_EXPORTER_OTLP_TIMEOUT, OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT,
    OTEL_EXPORTER_OTLP_TRACES_COMPRESSION, OTEL_EXPORTER_OTLP_TRACES_ENDPOINT,
    OTEL_EXPORTER_OTLP_TRACES_HEADERS, OTEL_EXPORTER_OTLP_TRACES_TIMEOUT, WithExportConfig,
    WithHttpConfig,
};

use super::{EndpointSource, TracingError, TracingOptions};

/// Standard trust variables, each a path to a PEM file; the traces variant
/// wins.
pub(super) const CERTIFICATE_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_CERTIFICATE",
];
pub(super) const CLIENT_CERTIFICATE_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
];
pub(super) const CLIENT_KEY_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY",
    "OTEL_EXPORTER_OTLP_CLIENT_KEY",
];

fn is_blank(value: &OsStr) -> bool {
    value.to_string_lossy().trim().is_empty()
}

fn non_blank(value: OsString) -> Option<OsString> {
    (!is_blank(&value)).then_some(value)
}

/// Where spans are exported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Endpoint<'a> {
    /// The typed URL.
    Typed(&'a str),
    /// The SDK reads the standard endpoint variables itself.
    Environment,
    /// The `OTEL_EXPORTER_OTLP_ENDPOINT` root, given to the SDK as its own
    /// traces URL, because the SDK would read the blank traces variable
    /// first and fail to parse it.
    EnvironmentRoot(String),
}

impl Endpoint<'_> {
    pub(super) const fn source(&self) -> EndpointSource {
        match self {
            Self::Typed(_) => EndpointSource::Typed,
            Self::Environment | Self::EnvironmentRoot(_) => EndpointSource::Environment,
        }
    }
}

/// A blank variable is vacant. The SDK falls back to `localhost:4318` for
/// an empty one and fails on a blank one, so the decision is made here.
pub(super) fn resolve_endpoint(
    options: &TracingOptions,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<Endpoint<'_>>, TracingError> {
    if let Some(endpoint) = options
        .otlp_endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.trim().is_empty())
    {
        // The SDK reads the traces header variable whenever it holds
        // Unicode, even empty, and the general one only otherwise.
        let name = if env(OTEL_EXPORTER_OTLP_TRACES_HEADERS)
            .is_some_and(|value| value.to_str().is_some())
        {
            OTEL_EXPORTER_OTLP_TRACES_HEADERS
        } else {
            OTEL_EXPORTER_OTLP_HEADERS
        };
        if env(name).as_deref().is_some_and(|value| !is_blank(value)) {
            return Err(TracingError::AmbientCredential { name });
        }
        return Ok(Some(Endpoint::Typed(endpoint)));
    }
    let traces = env(OTEL_EXPORTER_OTLP_TRACES_ENDPOINT);
    if traces.as_deref().is_some_and(|value| !is_blank(value)) {
        return Ok(Some(Endpoint::Environment));
    }
    let Some(root) = env(OTEL_EXPORTER_OTLP_ENDPOINT).and_then(non_blank) else {
        return Ok(None);
    };
    if traces.is_none_or(|value| value.is_empty()) {
        return Ok(Some(Endpoint::Environment));
    }
    Ok(Some(Endpoint::EnvironmentRoot(
        root.to_string_lossy().into_owned(),
    )))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrustFile {
    pub(super) variable: &'static str,
    pub(super) path: PathBuf,
}

impl TrustFile {
    #[expect(
        clippy::disallowed_methods,
        reason = "startup exporter client construction owns TLS file reads on its scoped thread"
    )]
    fn read(&self) -> Result<Vec<u8>, ExporterError> {
        std::fs::read(&self.path).map_err(|source| ExporterError::Read {
            variable: self.variable,
            source,
        })
    }

    fn unusable(&self, source: reqwest::Error) -> ExporterError {
        ExporterError::Pem {
            variable: self.variable,
            source,
        }
    }
}

/// The collector's trust material from the standard variables. All vacant
/// is the platform trust store and no client certificate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CollectorTrust {
    pub(super) certificate: Option<TrustFile>,
    pub(super) client_certificate: Option<TrustFile>,
    pub(super) client_key: Option<TrustFile>,
}

impl CollectorTrust {
    pub(super) fn from_env(env: impl Fn(&str) -> Option<OsString>) -> Self {
        let file = |variables: [&'static str; 2]| {
            variables.into_iter().find_map(|variable| {
                Some(TrustFile {
                    variable,
                    path: env(variable).and_then(non_blank)?.into(),
                })
            })
        };
        Self {
            certificate: file(CERTIFICATE_VARS),
            client_certificate: file(CLIENT_CERTIFICATE_VARS),
            client_key: file(CLIENT_KEY_VARS),
        }
    }
}

/// Why an exporter could not be built. Any of these degrades to
/// [`ExporterState::Degraded`](super::ExporterState::Degraded) with the fixed reason `exporter_build`: the
/// text stays out of logs by the configuration source policy, and is read
/// only by tests. Names the variable, never file content.
#[derive(Debug, thiserror::Error)]
pub(super) enum ExporterError {
    #[error(transparent)]
    Build(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("read the file {variable} names: {source}")]
    Read {
        variable: &'static str,
        source: std::io::Error,
    },
    #[error("the file {variable} names is not usable PEM: {source}")]
    Pem {
        variable: &'static str,
        source: reqwest::Error,
    },
    #[error("the file {variable} names holds no certificate")]
    NoCertificate { variable: &'static str },
    #[error("the client certificate and key files are not a usable PEM identity: {0}")]
    Identity(#[source] reqwest::Error),
    #[error("{present} is set without {missing}; a client certificate needs both")]
    IncompleteIdentity {
        present: &'static str,
        missing: &'static str,
    },
    #[error("build the collector HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("start the thread that builds the collector HTTP client: {0}")]
    ClientThread(#[source] std::io::Error),
    #[error("the thread that builds the collector HTTP client panicked")]
    ClientThreadPanicked,
    #[error("{variable} is neither `gzip` nor `none`")]
    Compression { variable: &'static str },
}

pub(super) fn span_exporter(
    endpoint: &Endpoint<'_>,
    headers: HashMap<String, String>,
    trust: &CollectorTrust,
    timeout: Duration,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<opentelemetry_otlp::SpanExporter, ExporterError> {
    // Always OTLP/HTTP protobuf. An environment endpoint still lets the SDK
    // pick the URL; it does not select gRPC or another protocol.
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_timeout(timeout);
    match endpoint {
        Endpoint::Typed(endpoint) => builder = builder.with_endpoint(traces_url(endpoint)),
        Endpoint::EnvironmentRoot(root) => builder = builder.with_endpoint(root_traces_url(root)),
        Endpoint::Environment => {}
    }
    if !headers.is_empty() {
        builder = builder.with_headers(headers);
    }
    if let Some(compression) = compression(env)? {
        builder = builder.with_compression(compression);
    }
    if let Some(client) = collector_client(trust, timeout)? {
        builder = builder.with_http_client(client);
    }
    Ok(builder.build()?)
}

/// The compression the standard variables select, read as the SDK reads
/// them: the traces variable, then the general one, an empty value vacant.
/// The SDK skips a value it cannot apply with a warning nobody sees, so such
/// a value fails the build here instead.
fn compression(
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<Option<opentelemetry_otlp::Compression>, ExporterError> {
    let Some((variable, value)) = [
        OTEL_EXPORTER_OTLP_TRACES_COMPRESSION,
        OTEL_EXPORTER_OTLP_COMPRESSION,
    ]
    .into_iter()
    .find_map(|variable| Some((variable, env(variable).filter(|value| !value.is_empty())?))) else {
        return Ok(None);
    };
    match value.to_str() {
        Some(value) if value.eq_ignore_ascii_case("gzip") => {
            Ok(Some(opentelemetry_otlp::Compression::Gzip))
        }
        Some(value) if value.eq_ignore_ascii_case("none") => Ok(None),
        _ => Err(ExporterError::Compression { variable }),
    }
}

/// The exporter's HTTP client when a trust variable is set; `None` leaves
/// the SDK's own client, which verifies with the platform trust store.
#[expect(
    clippy::disallowed_types,
    reason = "startup hands the blocking HTTP client to the SDK-owned exporter thread"
)]
fn collector_client(
    trust: &CollectorTrust,
    timeout: Duration,
) -> Result<Option<reqwest::blocking::Client>, ExporterError> {
    if *trust == CollectorTrust::default() {
        return Ok(None);
    }
    // The blocking client starts its own runtime thread and waits for it,
    // which an async worker must not do; the SDK builds its client the same
    // way.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .spawn_scoped(scope, || trusting_client(trust, timeout))
            .map_err(ExporterError::ClientThread)?
            .join()
            .map_err(|_| ExporterError::ClientThreadPanicked)?
    })
    .map(Some)
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "startup scoped thread constructs the exporter client; request execution stays on the SDK worker"
)]
fn trusting_client(
    trust: &CollectorTrust,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, ExporterError> {
    // The SDK sets its timeout on the client it builds, so a supplied
    // client carries the same one.
    let mut builder = reqwest::blocking::Client::builder().timeout(timeout);
    if let Some(file) = &trust.certificate {
        let certificates = reqwest::Certificate::from_pem_bundle(&file.read()?)
            .map_err(|source| file.unusable(source))?;
        if certificates.is_empty() {
            return Err(ExporterError::NoCertificate {
                variable: file.variable,
            });
        }
        builder = builder.tls_certs_only(certificates);
    }
    match (&trust.client_certificate, &trust.client_key) {
        (Some(certificate), Some(key)) => {
            let mut pem = key.read()?;
            pem.push(b'\n');
            pem.extend(certificate.read()?);
            let identity = reqwest::Identity::from_pem(&pem).map_err(ExporterError::Identity)?;
            builder = builder.identity(identity);
        }
        (Some(present), None) => {
            return Err(ExporterError::IncompleteIdentity {
                present: present.variable,
                missing: CLIENT_KEY_VARS[1],
            });
        }
        (None, Some(present)) => {
            return Err(ExporterError::IncompleteIdentity {
                present: present.variable,
                missing: CLIENT_CERTIFICATE_VARS[1],
            });
        }
        (None, None) => {}
    }
    builder.build().map_err(ExporterError::Client)
}

/// The export timeout as the SDK resolves it from the environment: the
/// traces variable, then the general one, in milliseconds, then ten seconds.
/// Set on the exporter as well, so the SDK's retry deadline, a supplied
/// client, and the export classification share this one value.
pub(super) fn export_timeout(env: &impl Fn(&str) -> Option<OsString>) -> Duration {
    [
        OTEL_EXPORTER_OTLP_TRACES_TIMEOUT,
        OTEL_EXPORTER_OTLP_TIMEOUT,
    ]
    .into_iter()
    .find_map(|name| env(name)?.to_str()?.parse().ok())
    .map_or(OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT, Duration::from_millis)
}

/// The SDK uses a programmatic endpoint verbatim. Apply the
/// `OTEL_EXPORTER_OTLP_ENDPOINT` rule to a collector root so
/// `http://collector:4318` posts to `/v1/traces`, not `/`.
fn traces_url(endpoint: &str) -> String {
    match endpoint.parse::<Uri>() {
        Ok(uri) if uri.path() == "/" && uri.query().is_none() => {
            format!("{}/v1/traces", endpoint.trim_end_matches('/'))
        }
        _ => endpoint.to_owned(),
    }
}

/// The SDK's rule for `OTEL_EXPORTER_OTLP_ENDPOINT`: `/v1/traces` appended
/// to the root as written.
fn root_traces_url(root: &str) -> String {
    format!("{}/v1/traces", root.trim_suffix('/'))
}

/// `key=value,key=value` into a map. Blank entries are skipped; any other
/// entry must be a valid header name and value, so a typo fails startup
/// instead of silently dropping a credential. The SDK percent-decodes
/// values. Errors name the entry position, never its content.
pub(super) fn parse_headers(raw: &str) -> Result<HashMap<String, String>, TracingError> {
    let mut headers = HashMap::new();
    for (index, entry) in raw.split(',').enumerate() {
        if entry.trim().is_empty() {
            continue;
        }
        let invalid = TracingError::InvalidHeader {
            position: index + 1,
        };
        let Some((name, value)) = entry.split_once('=') else {
            return Err(invalid);
        };
        let (name, value) = (name.trim(), value.trim());
        if HeaderName::from_bytes(name.as_bytes()).is_err() || HeaderValue::from_str(value).is_err()
        {
            return Err(invalid);
        }
        headers.insert(name.to_owned(), value.to_owned());
    }
    Ok(headers)
}

#[cfg(test)]
mod tests;
