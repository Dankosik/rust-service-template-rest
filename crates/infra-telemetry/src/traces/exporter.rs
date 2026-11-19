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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingScope {
    Typed,
    Signal,
    Common,
}

#[derive(Clone, Copy)]
struct Setting<'a> {
    scope: SettingScope,
    name: &'static str,
    raw: &'a OsStr,
}

impl<'a> Setting<'a> {
    fn borrowed((scope, name, raw): &'a (SettingScope, &'static str, OsString)) -> Self {
        Self {
            scope: *scope,
            name,
            raw,
        }
    }

    fn text(self) -> Result<&'a str, TracingError> {
        self.raw
            .to_str()
            .ok_or(TracingError::NonUnicode { name: self.name })
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
        let setting = |scope: SettingScope, name: &'static str| {
            Some((scope, name, env(name).and_then(non_blank)?))
        };
        let signal = setting(SettingScope::Signal, OTEL_EXPORTER_OTLP_TRACES_ENDPOINT);
        let common = setting(SettingScope::Common, OTEL_EXPORTER_OTLP_ENDPOINT);
        let signal = signal.as_ref().map(Setting::borrowed);
        let common = common.as_ref().map(Setting::borrowed);
        let standard_headers = [
            (SettingScope::Signal, OTEL_EXPORTER_OTLP_TRACES_HEADERS),
            (SettingScope::Common, OTEL_EXPORTER_OTLP_HEADERS),
        ]
        .into_iter()
        .find_map(|(scope, name)| setting(scope, name));
        let standard_headers = standard_headers.as_ref().map(Setting::borrowed);
        let typed = options
            .otlp_endpoint
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(|raw| Setting {
                scope: SettingScope::Typed,
                name: TYPED_ENDPOINT,
                raw: OsStr::new(raw),
            });
        let typed_headers = options
            .otlp_headers
            .as_ref()
            .map(ExposeSecret::expose_secret)
            .filter(|value| !value.trim().is_empty())
            .map(|raw| Setting {
                scope: SettingScope::Typed,
                name: TYPED_HEADERS,
                raw: OsStr::new(raw),
            });
        if typed.is_some()
            && let Some(Setting { name, .. }) = standard_headers
        {
            return Err(TracingError::AmbientCredential { name });
        }
        if typed_headers.is_some() && typed.is_none() {
            return Err(TracingError::UnboundCredential {
                name: TYPED_HEADERS,
            });
        }
        let headers = typed_headers.or(standard_headers);
        let mut credentials = headers
            .map(|header| Credential {
                scope: header.scope,
                name: header.name,
            })
            .into_iter()
            .chain(
                [trust.client_certificate.as_ref(), trust.client_key.as_ref()]
                    .into_iter()
                    .flatten()
                    .map(TrustFile::credential),
            );
        let Some(endpoint) = typed.or(signal).or(common) else {
            return match credentials.next() {
                Some(credential) => Err(TracingError::UnboundCredential {
                    name: credential.name,
                }),
                None => Ok(None),
            };
        };
        let url = collector_url(endpoint)?;
        if let Some(file) = &trust.certificate
            && url.scheme() != "https"
        {
            return Err(TracingError::InsecureCollector {
                name: file.variable,
            });
        }
        for credential in credentials {
            if url.scheme() != "https" {
                return Err(TracingError::InsecureCollector {
                    name: credential.name,
                });
            }
            if credential.scope != SettingScope::Typed {
                verify_standard_recipient(
                    credential,
                    signal.or(common),
                    common,
                    endpoint.scope,
                    &url,
                )?;
            }
        }
        Ok(Some(Self {
            source: if endpoint.scope == SettingScope::Typed {
                EndpointSource::Typed
            } else {
                EndpointSource::Environment
            },
            url,
            headers: headers.map(parse_headers).transpose()?.unwrap_or_default(),
        }))
    }
}

fn verify_standard_recipient(
    credential: Credential,
    standard_endpoint: Option<Setting<'_>>,
    common_endpoint: Option<Setting<'_>>,
    selected_scope: SettingScope,
    destination: &reqwest::Url,
) -> Result<(), TracingError> {
    let name = credential.name;
    let standard_endpoint = standard_endpoint.ok_or(TracingError::UnboundCredential { name })?;
    if standard_endpoint.scope != selected_scope
        && collector_url(standard_endpoint)? != *destination
    {
        return Err(TracingError::UnboundCredential { name });
    }
    if credential.scope == SettingScope::Common
        && let Some(common_endpoint) =
            common_endpoint.filter(|common| common.scope != standard_endpoint.scope)
        && collector_url(common_endpoint)? != *destination
    {
        return Err(TracingError::UnboundCredential { name });
    }
    Ok(())
}

fn collector_url(setting: Setting<'_>) -> Result<reqwest::Url, TracingError> {
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
    pub(super) variable: &'static str,
    pub(super) path: PathBuf,
}

impl TrustFile {
    fn credential(&self) -> Credential {
        let scope = match self.variable {
            OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE | OTEL_EXPORTER_OTLP_CLIENT_KEY => {
                SettingScope::Common
            }
            _ => SettingScope::Signal,
        };
        Credential {
            scope,
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
}

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
        let certificates = reqwest::Certificate::from_pem_bundle(&file.read()?).map_err(|_| {
            TracingError::TrustPem {
                name: file.variable,
            }
        })?;
        if certificates.is_empty() {
            return Err(TracingError::TrustPem {
                name: file.variable,
            });
        }
        builder = builder.tls_certs_only(certificates);
    }
    match (&trust.client_certificate, &trust.client_key) {
        (Some(certificate), Some(key)) => {
            let mut pem = key.read()?;
            if certificate.path.as_os_str() != key.path.as_os_str() {
                pem.push(b'\n');
                pem.extend(certificate.read()?);
            }
            let identity =
                reqwest::Identity::from_pem(&pem).map_err(|_| TracingError::IdentityPem)?;
            builder = builder.identity(identity);
        }
        (Some(present), None) => {
            return Err(TracingError::IncompleteIdentity {
                present: present.variable,
                missing: OTEL_EXPORTER_OTLP_CLIENT_KEY,
            });
        }
        (None, Some(present)) => {
            return Err(TracingError::IncompleteIdentity {
                present: present.variable,
                missing: OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE,
            });
        }
        (None, None) => {}
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

fn parse_headers(setting: Setting<'_>) -> Result<HashMap<String, String>, TracingError> {
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
