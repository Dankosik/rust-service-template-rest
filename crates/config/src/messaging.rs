//! `JetStream` connection and delivery-admission configuration.
//!
//! This section owns startup input only. It never contacts a broker or
//! reconciles operator-owned streams.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use bytesize::ByteSize;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::app::local_development_only;
use crate::de::{blank_as_none, blank_secret_as_none, value_free};
use crate::validate::{ValidationError, int_range, missing, non_blank, required};

/// Header bytes one delivery may add to its payload: `HEADER_LIMIT_BYTES` of
/// the `infra-messaging` wire decoder. Metadata is budgeted separately.
const DELIVERY_OVERHEAD_BYTES: u64 = 8 * 1024;
/// Payload and header MiB held at once: `PUBLICATION_WINDOW_BYTES` of
/// `infra-messaging`, so an admitted payload also fits one publication.
const MAX_RESIDENT_DELIVERY_MIB: u64 = 64;
const MAX_RESIDENT_DELIVERY_BYTES: u64 = MAX_RESIDENT_DELIVERY_MIB * 1024 * 1024;

const PRODUCER_ACTIVE: &str = "a producer is active";
const CONSUMER_ACTIVE: &str = "a consumer is active";

/// Optional `JetStream` connection and worker limits.
#[expect(
    clippy::struct_excessive_bools,
    reason = "NATS trust, authentication and TLS-first are independent operator capabilities"
)]
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MessagingConfig {
    /// NATS server URLs. An empty list keeps messaging inactive.
    #[serde(deserialize_with = "url_list")]
    pub urls: Vec<String>,
    /// Inline NATS credentials. This environment-only value is redacted.
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    pub credentials: Option<SecretString>,
    /// Path to a NATS credentials file, the alternative to `credentials` for
    /// a platform that rotates it: the client reads the file again for every
    /// connection. Missing, empty, or whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    pub credentials_file: Option<PathBuf>,
    /// Optional PEM root CA path for the operator-selected broker. Missing,
    /// empty, or whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    pub root_ca_path: Option<PathBuf>,
    /// Permit `nats://` only for a local or development process.
    pub allow_plaintext: bool,
    /// Perform TLS before INFO; every configured URL must use `tls://`.
    pub tls_first: bool,
    /// The operator declares the private network the broker trust boundary,
    /// as `grpc.security = "plaintext"` does for gRPC: `nats://` is admitted
    /// in every environment. Credentials stay required outside local and
    /// development.
    pub trusted_network: bool,
    /// Permit a connection without credentials only for a local or
    /// development process.
    pub allow_unauthenticated: bool,
    /// Operator-owned source stream for publication and consumption.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub source_stream: Option<String>,
    /// Largest payload admitted before handler allocation.
    pub max_payload_bytes: ByteSize,
    /// Durable consumer name, required only by a consuming worker.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub consumer_durable: Option<String>,
    /// Filter subject, required only by a consuming worker.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub consumer_filter_subject: Option<String>,
    /// DLQ subject, required only by a consuming worker.
    #[serde(default, deserialize_with = "blank_as_none")]
    pub dlq_subject: Option<String>,
    /// Maximum deliveries held through handling and settlement.
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    pub consumer_concurrency: NonZeroU32,
}

impl Default for MessagingConfig {
    fn default() -> Self {
        Self {
            urls: Vec::new(),
            credentials: None,
            credentials_file: None,
            root_ca_path: None,
            allow_plaintext: false,
            tls_first: false,
            trusted_network: false,
            allow_unauthenticated: false,
            source_stream: None,
            // A broker's default `max_payload` is 1 MiB for payload and
            // headers together, so the default must leave room below it; the
            // Go template defaults to the same value.
            max_payload_bytes: ByteSize::kib(256),
            consumer_durable: None,
            consumer_filter_subject: None,
            dlq_subject: None,
            consumer_concurrency: NonZeroU32::MIN,
        }
    }
}

impl MessagingConfig {
    fn is_active(&self) -> bool {
        !self.urls.is_empty()
    }

    #[must_use]
    pub fn plaintext_admitted(&self) -> bool {
        self.allow_plaintext || self.trusted_network
    }

    /// Validate the inputs required by a producer or outbox publisher.
    ///
    /// # Errors
    ///
    /// Returns the first missing producer input or a security-policy error.
    pub fn validate_producer(&self, app_env: &str) -> Result<(), ValidationError> {
        self.validate(app_env)?;
        if !self.is_active() {
            return Err(missing("messaging.urls", PRODUCER_ACTIVE));
        }
        if self.credential_source()?.is_none() && !self.allow_unauthenticated {
            return Err(ValidationError::new(
                "messaging.credentials",
                "one of messaging.credentials or messaging.credentials_file is required unless messaging.allow_unauthenticated = true",
            ));
        }
        self.required_source_stream()?;
        Ok(())
    }

    /// The credential source the client uses, if one is set.
    ///
    /// # Errors
    ///
    /// Returns `messaging.credentials_file` when both sources are set.
    pub fn credential_source(&self) -> Result<Option<MessagingCredentials<'_>>, ValidationError> {
        match (&self.credentials, &self.credentials_file) {
            (Some(_), Some(_)) => Err(ValidationError::new(
                "messaging.credentials_file",
                "cannot be set together with messaging.credentials",
            )),
            (Some(inline), None) => Ok(Some(MessagingCredentials::Inline(inline))),
            (None, Some(file)) => Ok(Some(MessagingCredentials::File(file))),
            (None, None) => Ok(None),
        }
    }

    /// The stream an active producer publishes to.
    ///
    /// # Errors
    ///
    /// Returns `messaging.source_stream` when the value is absent.
    pub fn required_source_stream(&self) -> Result<&str, ValidationError> {
        required(
            "messaging.source_stream",
            self.source_stream.as_deref(),
            PRODUCER_ACTIVE,
        )
    }

    /// Validate the inputs required by a consuming worker.
    ///
    /// # Errors
    ///
    /// Returns the first missing consumer input or a producer-policy error.
    pub fn validate_consumer(&self, app_env: &str) -> Result<(), ValidationError> {
        self.validate_producer(app_env)?;
        self.required_consumer()?;
        Ok(())
    }

    /// The subscription keys an active consumer needs.
    ///
    /// # Errors
    ///
    /// Returns the first missing consumer key.
    pub fn required_consumer(&self) -> Result<ConsumerKeys<'_>, ValidationError> {
        Ok(ConsumerKeys {
            durable: required(
                "messaging.consumer_durable",
                self.consumer_durable.as_deref(),
                CONSUMER_ACTIVE,
            )?,
            filter_subject: required(
                "messaging.consumer_filter_subject",
                self.consumer_filter_subject.as_deref(),
                CONSUMER_ACTIVE,
            )?,
            dlq_subject: required(
                "messaging.dlq_subject",
                self.dlq_subject.as_deref(),
                CONSUMER_ACTIVE,
            )?,
        })
    }

    pub(crate) fn validate(&self, app_env: &str) -> Result<(), ValidationError> {
        local_development_only("messaging.allow_plaintext", self.allow_plaintext, app_env)?;
        local_development_only(
            "messaging.allow_unauthenticated",
            self.allow_unauthenticated,
            app_env,
        )?;
        self.credential_source()?;
        self.validate_urls()?;

        int_range(
            "messaging.max_payload_bytes",
            self.max_payload_bytes.as_u64(),
            1,
            MAX_RESIDENT_DELIVERY_BYTES - DELIVERY_OVERHEAD_BYTES,
        )?;
        // The range above keeps the product far from overflow.
        let resident = u64::from(self.consumer_concurrency.get())
            * (self.max_payload_bytes.as_u64() + DELIVERY_OVERHEAD_BYTES);
        if resident > MAX_RESIDENT_DELIVERY_BYTES {
            return Err(ValidationError::new(
                "messaging.consumer_concurrency",
                format!(
                    "{} × (messaging.max_payload_bytes {} + {} KiB headers) = {resident} bytes exceeds the {MAX_RESIDENT_DELIVERY_MIB} MiB resident delivery bound",
                    self.consumer_concurrency,
                    self.max_payload_bytes.as_u64(),
                    DELIVERY_OVERHEAD_BYTES / 1024,
                ),
            ));
        }
        Ok(())
    }

    fn validate_urls(&self) -> Result<(), ValidationError> {
        let mut plaintext = false;
        for (index, raw) in self.urls.iter().enumerate() {
            // The key names the entry; a URL is never echoed.
            let key = format!("messaging.urls[{index}]");
            non_blank(&key, raw)?;
            if shared_rules::uri::has_whitespace_or_control(raw) {
                return Err(ValidationError::new(
                    key,
                    "must not contain whitespace or controls",
                ));
            }
            let url = Url::parse(raw)
                .map_err(|_| ValidationError::new(key.clone(), "must be a valid NATS URL"))?;
            if url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || shared_rules::uri::authority_has_userinfo(raw)
            {
                return Err(ValidationError::new(
                    key,
                    "must not contain userinfo and must name a host",
                ));
            }
            match url.scheme() {
                "tls" => {}
                "nats" => plaintext = true,
                _ => return Err(ValidationError::new(key, "must use nats:// or tls://")),
            }
        }
        if plaintext && self.tls_first {
            return Err(ValidationError::new(
                "messaging.tls_first",
                "requires every configured URL to use tls://",
            ));
        }
        if plaintext && !self.plaintext_admitted() {
            return Err(ValidationError::new(
                "messaging.allow_plaintext",
                "must be true for nats:// URLs in local/development; set messaging.trusted_network = true elsewhere",
            ));
        }

        Ok(())
    }
}

/// One admitted NATS credential source.
#[derive(Clone, Copy, Debug)]
pub enum MessagingCredentials<'a> {
    /// The environment-only `messaging.credentials` value.
    Inline(&'a SecretString),
    /// The file `messaging.credentials_file` names.
    File(&'a Path),
}

/// The consumer keys [`MessagingConfig::required_consumer`] admitted.
#[derive(Clone, Copy, Debug)]
pub struct ConsumerKeys<'a> {
    pub durable: &'a str,
    pub filter_subject: &'a str,
    pub dlq_subject: &'a str,
}

/// A TOML list, or one comma-separated `APP__MESSAGING__URLS` value. Each
/// part keeps its exact bytes; validation rejects an empty one.
fn url_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Text(String),
        List(Vec<String>),
    }

    let input = Input::deserialize(deserializer)
        .map_err(|_| value_free("must be a list of strings or a comma-separated string"))?;
    Ok(match input {
        Input::Text(value) => value.split(',').map(ToOwned::to_owned).collect(),
        Input::List(urls) => urls,
    })
}

#[cfg(test)]
mod tests;
