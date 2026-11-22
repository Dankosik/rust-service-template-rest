use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use temporalio_common::data_converters::PayloadCodec;
use url::Url;
use zeroize::Zeroizing;

use crate::connection::ConnectError;
use crate::credentials::validate_inline_api_key;

const MAX_IDENTIFIER_BYTES: usize = 255;
const MAX_TEMPORAL_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsRotationMode {
    Static,
    Dynamic,
}

#[derive(Clone)]
pub struct TemporalMtlsOptions {
    pub client_cert_path: PathBuf,
    pub client_key_path: PathBuf,
    pub rotation: TlsRotationMode,
}

impl fmt::Debug for TemporalMtlsOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalMtlsOptions")
            .field("client_cert_path", &"<redacted>")
            .field("client_key_path", &"<redacted>")
            .field("rotation", &self.rotation)
            .finish()
    }
}

#[derive(Clone)]
pub struct TemporalTlsOptions {
    pub server_root_ca_path: Option<PathBuf>,
    pub domain: Option<String>,
    pub mtls: Option<TemporalMtlsOptions>,
}

impl fmt::Debug for TemporalTlsOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalTlsOptions")
            .field(
                "server_root_ca_path",
                &self.server_root_ca_path.as_ref().map(|_| "<redacted>"),
            )
            .field("domain", &self.domain)
            .field("mtls", &self.mtls)
            .finish()
    }
}

#[derive(Clone)]
pub enum TemporalAuth {
    None,
    ApiKey(Zeroizing<String>),
    ApiKeyFile(PathBuf),
}

impl fmt::Debug for TemporalAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("TemporalAuth::None"),
            Self::ApiKey(_) => formatter.write_str("TemporalAuth::ApiKey(<redacted>)"),
            Self::ApiKeyFile(_) => formatter.write_str("TemporalAuth::ApiKeyFile(<redacted>)"),
        }
    }
}

#[derive(Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "TemporalOptions exposes four independent transport and security policy flags"
)]
pub struct TemporalOptions {
    pub endpoint: Url,
    pub namespace: String,
    pub identity: String,
    pub auth: TemporalAuth,
    pub tls: Option<TemporalTlsOptions>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub trusted_network: bool,
    pub credential_refresh_interval: Duration,
    pub connect_timeout: Duration,
    pub rpc_timeout: Duration,
    pub keep_alive_interval: Duration,
    pub keep_alive_timeout: Duration,
    pub dns_load_balancing: bool,
    pub http_proxy_url: Option<Url>,
    pub max_payload_bytes: usize,
    pub probe_budget: Duration,
    pub payload_codec: Option<Arc<dyn PayloadCodec + Send + Sync>>,
}

impl fmt::Debug for TemporalOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalOptions")
            .field("endpoint_scheme", &self.endpoint.scheme())
            .field("namespace", &self.namespace)
            .field("identity", &self.identity)
            .field("auth", &self.auth)
            .field("tls", &self.tls)
            .field("allow_plaintext", &self.allow_plaintext)
            .field("allow_unauthenticated", &self.allow_unauthenticated)
            .field("trusted_network", &self.trusted_network)
            .field(
                "credential_refresh_interval",
                &self.credential_refresh_interval,
            )
            .field("connect_timeout", &self.connect_timeout)
            .field("rpc_timeout", &self.rpc_timeout)
            .field("keep_alive_interval", &self.keep_alive_interval)
            .field("keep_alive_timeout", &self.keep_alive_timeout)
            .field("dns_load_balancing", &self.dns_load_balancing)
            .field(
                "http_proxy_url",
                &self.http_proxy_url.as_ref().map(|_| "<configured>"),
            )
            .field("max_payload_bytes", &self.max_payload_bytes)
            .field("probe_budget", &self.probe_budget)
            .field("payload_codec", &self.payload_codec.is_some())
            .finish()
    }
}

impl TemporalOptions {
    pub(crate) fn has_api_key(&self) -> bool {
        matches!(
            self.auth,
            TemporalAuth::ApiKey(_) | TemporalAuth::ApiKeyFile(_)
        )
    }

    pub(crate) fn tls_active(&self) -> bool {
        self.endpoint.scheme() == "https" || self.tls.is_some() || self.has_api_key()
    }

    pub(crate) fn validate(&self) -> Result<(), ConnectError> {
        validate_endpoint(&self.endpoint)?;
        if self.endpoint.scheme() == "http" && !self.allow_plaintext && !self.trusted_network {
            return Err(ConnectError::InvalidConfig(
                "plaintext endpoint requires allow_plaintext or trusted_network",
            ));
        }

        validate_namespace(&self.namespace)?;
        validate_identity(&self.identity)?;

        if let TemporalAuth::ApiKey(key) = &self.auth
            && !validate_inline_api_key(key.as_str())
        {
            return Err(ConnectError::InvalidConfig(
                "api_key must be non-empty ASCII header value",
            ));
        }

        if let Some(tls) = &self.tls {
            if let Some(domain) = &tls.domain
                && (domain.trim().is_empty()
                    || domain.trim() != domain
                    || domain.chars().any(char::is_control))
            {
                return Err(ConnectError::InvalidConfig(
                    "tls domain must be non-blank without control characters",
                ));
            }
            if let Some(mtls) = &tls.mtls
                && mtls.rotation == TlsRotationMode::Dynamic
                && (self.dns_load_balancing || self.http_proxy_url.is_some())
            {
                return Err(ConnectError::InvalidConfig(
                    "dynamic mtls rotation conflicts with dns_load_balancing or http_proxy_url",
                ));
            }
        }

        let has_mtls = self.tls.as_ref().and_then(|t| t.mtls.as_ref()).is_some();
        if self.tls_active()
            && !self.has_api_key()
            && !has_mtls
            && !self.allow_unauthenticated
            && !self.trusted_network
        {
            return Err(ConnectError::InvalidConfig(
                "tls connection requires api_key or mtls authentication",
            ));
        }

        if self.dns_load_balancing && self.http_proxy_url.is_some() {
            return Err(ConnectError::InvalidConfig(
                "dns_load_balancing and http_proxy_url are mutually exclusive",
            ));
        }

        if let Some(proxy) = &self.http_proxy_url {
            validate_proxy_url(proxy)?;
        }

        if self.connect_timeout.is_zero()
            || self.rpc_timeout.is_zero()
            || self.probe_budget.is_zero()
            || self.credential_refresh_interval.is_zero()
            || self.keep_alive_interval.is_zero()
            || self.keep_alive_timeout.is_zero()
        {
            return Err(ConnectError::InvalidConfig(
                "durations must be strictly positive",
            ));
        }
        if self.keep_alive_timeout >= self.keep_alive_interval {
            return Err(ConnectError::InvalidConfig(
                "keep_alive_timeout must be less than keep_alive_interval",
            ));
        }
        if self.max_payload_bytes == 0 || self.max_payload_bytes > MAX_TEMPORAL_PAYLOAD_BYTES {
            return Err(ConnectError::InvalidConfig(
                "max_payload_bytes out of bounds",
            ));
        }

        Ok(())
    }
}

fn validate_endpoint(url: &Url) -> Result<(), ConnectError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ConnectError::InvalidConfig(
            "endpoint scheme must be http or https",
        ));
    }
    if !url.has_host() {
        return Err(ConnectError::InvalidConfig("endpoint must include a host"));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || shared_rules::uri::authority_has_userinfo(url.as_str())
    {
        return Err(ConnectError::InvalidConfig(
            "endpoint must not contain userinfo",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ConnectError::InvalidConfig(
            "endpoint must not contain query or fragment",
        ));
    }
    if url
        .as_str()
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(ConnectError::InvalidConfig(
            "endpoint contains invalid whitespace or control characters",
        ));
    }
    Ok(())
}

fn validate_proxy_url(url: &Url) -> Result<(), ConnectError> {
    if url.scheme() != "http" {
        return Err(ConnectError::InvalidConfig(
            "http_proxy_url scheme must be http",
        ));
    }
    if !url.has_host() || url.port_or_known_default().is_none() {
        return Err(ConnectError::InvalidConfig(
            "http_proxy_url must include a host and port",
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || shared_rules::uri::authority_has_userinfo(url.as_str())
    {
        return Err(ConnectError::InvalidConfig(
            "http_proxy_url must not contain userinfo",
        ));
    }
    if url.query().is_some()
        || url.fragment().is_some()
        || url
            .as_str()
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(ConnectError::InvalidConfig(
            "http_proxy_url contains invalid query, fragment, whitespace, or control characters",
        ));
    }
    Ok(())
}

fn validate_namespace(namespace: &str) -> Result<(), ConnectError> {
    if namespace.is_empty()
        || namespace.len() > MAX_IDENTIFIER_BYTES
        || namespace.trim() != namespace
        || !namespace.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
    {
        return Err(ConnectError::InvalidConfig(
            "namespace must be non-blank ASCII printable within 255 bytes",
        ));
    }
    Ok(())
}

fn validate_identity(identity: &str) -> Result<(), ConnectError> {
    if identity.is_empty()
        || identity.len() > MAX_IDENTIFIER_BYTES
        || identity.trim() != identity
        || identity.chars().any(char::is_control)
    {
        return Err(ConnectError::InvalidConfig(
            "identity must be non-blank without control characters within 255 bytes",
        ));
    }
    Ok(())
}
