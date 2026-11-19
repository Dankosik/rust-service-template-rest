use std::borrow::Cow;
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

use super::{EndpointSource, TracingError, TracingOptions};

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
pub(super) enum SettingScope {
    Typed,
    Signal,
    Common,
}

struct Setting<'a> {
    scope: SettingScope,
    name: &'static str,
    raw: Cow<'a, OsStr>,
}

impl Setting<'_> {
    fn text(&self) -> Result<&str, TracingError> {
        self.raw
            .to_str()
            .ok_or(TracingError::NonUnicode { name: self.name })
    }
}

fn typed_setting<'a>(raw: Option<&'a str>, name: &'static str) -> Option<Setting<'a>> {
    raw.filter(|value| !value.trim().is_empty())
        .map(|raw| Setting {
            scope: SettingScope::Typed,
            name,
            raw: Cow::Borrowed(OsStr::new(raw)),
        })
}

struct StandardEnv {
    signal_endpoint: Option<Setting<'static>>,
    common_endpoint: Option<Setting<'static>>,
    headers: Option<Setting<'static>>,
}

impl StandardEnv {
    fn from_env(env: &impl Fn(&str) -> Option<OsString>) -> Self {
        let setting = |scope: SettingScope, name: &'static str| {
            Some(Setting {
                scope,
                name,
                raw: Cow::Owned(env(name).and_then(non_blank)?),
            })
        };
        Self {
            signal_endpoint: setting(SettingScope::Signal, OTEL_EXPORTER_OTLP_TRACES_ENDPOINT),
            common_endpoint: setting(SettingScope::Common, OTEL_EXPORTER_OTLP_ENDPOINT),
            headers: [
                (SettingScope::Signal, OTEL_EXPORTER_OTLP_TRACES_HEADERS),
                (SettingScope::Common, OTEL_EXPORTER_OTLP_HEADERS),
            ]
            .into_iter()
            .find_map(|(scope, name)| setting(scope, name)),
        }
    }

    fn endpoint(&self) -> Option<&Setting<'static>> {
        self.signal_endpoint
            .as_ref()
            .or(self.common_endpoint.as_ref())
    }
}

#[derive(Clone, Copy)]
struct Credential {
    scope: SettingScope,
    name: &'static str,
}

impl CollectorConfig {
    pub(super) fn resolve(
        options: &TracingOptions,
        trust: &CollectorTrust,
        env: &impl Fn(&str) -> Option<OsString>,
    ) -> Result<Option<Self>, TracingError> {
        let standard = StandardEnv::from_env(env);
        let typed_endpoint = typed_setting(options.otlp_endpoint.as_deref(), TYPED_ENDPOINT);
        let typed_headers = typed_setting(
            options
                .otlp_headers
                .as_ref()
                .map(ExposeSecret::expose_secret),
            TYPED_HEADERS,
        );
        let (endpoint, source, headers) = match (
            typed_endpoint.as_ref(),
            typed_headers.as_ref(),
            standard.headers.as_ref(),
        ) {
            (Some(_), _, Some(headers)) => {
                return Err(TracingError::AmbientCredential { name: headers.name });
            }
            (None, Some(_), _) => {
                return Err(TracingError::UnboundCredential {
                    name: TYPED_HEADERS,
                });
            }
            (Some(endpoint), headers, None) => (Some(endpoint), EndpointSource::Typed, headers),
            (None, None, headers) => (standard.endpoint(), EndpointSource::Environment, headers),
        };
        let credentials = [
            headers.map(|header| Credential {
                scope: header.scope,
                name: header.name,
            }),
            trust.client_certificate.as_ref().map(TrustFile::credential),
            trust.client_key.as_ref().map(TrustFile::credential),
        ];
        let Some(endpoint) = endpoint else {
            return match credentials.into_iter().flatten().next() {
                Some(credential) => Err(TracingError::UnboundCredential {
                    name: credential.name,
                }),
                None => Ok(None),
            };
        };
        let url = collector_url(endpoint)?;
        verify_collector_security(&url, endpoint.scope, &standard, trust, credentials)?;
        Ok(Some(Self {
            source,
            url,
            headers: headers.map(parse_headers).transpose()?.unwrap_or_default(),
        }))
    }
}

fn verify_collector_security(
    url: &reqwest::Url,
    selected_scope: SettingScope,
    standard: &StandardEnv,
    trust: &CollectorTrust,
    credentials: [Option<Credential>; 3],
) -> Result<(), TracingError> {
    if url.scheme() != "https" {
        return match trust
            .certificate
            .as_ref()
            .map(|file| file.variable)
            .or_else(|| {
                credentials
                    .into_iter()
                    .flatten()
                    .next()
                    .map(|credential| credential.name)
            }) {
            Some(name) => Err(TracingError::InsecureCollector { name }),
            None => Ok(()),
        };
    }
    for credential in credentials
        .into_iter()
        .flatten()
        .filter(|credential| credential.scope != SettingScope::Typed)
    {
        verify_standard_recipient(credential, standard, selected_scope, url)?;
    }
    Ok(())
}

fn verify_standard_recipient(
    credential: Credential,
    standard: &StandardEnv,
    selected_scope: SettingScope,
    destination: &reqwest::Url,
) -> Result<(), TracingError> {
    let name = credential.name;
    let standard_endpoint = standard
        .endpoint()
        .ok_or(TracingError::UnboundCredential { name })?;
    if standard_endpoint.scope != selected_scope
        && collector_url(standard_endpoint)? != *destination
    {
        return Err(TracingError::UnboundCredential { name });
    }
    if credential.scope == SettingScope::Common
        && let Some(common_endpoint) = standard
            .common_endpoint
            .as_ref()
            .filter(|common| common.scope != standard_endpoint.scope)
        && collector_url(common_endpoint)? != *destination
    {
        return Err(TracingError::UnboundCredential { name });
    }
    Ok(())
}

fn collector_url(setting: &Setting<'_>) -> Result<reqwest::Url, TracingError> {
    let name = setting.name;
    let raw = setting.text()?;
    let invalid = || TracingError::InvalidEndpoint { name };
    let uri = raw.parse::<Uri>().map_err(|_| invalid())?;
    if !matches!(uri.scheme_str(), Some("http" | "https"))
        || uri
            .authority()
            .is_none_or(|authority| authority.host().is_empty() || authority.as_str().contains('@'))
        || raw.contains(['?', '#', '\\'])
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(invalid());
    }
    let mut url = reqwest::Url::parse(raw).map_err(|_| invalid())?;
    if setting.scope == SettingScope::Common
        || (setting.scope == SettingScope::Typed && url.path() == "/")
    {
        let path = format!("{}/v1/traces", url.path().trim_suffix('/'));
        url.set_path(&path);
    }
    Ok(url)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrustFile {
    pub(super) scope: SettingScope,
    pub(super) variable: &'static str,
    pub(super) path: PathBuf,
}

impl TrustFile {
    fn credential(&self) -> Credential {
        Credential {
            scope: self.scope,
            name: self.variable,
        }
    }

    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "the blocking exporter preparation task owns TLS file reads"
    )]
    fn read(&self) -> Result<Vec<u8>, TracingError> {
        let name = self.variable;
        let read_error = |error: io::Error| TracingError::TrustRead {
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
        if !file.metadata().map_err(read_error)?.is_file() {
            return Err(TracingError::TrustFileType { name });
        }
        let mut pem = Vec::new();
        file.take(TRUST_FILE_MAX_BYTES + 1)
            .read_to_end(&mut pem)
            .map_err(read_error)?;
        if pem.len() as u64 > TRUST_FILE_MAX_BYTES {
            return Err(TracingError::TrustFileTooLarge {
                name,
                max_bytes: TRUST_FILE_MAX_BYTES,
            });
        }
        Ok(pem)
    }

    fn load_certificates(&self) -> Result<Vec<reqwest::Certificate>, TracingError> {
        let certificates = reqwest::Certificate::from_pem_bundle(&self.read()?).map_err(|_| {
            TracingError::TrustPem {
                name: self.variable,
            }
        })?;
        if certificates.is_empty() {
            return Err(TracingError::TrustPem {
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
            [
                (SettingScope::Signal, signal),
                (SettingScope::Common, common),
            ]
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

    fn load_client_identity(&self) -> Result<Option<reqwest::Identity>, TracingError> {
        match (&self.client_certificate, &self.client_key) {
            (Some(certificate), Some(key)) => {
                let mut pem = key.read()?;
                if certificate.path.as_os_str() != key.path.as_os_str() {
                    pem.push(b'\n');
                    pem.extend(certificate.read()?);
                }
                reqwest::Identity::from_pem(&pem)
                    .map(Some)
                    .map_err(|_| TracingError::IdentityPem)
            }
            (Some(present), None) => Err(TracingError::IncompleteIdentity {
                present: present.variable,
                missing: OTEL_EXPORTER_OTLP_CLIENT_KEY,
            }),
            (None, Some(present)) => Err(TracingError::IncompleteIdentity {
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
) -> Result<opentelemetry_otlp::SpanExporter, TracingError> {
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
    builder.build().map_err(|_| TracingError::ExporterBuild)
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
) -> Result<reqwest::blocking::Client, TracingError> {
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
    builder.build().map_err(|_| TracingError::ClientBuild)
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

fn parse_headers(setting: &Setting<'_>) -> Result<HashMap<String, String>, TracingError> {
    let raw = setting.text()?;
    let mut headers = HashMap::new();
    for (index, entry) in raw.split(',').enumerate() {
        if entry.trim().is_empty() {
            continue;
        }
        let invalid = TracingError::InvalidHeader {
            name: setting.name,
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
                .is_some_and(|decoded| HeaderValue::try_from(decoded).is_err())
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

fn decoded_otlp_header_value(value: &str) -> Option<String> {
    let (prefix, escapes) = value.split_once('%')?;
    let mut decoded = Vec::with_capacity(value.len());
    decoded.extend_from_slice(prefix.as_bytes());
    for part in escapes.split('%') {
        let (digits, literal) = part.split_at_checked(2)?;
        decoded.push(u8::from_str_radix(digits, 16).ok()?);
        decoded.extend_from_slice(literal.as_bytes());
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests;
