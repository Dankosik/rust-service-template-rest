//! `JetStream` connection and delivery-admission configuration.
//!
//! This section owns startup input only. It never contacts a broker or
//! reconciles operator-owned streams.

use std::num::NonZeroU32;
use std::path::PathBuf;

use bytesize::ByteSize;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::app::is_local_development;
use crate::de::{VALUE_FREE, blank_as_none, blank_secret_as_none};
use crate::validate::{ValidationError, int_range, non_empty};

const DELIVERY_OVERHEAD_BYTES: u64 = 8 * 1024;
const MAX_RESIDENT_DELIVERY_BYTES: u64 = 64 * 1024 * 1024;

/// Optional `JetStream` connection and worker limits.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MessagingConfig {
    /// NATS server URLs. An empty list keeps messaging inactive in the API.
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
    /// Whether the API has a selected broker destination.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.urls.is_empty()
    }

    /// Validate the inputs required by a producer or outbox publisher.
    ///
    /// # Errors
    ///
    /// Returns the first missing producer input or a security-policy error.
    pub fn validate_producer(&self, app_env: &str) -> Result<(), ValidationError> {
        self.validate(app_env)?;
        if !self.is_active() {
            return Err(ValidationError::new(
                "messaging.urls",
                "is required for an active producer",
            ));
        }
        self.required_credentials(app_env)?;
        self.required_source_stream()?;
        Ok(())
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
            "an active producer",
        )
    }

    /// Validate the inputs required by a consuming worker.
    ///
    /// # Errors
    ///
    /// Returns the first missing consumer input or a producer-policy error.
    pub fn validate_consumer(&self, app_env: &str) -> Result<(), ValidationError> {
        self.validate_producer(app_env)?;
        required(
            "messaging.consumer_durable",
            self.consumer_durable.as_deref(),
            "an active consumer",
        )?;
        required(
            "messaging.consumer_filter_subject",
            self.consumer_filter_subject.as_deref(),
            "an active consumer",
        )?;
        required(
            "messaging.dlq_subject",
            self.dlq_subject.as_deref(),
            "an active consumer",
        )?;
        Ok(())
    }

    pub(crate) fn validate(&self, app_env: &str) -> Result<(), ValidationError> {
        let local_development = is_local_development(app_env);
        if self.allow_plaintext && !local_development {
            return Err(ValidationError::new(
                "messaging.allow_plaintext",
                "is local/development-only",
            ));
        }
        if self.allow_unauthenticated && !local_development {
            return Err(ValidationError::new(
                "messaging.allow_unauthenticated",
                "is local/development-only",
            ));
        }
        if self.credentials.is_some() && self.credentials_file.is_some() {
            return Err(ValidationError::new(
                "messaging.credentials_file",
                "cannot be set together with messaging.credentials",
            ));
        }
        let mut plaintext = false;
        for raw in &self.urls {
            non_empty("messaging.urls", raw)?;
            let url = Url::parse(raw).map_err(|_| {
                ValidationError::new("messaging.urls", "must contain valid NATS URLs")
            })?;
            if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
                return Err(ValidationError::new(
                    "messaging.urls",
                    "must not contain userinfo and must name a host",
                ));
            }
            match url.scheme() {
                "tls" => {}
                "nats" => plaintext = true,
                _ => {
                    return Err(ValidationError::new(
                        "messaging.urls",
                        "must use nats:// or tls://",
                    ));
                }
            }
        }
        if plaintext && !self.allow_plaintext {
            return Err(ValidationError::new(
                "messaging.allow_plaintext",
                "must be true for nats:// URLs",
            ));
        }

        int_range(
            "messaging.max_payload_bytes",
            self.max_payload_bytes.as_u64(),
            1,
            MAX_RESIDENT_DELIVERY_BYTES - DELIVERY_OVERHEAD_BYTES,
        )?;
        let resident = u64::from(self.consumer_concurrency.get())
            .checked_mul(
                self.max_payload_bytes
                    .as_u64()
                    .saturating_add(DELIVERY_OVERHEAD_BYTES),
            )
            .ok_or_else(|| {
                ValidationError::new(
                    "messaging.consumer_concurrency",
                    "with messaging.max_payload_bytes exceeds the 64 MiB resident delivery bound",
                )
            })?;
        if resident > MAX_RESIDENT_DELIVERY_BYTES {
            return Err(ValidationError::new(
                "messaging.consumer_concurrency",
                "with messaging.max_payload_bytes exceeds the 64 MiB resident delivery bound",
            ));
        }
        Ok(())
    }

    fn required_credentials(&self, app_env: &str) -> Result<(), ValidationError> {
        if self.credentials.is_none()
            && self.credentials_file.is_none()
            && !self.allow_unauthenticated
        {
            return Err(ValidationError::new(
                "messaging.credentials",
                "or messaging.credentials_file is required unless messaging.allow_unauthenticated = true",
            ));
        }
        if self.allow_unauthenticated && !is_local_development(app_env) {
            return Err(ValidationError::new(
                "messaging.allow_unauthenticated",
                "is local/development-only",
            ));
        }
        Ok(())
    }
}

fn required<'a>(
    key: &str,
    value: Option<&'a str>,
    requirement: &str,
) -> Result<&'a str, ValidationError> {
    value.ok_or_else(|| ValidationError::new(key, format!("is required for {requirement}")))
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

    let input = Input::deserialize(deserializer).map_err(|_| {
        serde::de::Error::custom(format_args!(
            "{VALUE_FREE}must be a list of strings or a comma-separated string"
        ))
    })?;
    Ok(match input {
        Input::Text(value) => value.split(',').map(ToOwned::to_owned).collect(),
        Input::List(urls) => urls,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "focused configuration tests use unwrap only after the asserted contract succeeds"
)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    #[test]
    fn defaults_are_inactive_and_within_the_delivery_budget() {
        let config = MessagingConfig::default();
        assert!(!config.is_active());
        assert_eq!(config.max_payload_bytes, ByteSize::kib(256));
        assert_eq!(config.consumer_concurrency, NonZeroU32::MIN);
        config.validate("production").unwrap();
    }

    #[test]
    fn default_delivery_fits_a_broker_with_the_default_payload_limit() {
        // nats-server's default `max_payload`, which bounds payload and
        // headers together.
        const BROKER_DEFAULT_MAX_PAYLOAD: u64 = 1024 * 1024;
        let config = MessagingConfig::default();
        assert!(
            config.max_payload_bytes.as_u64() + DELIVERY_OVERHEAD_BYTES
                <= BROKER_DEFAULT_MAX_PAYLOAD
        );
    }

    #[test]
    fn a_credentials_file_is_the_alternative_to_inline_credentials() {
        let mut config = MessagingConfig {
            urls: vec!["tls://nats.example:4222".to_owned()],
            credentials_file: Some(PathBuf::from("/run/secrets/nats.creds")),
            source_stream: Some("events".to_owned()),
            ..MessagingConfig::default()
        };
        config.validate_producer("production").unwrap();

        config.credentials = Some(SecretString::from("fixture-credentials".to_owned()));
        assert_eq!(
            config.validate_producer("production").unwrap_err().key,
            "messaging.credentials_file"
        );
    }

    #[test]
    fn producer_requires_admitted_connection_credentials_and_source() {
        let mut config = MessagingConfig {
            urls: vec!["tls://nats.example:4222".to_owned()],
            ..MessagingConfig::default()
        };
        assert_eq!(
            config.validate_producer("production").unwrap_err().key,
            "messaging.credentials"
        );
        config.credentials = Some(SecretString::from("fixture-credentials".to_owned()));
        assert_eq!(
            config.validate_producer("production").unwrap_err().key,
            "messaging.source_stream"
        );
        config.source_stream = Some("events".to_owned());
        config.validate_producer("production").unwrap();
        assert!(!format!("{config:?}").contains("fixture-credentials"));
    }

    #[test]
    fn consumer_requires_its_distinct_operator_owned_names() {
        let config = MessagingConfig {
            urls: vec!["tls://nats.example:4222".to_owned()],
            credentials: Some(SecretString::from("fixture-credentials".to_owned())),
            source_stream: Some("events".to_owned()),
            ..MessagingConfig::default()
        };
        assert_eq!(
            config.validate_consumer("production").unwrap_err().key,
            "messaging.consumer_durable"
        );
    }

    #[test]
    fn rejects_url_userinfo_unknown_protocol_and_unapproved_plaintext() {
        for (url, key) in [
            ("tls://user:password@nats.example:4222", "messaging.urls"),
            ("https://nats.example:4222", "messaging.urls"),
            ("nats://nats.example:4222", "messaging.allow_plaintext"),
        ] {
            let config = MessagingConfig {
                urls: vec![url.to_owned()],
                ..MessagingConfig::default()
            };
            assert_eq!(config.validate("local").unwrap_err().key, key, "{url}");
        }
    }

    #[test]
    fn plaintext_and_unauthenticated_are_only_available_in_local_development() {
        let config = MessagingConfig {
            urls: vec!["nats://127.0.0.1:4222".to_owned()],
            allow_plaintext: true,
            allow_unauthenticated: true,
            source_stream: Some("events".to_owned()),
            ..MessagingConfig::default()
        };
        config.validate_producer("local").unwrap();
        assert_eq!(
            config.validate("production").unwrap_err().key,
            "messaging.allow_plaintext"
        );
    }

    #[test]
    fn resident_delivery_memory_is_bounded() {
        let config = MessagingConfig {
            max_payload_bytes: ByteSize::mib(1),
            consumer_concurrency: NonZeroU32::new(64).unwrap(),
            ..MessagingConfig::default()
        };
        assert_eq!(
            config.validate("production").unwrap_err().key,
            "messaging.consumer_concurrency"
        );
    }
}
