use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use shared_rules::clickhouse::{
    DEFAULT_HTTP_RESPONSE_BUFFER_BYTES, MAX_CONCURRENCY_CEILING, MAX_MEMORY_USAGE,
    MAX_RESULT_BYTES, MAX_RESULT_ROWS_CEILING, MAX_TIMEOUT, MIN_MEMORY_USAGE, MIN_RESULT_BYTES,
    MIN_TIMEOUT, UrlLexicalError, strip_root_trailing_slash, trim_path_prefix,
    validate_url_lexical,
};
use shared_rules::rfc9110::has_ascii_control;
use url::Url;

use crate::app::local_development_only;
use crate::de::{BlankAsNone, BlankAsNoneVerbatim, StrictBool, StrictCount, StrictDuration};
use crate::validate::{
    Rejected, ValidationError, Violations, duration_range, int_range, missing, nonzero_usize_range,
    optional_pair, required,
};

/// Where per-query execution, overflow, and memory budgets live.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClickHouseQuerySettings {
    /// The client attaches per-query budget and cancellation settings.
    #[default]
    Client,
    /// The server user profile owns query budgets (for `readonly = 1` users).
    Server,
}

#[derive(Clone, Debug)]
pub struct ClickHouseClientCertificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

#[serde_with::serde_as]
#[derive(Clone, Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct ClickHouseInput {
    /// `ClickHouse` HTTP(S) base URL without userinfo, query, or fragment.
    /// Missing, empty, or whitespace-only leaves `ClickHouse` inert (`None`).
    #[serde_as(as = "BlankAsNoneVerbatim")]
    url: Option<String>,
    /// Target database for `ClickHouse` queries and inserts.
    #[serde_as(as = "BlankAsNoneVerbatim")]
    database: Option<String>,
    /// Username sent in `ClickHouse` HTTP authentication headers.
    #[serde_as(as = "BlankAsNoneVerbatim")]
    username: Option<String>,
    /// Password sent in `ClickHouse` HTTP authentication headers.
    /// Set `APP__CLICKHOUSE__PASSWORD` in the environment or via `--secrets-dir`.
    #[serde_as(as = "BlankAsNoneVerbatim")]
    password: Option<SecretString>,
    /// Regular file (`<= 16 KiB`) holding the password for rotation.
    /// Mutually exclusive with `password`.
    #[serde_as(as = "BlankAsNone")]
    password_file: Option<PathBuf>,
    /// PEM root CA path (`<= 1 MiB`) for private HTTPS certificates.
    #[serde_as(as = "BlankAsNone")]
    root_ca_path: Option<PathBuf>,
    /// PEM certificate chain (`<= 1 MiB`) for mutual TLS. Set with `client_key_path`.
    #[serde_as(as = "BlankAsNone")]
    client_cert_path: Option<PathBuf>,
    /// PEM private key (`<= 64 KiB`) matching `client_cert_path`.
    #[serde_as(as = "BlankAsNone")]
    client_key_path: Option<PathBuf>,
    /// Permit `http://` with `app.allow_insecure_development`.
    #[serde_as(as = "StrictBool")]
    allow_plaintext: bool,
    /// Permit unauthenticated connections with `app.allow_insecure_development`.
    #[serde_as(as = "StrictBool")]
    allow_unauthenticated: bool,
    query_settings: ClickHouseQuerySettings,
    /// Default ceiling for bounded read queries (`10ms..=5m`).
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(5)))]
    query_timeout: Duration,
    /// Default ceiling for synchronous and server-buffered writes (`10ms..=5m`).
    #[serde_as(as = "StrictDuration")]
    #[educe(Default(expression = Duration::from_secs(5)))]
    insert_timeout: Duration,
    /// Maximum simultaneous in-flight `ClickHouse` operations (`1..=1024`).
    #[serde_as(as = "StrictCount")]
    #[educe(Default = 32)]
    max_concurrency: usize,
    /// Maximum rows returned by one bounded read (`1..=10_000_000`).
    #[serde_as(as = "StrictCount")]
    #[educe(Default = 100_000)]
    max_result_rows: u64,
    /// Maximum decoded `RowBinary` response bytes per read (`64 KiB..=128 MiB`).
    #[educe(Default(expression = ByteSize::mib(16)))]
    max_result_bytes: ByteSize,
    /// Optional server `buffer_size` override (`64 KiB..=max_result_bytes`).
    /// Defaults to `min(2 MiB, max_result_bytes)`.
    http_response_buffer_size: Option<ByteSize>,
    /// Per-query `ClickHouse` memory limit (`16 MiB..=64 GiB`, or `0` to disable).
    #[educe(Default(expression = ByteSize::mib(512)))]
    max_memory_usage: ByteSize,
    /// Inject W3C `traceparent`/`tracestate` on outgoing `ClickHouse` requests.
    #[serde_as(as = "StrictBool")]
    #[educe(Default = true)]
    propagate_trace_context: bool,
}

#[derive(Clone, Debug)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: String,
    pub username: Option<String>,
    pub password: Option<SecretString>,
    pub password_file: Option<PathBuf>,
    pub root_ca_path: Option<PathBuf>,
    pub client_certificate: Option<ClickHouseClientCertificate>,
    pub allow_plaintext: bool,
    pub allow_unauthenticated: bool,
    pub query_settings: ClickHouseQuerySettings,
    pub query_timeout: Duration,
    pub insert_timeout: Duration,
    pub max_concurrency: NonZeroUsize,
    pub max_result_rows: NonZeroU64,
    pub max_result_bytes: NonZeroUsize,
    pub http_response_buffer_size: NonZeroUsize,
    pub max_memory_usage: Option<NonZeroU64>,
    pub propagate_trace_context: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UrlScheme {
    Https,
    Http,
}

struct AdmittedLimits {
    max_concurrency: NonZeroUsize,
    max_result_rows: NonZeroU64,
    max_result_bytes: NonZeroUsize,
    http_response_buffer_size: NonZeroUsize,
    max_memory_usage: Option<NonZeroU64>,
}

impl ClickHouseInput {
    pub(crate) fn admit(
        self,
        local_development: bool,
        v: &mut Violations,
    ) -> Result<Option<ClickHouseConfig>, Rejected> {
        v.check(local_development_only(
            "clickhouse.allow_plaintext",
            self.allow_plaintext,
            local_development,
        ));
        v.check(local_development_only(
            "clickhouse.allow_unauthenticated",
            self.allow_unauthenticated,
            local_development,
        ));
        let limits = self.admit_limits(v);
        let has_cert_path = self.client_cert_path.is_some();
        let has_key_path = self.client_key_path.is_some();
        let client_certificate = v
            .record(optional_pair(
                "clickhouse.client_cert_path",
                self.client_cert_path,
                "clickhouse.client_key_path",
                self.client_key_path,
            ))
            .map(|pair| {
                pair.map(|(cert_path, key_path)| ClickHouseClientCertificate {
                    cert_path,
                    key_path,
                })
            });
        if let Some(database) = self.database.as_deref() {
            v.check(header_safe("clickhouse.database", database));
        }
        if let Some(username) = self.username.as_deref() {
            v.check(header_safe("clickhouse.username", username));
        }
        if let Some(password) = self.password.as_ref() {
            v.check(header_safe("clickhouse.password", password.expose_secret()));
        }
        if self.password.is_some() && self.password_file.is_some() {
            v.reject(ValidationError::new(
                "clickhouse.password_file",
                "cannot be set together with clickhouse.password",
            ));
        }

        let Some(raw_url) = self.url else {
            let _ = client_certificate?;
            let _ = limits?;
            return Ok(None);
        };

        let url_result = validate_url(&raw_url, self.allow_plaintext, v);
        if matches!(url_result, Ok((_, UrlScheme::Http))) {
            if self.root_ca_path.is_some() {
                v.reject(ValidationError::new(
                    "clickhouse.root_ca_path",
                    "is not valid with an http:// clickhouse.url",
                ));
            }
            v.reject_present(
                &[
                    ("clickhouse.client_cert_path", has_cert_path),
                    ("clickhouse.client_key_path", has_key_path),
                ],
                "is not valid with an http:// clickhouse.url",
            );
        }

        validate_auth_matrix(
            self.allow_unauthenticated,
            self.username.as_deref(),
            self.password.as_ref(),
            self.password_file.as_deref(),
            v,
        );

        let database = v.record(required(
            "clickhouse.database",
            self.database,
            "clickhouse.url is set",
        ));

        let (url, _) = url_result?;
        let client_certificate = client_certificate?;
        let limits = limits?;
        let database = database?;

        Ok(Some(ClickHouseConfig {
            url,
            database,
            username: self.username,
            password: self.password,
            password_file: self.password_file,
            root_ca_path: self.root_ca_path,
            client_certificate,
            allow_plaintext: self.allow_plaintext,
            allow_unauthenticated: self.allow_unauthenticated,
            query_settings: self.query_settings,
            query_timeout: self.query_timeout,
            insert_timeout: self.insert_timeout,
            max_concurrency: limits.max_concurrency,
            max_result_rows: limits.max_result_rows,
            max_result_bytes: limits.max_result_bytes,
            http_response_buffer_size: limits.http_response_buffer_size,
            max_memory_usage: limits.max_memory_usage,
            propagate_trace_context: self.propagate_trace_context,
        }))
    }

    fn admit_limits(&self, v: &mut Violations) -> Result<AdmittedLimits, Rejected> {
        v.check(duration_range(
            "clickhouse.query_timeout",
            self.query_timeout,
            MIN_TIMEOUT..=MAX_TIMEOUT,
        ));
        v.check(duration_range(
            "clickhouse.insert_timeout",
            self.insert_timeout,
            MIN_TIMEOUT..=MAX_TIMEOUT,
        ));
        let max_concurrency = v.record(nonzero_usize_range(
            "clickhouse.max_concurrency",
            self.max_concurrency,
            1..=MAX_CONCURRENCY_CEILING,
        ));
        let max_result_rows = v.record(nonzero_u64_range(
            "clickhouse.max_result_rows",
            self.max_result_rows,
            1..=MAX_RESULT_ROWS_CEILING,
        ));
        let max_result_bytes = v.record(nonzero_usize_range(
            "clickhouse.max_result_bytes",
            self.max_result_bytes.as_u64(),
            MIN_RESULT_BYTES..=MAX_RESULT_BYTES,
        ));
        let http_response_buffer_size = self.http_response_buffer_size.map(|buf| {
            let ceiling = max_result_bytes
                .as_ref()
                .map_or(MAX_RESULT_BYTES, |max| max.get());
            v.record(nonzero_usize_range(
                "clickhouse.http_response_buffer_size",
                buf.as_u64(),
                MIN_RESULT_BYTES..=ceiling,
            ))
        });
        let max_memory_usage = if self.max_memory_usage.as_u64() == 0 {
            Ok(None)
        } else {
            v.record(nonzero_u64_range(
                "clickhouse.max_memory_usage",
                self.max_memory_usage.as_u64(),
                MIN_MEMORY_USAGE..=MAX_MEMORY_USAGE,
            ))
            .map(Some)
        };

        let max_concurrency = max_concurrency?;
        let max_result_rows = max_result_rows?;
        let max_result_bytes = max_result_bytes?;
        let http_response_buffer_size = match http_response_buffer_size {
            Some(buf) => buf?,
            None => max_result_bytes.min(DEFAULT_HTTP_RESPONSE_BUFFER_BYTES),
        };
        let max_memory_usage = max_memory_usage?;

        Ok(AdmittedLimits {
            max_concurrency,
            max_result_rows,
            max_result_bytes,
            http_response_buffer_size,
            max_memory_usage,
        })
    }
}

fn validate_auth_matrix(
    allow_unauthenticated: bool,
    username: Option<&str>,
    password: Option<&SecretString>,
    password_file: Option<&std::path::Path>,
    v: &mut Violations,
) {
    if allow_unauthenticated {
        v.reject_present(
            &[
                ("clickhouse.password", password.is_some()),
                ("clickhouse.password_file", password_file.is_some()),
            ],
            "cannot be set when clickhouse.allow_unauthenticated = true",
        );
    } else {
        if username.is_none() {
            v.reject(missing(
                "clickhouse.username",
                "clickhouse.url is set and clickhouse.allow_unauthenticated = false",
            ));
        }
        if password.is_none() && password_file.is_none() {
            v.reject(ValidationError::new(
                "clickhouse.password",
                "one of clickhouse.password or clickhouse.password_file is required unless clickhouse.allow_unauthenticated = true; set APP__CLICKHOUSE__PASSWORD",
            ));
        }
    }
}

fn validate_url(
    raw: &str,
    allow_plaintext: bool,
    v: &mut Violations,
) -> Result<(String, UrlScheme), Rejected> {
    let key = "clickhouse.url";
    if let Err(err) = validate_url_lexical(raw) {
        let reason = match err {
            UrlLexicalError::ForbiddenCharacters => {
                "must be a valid http:// or https:// URL without whitespace, controls, query, fragment, or backslash"
            }
            UrlLexicalError::MissingHost => "must include a valid host",
            UrlLexicalError::UserinfoNotAllowed => "must not include userinfo",
            UrlLexicalError::EmptyPathSegment => "path prefix must not contain empty segments",
            UrlLexicalError::DotPathSegment => "path prefix must not contain empty or dot segments",
        };
        return Err(v.reject(ValidationError::new(key, reason)));
    }
    let Ok(mut parsed) = Url::parse(raw) else {
        return Err(v.reject(ValidationError::new(
            key,
            "must be a valid http:// or https:// URL",
        )));
    };
    let scheme = match parsed.scheme() {
        "https" => UrlScheme::Https,
        "http" => {
            if !allow_plaintext {
                v.reject(ValidationError::new(
                    key,
                    "must use https:// unless clickhouse.allow_plaintext = true",
                ));
            }
            UrlScheme::Http
        }
        _ => {
            return Err(v.reject(ValidationError::new(key, "must use https:// or http://")));
        }
    };
    if parsed.cannot_be_a_base() || parsed.host_str().is_none_or(str::is_empty) {
        return Err(v.reject(ValidationError::new(key, "must include a valid host")));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(v.reject(ValidationError::new(key, "must not include userinfo")));
    }
    let clean_path = trim_path_prefix(parsed.path()).to_owned();
    parsed.set_path(&clean_path);
    let normalized = strip_root_trailing_slash(parsed.into(), &clean_path);
    Ok((normalized, scheme))
}

fn header_safe(key: &str, value: &str) -> Result<(), ValidationError> {
    if has_ascii_control(value.as_bytes()) {
        return Err(ValidationError::new(
            key,
            "must not contain ASCII control characters",
        ));
    }
    Ok(())
}

fn nonzero_u64_range(
    key: &str,
    value: u64,
    range: std::ops::RangeInclusive<u64>,
) -> Result<NonZeroU64, ValidationError> {
    int_range(key, value, range)?;
    NonZeroU64::new(value).ok_or_else(|| ValidationError::new(key, "must be non-zero"))
}

#[cfg(test)]
mod tests;
