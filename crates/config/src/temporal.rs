use std::path::{Path, PathBuf};
use std::time::Duration;

use bytesize::ByteSize;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use url::Url;

use crate::app::local_development_only;
use crate::de::{BlankAsNone, BlankAsNoneVerbatim, StrictBool, StrictDuration};
use crate::validate::{
    Rejected, ValidationError, Violations, duration_range, nonzero_usize_range, optional_pair,
    required,
};

const DEFAULT_NAMESPACE: &str = "default";
const MAX_NAME_BYTES: usize = 255;
const MIN_PAYLOAD_BYTES: usize = 1024;
const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_PAYLOAD_BYTES: ByteSize = ByteSize::kib(512);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalTlsRotation {
    #[default]
    Static,
    Dynamic,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Temporal transport, authentication, load balancing, and readiness flags are independent operator settings"
)]
#[serde_with::serde_as]
#[derive(derive_more::Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct TemporalInput {
    #[serde_as(as = "StrictBool")]
    enabled: bool,
    #[serde_as(as = "BlankAsNoneVerbatim")]
    #[debug(skip)]
    endpoint: Option<String>,
    #[educe(Default(expression = DEFAULT_NAMESPACE.to_owned()))]
    namespace: String,
    identity: Option<String>,
    #[serde_as(as = "BlankAsNoneVerbatim")]
    api_key: Option<SecretString>,
    #[serde_as(as = "BlankAsNone")]
    #[debug("{}", api_key_file.is_some())]
    api_key_file: Option<PathBuf>,
    #[serde_as(as = "BlankAsNone")]
    #[debug("{}", client_cert_path.is_some())]
    client_cert_path: Option<PathBuf>,
    #[serde_as(as = "BlankAsNone")]
    #[debug("{}", client_key_path.is_some())]
    client_key_path: Option<PathBuf>,
    #[serde_as(as = "BlankAsNone")]
    #[debug("{}", root_ca_path.is_some())]
    root_ca_path: Option<PathBuf>,
    mtls_rotation: TemporalTlsRotation,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(30)))]
    credential_refresh_interval: Duration,
    #[serde_as(as = "StrictBool")]
    allow_plaintext: bool,
    #[serde_as(as = "StrictBool")]
    allow_unauthenticated: bool,
    #[serde_as(as = "StrictBool")]
    trusted_network: bool,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(5)))]
    connect_timeout: Duration,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(10)))]
    rpc_timeout: Duration,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(30)))]
    keep_alive_interval: Duration,
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(15)))]
    keep_alive_timeout: Duration,
    #[serde_as(as = "StrictBool")]
    #[educe(Default = true)]
    dns_load_balancing: bool,
    #[serde_as(as = "BlankAsNoneVerbatim")]
    #[debug(skip)]
    http_proxy_url: Option<String>,
    #[educe(Default(expression = DEFAULT_PAYLOAD_BYTES))]
    max_payload_bytes: ByteSize,
    #[serde_as(as = "StrictBool")]
    readiness_critical: bool,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Temporal transport, authentication, load balancing, and readiness flags are independent operator settings"
)]
#[derive(Clone, derive_more::Debug)]
pub struct TemporalClientConfig {
    #[debug(skip)]
    endpoint: Url,
    namespace: String,
    identity: String,
    api_key: Option<SecretString>,
    #[debug("{}", api_key_file.is_some())]
    api_key_file: Option<PathBuf>,
    #[debug("{}", client_cert_path.is_some())]
    client_cert_path: Option<PathBuf>,
    #[debug("{}", client_key_path.is_some())]
    client_key_path: Option<PathBuf>,
    #[debug("{}", root_ca_path.is_some())]
    root_ca_path: Option<PathBuf>,
    mtls_rotation: TemporalTlsRotation,
    credential_refresh_interval: Duration,
    allow_plaintext: bool,
    allow_unauthenticated: bool,
    trusted_network: bool,
    connect_timeout: Duration,
    rpc_timeout: Duration,
    keep_alive_interval: Duration,
    keep_alive_timeout: Duration,
    dns_load_balancing: bool,
    #[debug(skip)]
    http_proxy_url: Option<Url>,
    max_payload_bytes: usize,
    readiness_critical: bool,
}

impl TemporalClientConfig {
    #[must_use]
    pub const fn endpoint(&self) -> &Url {
        &self.endpoint
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub const fn api_key(&self) -> Option<&SecretString> {
        self.api_key.as_ref()
    }

    #[must_use]
    pub fn api_key_file(&self) -> Option<&Path> {
        self.api_key_file.as_deref()
    }

    #[must_use]
    pub fn client_cert_path(&self) -> Option<&Path> {
        self.client_cert_path.as_deref()
    }

    #[must_use]
    pub fn client_key_path(&self) -> Option<&Path> {
        self.client_key_path.as_deref()
    }

    #[must_use]
    pub fn root_ca_path(&self) -> Option<&Path> {
        self.root_ca_path.as_deref()
    }

    #[must_use]
    pub const fn mtls_rotation(&self) -> TemporalTlsRotation {
        self.mtls_rotation
    }

    #[must_use]
    pub const fn credential_refresh_interval(&self) -> Duration {
        self.credential_refresh_interval
    }

    #[must_use]
    pub const fn allow_plaintext(&self) -> bool {
        self.allow_plaintext
    }

    #[must_use]
    pub const fn allow_unauthenticated(&self) -> bool {
        self.allow_unauthenticated
    }

    #[must_use]
    pub const fn trusted_network(&self) -> bool {
        self.trusted_network
    }

    #[must_use]
    pub const fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    #[must_use]
    pub const fn rpc_timeout(&self) -> Duration {
        self.rpc_timeout
    }

    #[must_use]
    pub const fn keep_alive_interval(&self) -> Duration {
        self.keep_alive_interval
    }

    #[must_use]
    pub const fn keep_alive_timeout(&self) -> Duration {
        self.keep_alive_timeout
    }

    #[must_use]
    pub const fn dns_load_balancing(&self) -> bool {
        self.dns_load_balancing
    }

    #[must_use]
    pub const fn http_proxy_url(&self) -> Option<&Url> {
        self.http_proxy_url.as_ref()
    }

    #[must_use]
    pub const fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    #[must_use]
    pub const fn readiness_critical(&self) -> bool {
        self.readiness_critical
    }
}

impl TemporalInput {
    pub(crate) fn admit(
        mut self,
        local_development: bool,
        service_name: &str,
        app_version: &str,
        v: &mut Violations,
    ) -> Result<Option<TemporalClientConfig>, Rejected> {
        if !self.enabled {
            return Ok(None);
        }
        v.check(local_development_only(
            "temporal.allow_plaintext",
            self.allow_plaintext,
            local_development,
        ));
        v.check(local_development_only(
            "temporal.allow_unauthenticated",
            self.allow_unauthenticated,
            local_development,
        ));

        let endpoint = self.admit_endpoint(local_development, v);
        let namespace = v.record(validate_ascii_printable_name(
            "temporal.namespace",
            &self.namespace,
        ));
        let identity = match self.identity.take() {
            Some(raw) => v.record(validate_identity(&raw)),
            None => Ok(format!("{service_name}@{app_version}")),
        };

        let has_proxy_input = self.http_proxy_url.is_some();
        let http_proxy_url = match self.http_proxy_url.take() {
            Some(raw) => v
                .record(validate_http_url("temporal.http_proxy_url", &raw))
                .map(Some),
            None => Ok(None),
        };
        if self.dns_load_balancing && has_proxy_input {
            v.reject(ValidationError::new(
                "temporal.http_proxy_url",
                "cannot be set when temporal.dns_load_balancing = true",
            ));
        }

        let tls_active = endpoint.as_ref().is_ok_and(|url| url.scheme() == "https");
        let client_pair =
            self.admit_auth_and_mtls(local_development, tls_active, has_proxy_input, v);
        self.validate_timings(v);
        let max_payload_bytes = v.record(nonzero_usize_range(
            "temporal.max_payload_bytes",
            self.max_payload_bytes.as_u64(),
            MIN_PAYLOAD_BYTES..=MAX_PAYLOAD_BYTES,
        ));

        let (client_cert_path, client_key_path) = match client_pair? {
            Some((cert, key)) => (Some(cert), Some(key)),
            None => (None, None),
        };

        Ok(Some(TemporalClientConfig {
            endpoint: endpoint?,
            namespace: namespace?,
            identity: identity?,
            api_key: self.api_key,
            api_key_file: self.api_key_file,
            client_cert_path,
            client_key_path,
            root_ca_path: self.root_ca_path,
            mtls_rotation: self.mtls_rotation,
            credential_refresh_interval: self.credential_refresh_interval,
            allow_plaintext: self.allow_plaintext,
            allow_unauthenticated: self.allow_unauthenticated,
            trusted_network: self.trusted_network,
            connect_timeout: self.connect_timeout,
            rpc_timeout: self.rpc_timeout,
            keep_alive_interval: self.keep_alive_interval,
            keep_alive_timeout: self.keep_alive_timeout,
            dns_load_balancing: self.dns_load_balancing,
            http_proxy_url: http_proxy_url?,
            max_payload_bytes: max_payload_bytes?.get(),
            readiness_critical: self.readiness_critical,
        }))
    }

    fn admit_endpoint(
        &mut self,
        local_development: bool,
        v: &mut Violations,
    ) -> Result<Url, Rejected> {
        let endpoint = v.record(
            required(
                "temporal.endpoint",
                self.endpoint.take(),
                "temporal.enabled = true",
            )
            .and_then(|value| validate_http_url("temporal.endpoint", &value)),
        );
        if let Ok(ref url) = endpoint
            && url.scheme() == "http"
            && !((self.allow_plaintext && local_development) || self.trusted_network)
        {
            v.reject(ValidationError::new(
                "temporal.endpoint",
                "must use https:// unless temporal.allow_plaintext (with app.allow_insecure_development) or temporal.trusted_network is true",
            ));
        }
        endpoint
    }

    fn admit_auth_and_mtls(
        &mut self,
        local_development: bool,
        tls_active: bool,
        has_proxy_input: bool,
        v: &mut Violations,
    ) -> Result<Option<(PathBuf, PathBuf)>, Rejected> {
        if self.api_key.is_some() && self.api_key_file.is_some() {
            v.reject(ValidationError::new(
                "temporal.api_key_file",
                "cannot be set together with temporal.api_key",
            ));
        }
        if let Some(ref secret) = self.api_key {
            v.check(validate_api_key(secret.expose_secret()));
        }

        let client_pair = v.record(optional_pair(
            "temporal.client_cert_path",
            self.client_cert_path.take(),
            "temporal.client_key_path",
            self.client_key_path.take(),
        ));
        let has_mtls = matches!(client_pair, Ok(Some(_)));

        if self.mtls_rotation == TemporalTlsRotation::Dynamic {
            if !has_mtls {
                v.reject(ValidationError::new(
                    "temporal.mtls_rotation",
                    "dynamic rotation requires temporal.client_cert_path and temporal.client_key_path",
                ));
            }
            if self.dns_load_balancing {
                v.reject(ValidationError::new(
                    "temporal.mtls_rotation",
                    "cannot be dynamic when temporal.dns_load_balancing = true",
                ));
            }
            if has_proxy_input {
                v.reject(ValidationError::new(
                    "temporal.mtls_rotation",
                    "cannot be dynamic when temporal.http_proxy_url is set",
                ));
            }
        }

        let has_auth = self.api_key.is_some() || self.api_key_file.is_some() || has_mtls;
        let unauth_allowed =
            (self.allow_unauthenticated && local_development) || self.trusted_network;
        if tls_active && !has_auth && !unauth_allowed {
            v.reject(ValidationError::new(
                "temporal.api_key",
                "authentication (temporal.api_key, temporal.api_key_file, or mTLS client certificate pair) is required for https:// endpoints unless temporal.allow_unauthenticated (with app.allow_insecure_development) or temporal.trusted_network is true",
            ));
        }
        client_pair
    }

    fn validate_timings(&self, v: &mut Violations) {
        v.check(duration_range(
            "temporal.credential_refresh_interval",
            self.credential_refresh_interval,
            Duration::from_secs(5)..=Duration::from_mins(15),
        ));
        v.check(duration_range(
            "temporal.connect_timeout",
            self.connect_timeout,
            Duration::from_millis(100)..=Duration::from_secs(60),
        ));
        v.check(duration_range(
            "temporal.rpc_timeout",
            self.rpc_timeout,
            Duration::from_millis(100)..=Duration::from_mins(5),
        ));
        let keep_alive_interval_ok = v
            .record(duration_range(
                "temporal.keep_alive_interval",
                self.keep_alive_interval,
                Duration::from_secs(10)..=Duration::from_mins(5),
            ))
            .is_ok();
        let keep_alive_timeout_ok = v
            .record(duration_range(
                "temporal.keep_alive_timeout",
                self.keep_alive_timeout,
                Duration::from_secs(1)..=Duration::from_secs(60),
            ))
            .is_ok();
        if keep_alive_interval_ok
            && keep_alive_timeout_ok
            && self.keep_alive_timeout >= self.keep_alive_interval
        {
            v.reject(ValidationError::new(
                "temporal.keep_alive_timeout",
                format!(
                    "must be less than temporal.keep_alive_interval ({}), got {}",
                    crate::de::display_duration(self.keep_alive_interval),
                    crate::de::display_duration(self.keep_alive_timeout),
                ),
            ));
        }
    }
}

pub(crate) fn validate_ascii_printable_name(
    key: &str,
    raw: &str,
) -> Result<String, ValidationError> {
    if raw.chars().any(char::is_control) {
        return Err(ValidationError::new(
            key,
            "must not contain control characters",
        ));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::new(key, "cannot be empty"));
    }
    if trimmed.len() > MAX_NAME_BYTES {
        return Err(ValidationError::new(
            key,
            format!("must be at most {MAX_NAME_BYTES} bytes"),
        ));
    }
    if !trimmed.bytes().all(|byte| matches!(byte, 0x20..=0x7E)) {
        return Err(ValidationError::new(
            key,
            "must contain only printable ASCII characters",
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_identity(raw: &str) -> Result<String, ValidationError> {
    const KEY: &str = "temporal.identity";
    if raw.chars().any(char::is_control) {
        return Err(ValidationError::new(
            KEY,
            "must not contain control characters",
        ));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::new(KEY, "cannot be empty"));
    }
    if trimmed.len() > MAX_NAME_BYTES {
        return Err(ValidationError::new(
            KEY,
            format!("must be at most {MAX_NAME_BYTES} bytes"),
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_api_key(raw: &str) -> Result<(), ValidationError> {
    if raw.trim().is_empty() || !raw.bytes().all(|byte| matches!(byte, 0x20..=0x7E)) {
        return Err(ValidationError::new(
            "temporal.api_key",
            "must be non-blank printable ASCII (0x20..=0x7E)",
        ));
    }
    Ok(())
}

fn validate_http_url(key: &str, raw: &str) -> Result<Url, ValidationError> {
    if shared_rules::uri::has_whitespace_or_control(raw) {
        return Err(ValidationError::new(
            key,
            "must not contain whitespace or control characters",
        ));
    }
    // RFC 9110 §4.2.4
    if shared_rules::uri::authority_has_userinfo(raw) {
        return Err(ValidationError::new(key, "must not include userinfo"));
    }
    if raw.contains(['?', '#', '\\']) {
        return Err(ValidationError::new(
            key,
            "must not include a query, fragment, or backslash",
        ));
    }
    let Ok(url) = Url::parse(raw) else {
        return Err(ValidationError::new(
            key,
            "must be a valid http:// or https:// URL",
        ));
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ValidationError::new(
            key,
            "must use an http:// or https:// scheme",
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(ValidationError::new(key, "must include a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ValidationError::new(key, "must not include userinfo"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ValidationError::new(
            key,
            "must not include a query or fragment",
        ));
    }
    Ok(url)
}

#[cfg(test)]
mod tests;
