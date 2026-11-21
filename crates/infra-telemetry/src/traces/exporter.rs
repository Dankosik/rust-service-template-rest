use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _};
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
use secrecy::ExposeSecret;
use shared_rules::uri::{authority_has_userinfo, has_whitespace_or_control};
use zeroize::{Zeroize as _, Zeroizing};

use super::{EndpointSource, TracingErrorKind, TracingOptions};

const OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE: &str = "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE";
const OTEL_EXPORTER_OTLP_CLIENT_KEY: &str = "OTEL_EXPORTER_OTLP_CLIENT_KEY";

pub(super) const CERTIFICATE_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE",
    "OTEL_EXPORTER_OTLP_CERTIFICATE",
];
pub(super) const CLIENT_CERTIFICATE_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_CERTIFICATE",
    OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE,
];
pub(super) const CLIENT_KEY_VARS: [&str; 2] = [
    "OTEL_EXPORTER_OTLP_TRACES_CLIENT_KEY",
    OTEL_EXPORTER_OTLP_CLIENT_KEY,
];
const TRUST_FILE_MAX_BYTES: u64 = 1024 * 1024;

fn is_blank(value: &OsStr) -> bool {
    value.to_string_lossy().trim().is_empty()
}

fn non_blank(value: OsString) -> Option<OsString> {
    (!is_blank(&value)).then_some(value)
}

const TYPED_ENDPOINT: &str = "observability.otel.exporter.otlp_endpoint";
const TYPED_HEADERS: &str = "observability.otel.exporter.otlp_headers";

pub(super) struct CollectorConfig {
    pub(super) url: reqwest::Url,
    pub(super) source: EndpointSource,
    pub(super) headers: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EnvScope {
    Signal,
    Common,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UrlOrigin {
    Typed,
    Env(EnvScope),
}

struct EnvSetting {
    scope: EnvScope,
    name: &'static str,
    raw: OsString,
}

impl EnvSetting {
    fn text(&self) -> Result<&str, TracingErrorKind> {
        self.raw
            .to_str()
            .ok_or(TracingErrorKind::NonUnicode { name: self.name })
    }

    fn url(&self) -> Result<reqwest::Url, TracingErrorKind> {
        parse_collector_url(self.text()?, self.name, UrlOrigin::Env(self.scope))
    }

    const fn credential(&self) -> EnvCredential {
        EnvCredential {
            scope: self.scope,
            name: self.name,
        }
    }
}

fn non_blank_str(raw: Option<&str>) -> Option<&str> {
    raw.filter(|value| !value.trim().is_empty())
}

struct StandardEnv {
    signal_endpoint: Option<EnvSetting>,
    common_endpoint: Option<EnvSetting>,
    headers: Option<EnvSetting>,
}

impl StandardEnv {
    fn from_env(env: &impl Fn(&str) -> Option<OsString>) -> Self {
        let setting = |scope: EnvScope, name: &'static str| {
            Some(EnvSetting {
                scope,
                name,
                raw: env(name).and_then(non_blank)?,
            })
        };
        Self {
            signal_endpoint: setting(EnvScope::Signal, OTEL_EXPORTER_OTLP_TRACES_ENDPOINT),
            common_endpoint: setting(EnvScope::Common, OTEL_EXPORTER_OTLP_ENDPOINT),
            headers: [
                (EnvScope::Signal, OTEL_EXPORTER_OTLP_TRACES_HEADERS),
                (EnvScope::Common, OTEL_EXPORTER_OTLP_HEADERS),
            ]
            .into_iter()
            .find_map(|(scope, name)| setting(scope, name)),
        }
    }

    fn endpoint(&self) -> Option<&EnvSetting> {
        self.signal_endpoint
            .as_ref()
            .or(self.common_endpoint.as_ref())
    }
}

#[derive(Clone, Copy)]
struct EnvCredential {
    scope: EnvScope,
    name: &'static str,
}

#[derive(Clone, Copy)]
enum SelectedHeaders<'a> {
    Typed(&'a str),
    Env(&'a EnvSetting),
}

impl SelectedHeaders<'_> {
    const fn name(self) -> &'static str {
        match self {
            Self::Typed(_) => TYPED_HEADERS,
            Self::Env(setting) => setting.name,
        }
    }

    const fn env_credential(self) -> Option<EnvCredential> {
        match self {
            Self::Typed(_) => None,
            Self::Env(setting) => Some(setting.credential()),
        }
    }

    fn parse(self) -> Result<HashMap<String, String>, TracingErrorKind> {
        match self {
            Self::Typed(raw) => parse_headers(raw, TYPED_HEADERS),
            Self::Env(setting) => parse_headers(setting.text()?, setting.name),
        }
    }
}

fn first_credential_name(
    headers: Option<SelectedHeaders<'_>>,
    trust: &CollectorTrust,
) -> Option<&'static str> {
    headers
        .map(SelectedHeaders::name)
        .or_else(|| trust.client_certificate.as_ref().map(|file| file.variable))
        .or_else(|| trust.client_key.as_ref().map(|file| file.variable))
}

impl CollectorConfig {
    pub(super) fn resolve(
        options: &TracingOptions<'_>,
        trust: &CollectorTrust,
        env: &impl Fn(&str) -> Option<OsString>,
    ) -> Result<Option<Self>, TracingErrorKind> {
        let standard = StandardEnv::from_env(env);
        let typed_endpoint = non_blank_str(options.otlp_endpoint);
        let typed_headers = non_blank_str(options.otlp_headers.map(ExposeSecret::expose_secret));
        let (source, url, headers) =
            match (typed_endpoint, typed_headers, standard.headers.as_ref()) {
                (Some(_), _, Some(headers)) => {
                    return Err(TracingErrorKind::AmbientCredential { name: headers.name });
                }
                (None, Some(_), _) => {
                    return Err(TracingErrorKind::UnboundCredential {
                        name: TYPED_HEADERS,
                    });
                }
                (Some(endpoint), headers, None) => (
                    EndpointSource::Typed,
                    parse_collector_url(endpoint, TYPED_ENDPOINT, UrlOrigin::Typed)?,
                    headers.map(SelectedHeaders::Typed),
                ),
                (None, None, headers) => {
                    let headers = headers.map(SelectedHeaders::Env);
                    let Some(endpoint) = standard.endpoint() else {
                        return match first_credential_name(headers, trust) {
                            Some(name) => Err(TracingErrorKind::UnboundCredential { name }),
                            None => Ok(None),
                        };
                    };
                    (EndpointSource::Environment, endpoint.url()?, headers)
                }
            };
        verify_collector_security(&url, source, &standard, trust, headers)?;
        Ok(Some(Self {
            source,
            url,
            headers: headers
                .map(SelectedHeaders::parse)
                .transpose()?
                .unwrap_or_default(),
        }))
    }
}

fn verify_collector_security(
    url: &reqwest::Url,
    source: EndpointSource,
    standard: &StandardEnv,
    trust: &CollectorTrust,
    headers: Option<SelectedHeaders<'_>>,
) -> Result<(), TracingErrorKind> {
    if url.scheme() != "https" {
        return match trust
            .certificate
            .as_ref()
            .map(|file| file.variable)
            .or_else(|| first_credential_name(headers, trust))
        {
            Some(name) => Err(TracingErrorKind::InsecureCollector { name }),
            None => Ok(()),
        };
    }
    for credential in [
        headers.and_then(SelectedHeaders::env_credential),
        trust.client_certificate.as_ref().map(TrustFile::credential),
        trust.client_key.as_ref().map(TrustFile::credential),
    ]
    .into_iter()
    .flatten()
    {
        verify_standard_recipient(credential, standard, source, url)?;
    }
    Ok(())
}

fn verify_standard_recipient(
    credential: EnvCredential,
    standard: &StandardEnv,
    source: EndpointSource,
    destination: &reqwest::Url,
) -> Result<(), TracingErrorKind> {
    let name = credential.name;
    let standard_endpoint = standard
        .endpoint()
        .ok_or(TracingErrorKind::UnboundCredential { name })?;
    if source == EndpointSource::Typed && standard_endpoint.url()? != *destination {
        return Err(TracingErrorKind::UnboundCredential { name });
    }
    if credential.scope == EnvScope::Common
        && standard_endpoint.scope == EnvScope::Signal
        && let Some(common_endpoint) = standard.common_endpoint.as_ref()
        && common_endpoint.url()? != *destination
    {
        return Err(TracingErrorKind::UnboundCredential { name });
    }
    Ok(())
}

fn parse_collector_url(
    raw: &str,
    name: &'static str,
    origin: UrlOrigin,
) -> Result<reqwest::Url, TracingErrorKind> {
    let invalid = || TracingErrorKind::InvalidEndpoint { name };
    let uri = raw.parse::<Uri>().map_err(|_| invalid())?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri
            .authority()
            .is_none_or(|authority| authority.host().is_empty())
        || authority_has_userinfo(raw)
        || raw.contains(['?', '#', '\\'])
        || has_whitespace_or_control(raw)
    {
        return Err(invalid());
    }
    let mut url = reqwest::Url::parse(raw).map_err(|_| invalid())?;
    if origin == UrlOrigin::Env(EnvScope::Common)
        || (origin == UrlOrigin::Typed && url.path() == "/")
    {
        let path = format!("{}/v1/traces", url.path().trim_suffix('/'));
        url.set_path(&path);
    }
    Ok(url)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrustFile {
    pub(super) scope: EnvScope,
    pub(super) variable: &'static str,
    pub(super) path: PathBuf,
}

impl TrustFile {
    const fn credential(&self) -> EnvCredential {
        EnvCredential {
            scope: self.scope,
            name: self.variable,
        }
    }

    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "the blocking exporter preparation task owns TLS file reads"
    )]
    fn read(&self) -> Result<Zeroizing<Vec<u8>>, TracingErrorKind> {
        let name = self.variable;
        let read_error = |error: io::Error| TracingErrorKind::TrustRead {
            name,
            kind: error.kind(),
        };
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
        }
        let file = options.open(&self.path).map_err(read_error)?;
        let metadata = file.metadata().map_err(read_error)?;
        if !metadata.is_file() {
            return Err(TracingErrorKind::TrustFileType { name });
        }
        let max_bytes = usize::try_from(TRUST_FILE_MAX_BYTES).unwrap_or(usize::MAX);
        let capacity = usize::try_from(metadata.len())
            .unwrap_or(max_bytes)
            .min(max_bytes)
            .saturating_add(1);
        let mut pem = Zeroizing::new(Vec::with_capacity(capacity));
        file.take(TRUST_FILE_MAX_BYTES + 1)
            .read_to_end(&mut pem)
            .map_err(read_error)?;
        if pem.len() as u64 > TRUST_FILE_MAX_BYTES {
            return Err(TracingErrorKind::TrustFileTooLarge {
                name,
                max_bytes: TRUST_FILE_MAX_BYTES,
            });
        }
        Ok(pem)
    }

    fn load_certificates(&self) -> Result<Vec<reqwest::Certificate>, TracingErrorKind> {
        let certificates = reqwest::Certificate::from_pem_bundle(&self.read()?).map_err(|_| {
            TracingErrorKind::TrustPem {
                name: self.variable,
            }
        })?;
        if certificates.is_empty() {
            return Err(TracingErrorKind::TrustPem {
                name: self.variable,
            });
        }
        Ok(certificates)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct CollectorTrust {
    pub(super) certificate: Option<TrustFile>,
    pub(super) client_certificate: Option<TrustFile>,
    pub(super) client_key: Option<TrustFile>,
}

impl CollectorTrust {
    pub(super) fn from_env(env: impl Fn(&str) -> Option<OsString>) -> Self {
        let file = |[signal, common]: [&'static str; 2]| {
            [(EnvScope::Signal, signal), (EnvScope::Common, common)]
                .into_iter()
                .find_map(|(scope, variable)| {
                    Some(TrustFile {
                        scope,
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

    fn load_client_identity(&self) -> Result<Option<reqwest::Identity>, TracingErrorKind> {
        match (&self.client_certificate, &self.client_key) {
            (Some(certificate), Some(key)) => {
                let key_pem = key.read()?;
                let pem = if certificate.path.as_os_str() == key.path.as_os_str() {
                    key_pem
                } else {
                    let cert_pem = certificate.read()?;
                    let mut combined =
                        Zeroizing::new(Vec::with_capacity(key_pem.len() + 1 + cert_pem.len()));
                    combined.extend_from_slice(&key_pem);
                    combined.push(b'\n');
                    combined.extend_from_slice(&cert_pem);
                    combined
                };
                reqwest::Identity::from_pem(&pem)
                    .map(Some)
                    .map_err(|_| TracingErrorKind::IdentityPem)
            }
            (Some(present), None) => Err(TracingErrorKind::IncompleteIdentity {
                present: present.variable,
                missing: OTEL_EXPORTER_OTLP_CLIENT_KEY,
            }),
            (None, Some(present)) => Err(TracingErrorKind::IncompleteIdentity {
                present: present.variable,
                missing: OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE,
            }),
            (None, None) => Ok(None),
        }
    }
}

pub(super) fn span_exporter(
    endpoint: &str,
    headers: HashMap<String, String>,
    trust: &CollectorTrust,
    timeout: Duration,
    compression: Option<opentelemetry_otlp::Compression>,
) -> Result<opentelemetry_otlp::SpanExporter, TracingErrorKind> {
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_timeout(timeout)
        .with_endpoint(endpoint)
        .with_headers(headers);
    if let Some(compression) = compression {
        builder = builder.with_compression(compression);
    }
    builder = builder.with_http_client(collector_client(trust, timeout)?);
    builder.build().map_err(|_| TracingErrorKind::ExporterBuild)
}

pub(super) fn compression(
    env: &impl Fn(&str) -> Option<OsString>,
) -> (Option<opentelemetry_otlp::Compression>, Vec<&'static str>) {
    let mut ignored = Vec::new();
    for name in [
        OTEL_EXPORTER_OTLP_TRACES_COMPRESSION,
        OTEL_EXPORTER_OTLP_COMPRESSION,
    ] {
        let Some(value) = env(name).filter(|value| !value.is_empty()) else {
            continue;
        };
        match value.to_str() {
            Some(value) if value.eq_ignore_ascii_case("gzip") => {
                return (Some(opentelemetry_otlp::Compression::Gzip), ignored);
            }
            Some(value) if value.eq_ignore_ascii_case("none") => break,
            _ => ignored.push(name),
        }
    }
    (None, ignored)
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the blocking preparation task constructs the client for the SDK exporter thread"
)]
fn collector_client(
    trust: &CollectorTrust,
    timeout: Duration,
) -> Result<reqwest::blocking::Client, TracingErrorKind> {
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .referer(false);
    if let Some(file) = &trust.certificate {
        builder = builder.tls_certs_only(file.load_certificates()?);
    }
    if let Some(identity) = trust.load_client_identity()? {
        builder = builder.identity(identity);
    }
    builder.build().map_err(|_| TracingErrorKind::ClientBuild)
}

pub(super) fn export_timeout(env: &impl Fn(&str) -> Option<OsString>) -> Duration {
    [
        OTEL_EXPORTER_OTLP_TRACES_TIMEOUT,
        OTEL_EXPORTER_OTLP_TIMEOUT,
    ]
    .into_iter()
    .find_map(|name| env(name)?.to_str()?.parse().ok())
    .map_or(OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT, Duration::from_millis)
}

fn parse_headers(
    raw: &str,
    setting_name: &'static str,
) -> Result<HashMap<String, String>, TracingErrorKind> {
    let mut headers = HashMap::new();
    for (index, entry) in raw.split(',').enumerate() {
        if entry.trim().is_empty() {
            continue;
        }
        let invalid = TracingErrorKind::InvalidHeader {
            name: setting_name,
            position: index + 1,
        };
        let Some((name, value)) = entry.split_once('=') else {
            return Err(invalid);
        };
        let (name, value) = (name.trim(), value.trim());
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            return Err(invalid);
        };
        if name == axum::http::header::HOST
            || HeaderValue::from_str(value).is_err()
            || decoded_otlp_header_value(value)
                .is_some_and(|decoded| HeaderValue::from_str(&decoded).is_err())
        {
            return Err(invalid);
        }
        let key = if name == axum::http::header::USER_AGENT {
            "User-Agent"
        } else {
            name.as_str()
        };
        if headers.insert(key.to_owned(), value.to_owned()).is_some() {
            return Err(invalid);
        }
    }
    Ok(headers)
}

fn decoded_otlp_header_value(value: &str) -> Option<Zeroizing<String>> {
    let (prefix, escapes) = value.split_once('%')?;
    let mut decoded = Zeroizing::new(Vec::with_capacity(value.len()));
    decoded.extend_from_slice(prefix.as_bytes());
    for part in escapes.split('%') {
        let (digits, literal) = part.split_at_checked(2)?;
        decoded.push(u8::from_str_radix(digits, 16).ok()?);
        decoded.extend_from_slice(literal.as_bytes());
    }
    let raw = std::mem::take(&mut *decoded);
    match String::from_utf8(raw) {
        Ok(text) => Some(Zeroizing::new(text)),
        Err(error) => {
            error.into_bytes().zeroize();
            None
        }
    }
}

#[cfg(test)]
mod tests;
