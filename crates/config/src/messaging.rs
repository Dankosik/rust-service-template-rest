//! JetStream connection and delivery-admission configuration.
//!
//! This section owns startup input only. It never contacts a broker or
//! reconciles operator-owned streams.

use std::path::PathBuf;

use bytesize::ByteSize;
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::app::occupied_string;
use crate::secret_policy::occupied_secret;
use crate::validate::{ValidationError, int_range, non_empty};

const DELIVERY_OVERHEAD_BYTES: u64 = 8 * 1024;
const MAX_RESIDENT_DELIVERY_BYTES: u64 = 64 * 1024 * 1024;

/// Optional JetStream connection and worker limits.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MessagingConfig {
    /// NATS server URLs. An empty list keeps messaging inactive in the API.
    pub urls: Vec<String>,
    /// Inline NATS credentials. This environment-only value is redacted.
    #[serde(default, deserialize_with = "occupied_secret")]
    pub credentials: Option<SecretString>,
    /// Optional PEM root CA path for the operator-selected broker.
    pub root_ca_path: Option<PathBuf>,
    /// Permit `nats://` only for a local or development process.
    pub allow_plaintext: bool,
    /// Permit a connection without inline credentials only for a local or
    /// development process.
    pub allow_unauthenticated: bool,
    /// Operator-owned source stream for publication and consumption.
    #[serde(default, deserialize_with = "occupied_string")]
    pub source_stream: Option<String>,
    /// Largest payload admitted before handler allocation.
    pub max_payload_bytes: ByteSize,
    /// Durable consumer name, required only by a consuming worker.
    #[serde(default, deserialize_with = "occupied_string")]
    pub consumer_durable: Option<String>,
    /// Filter subject, required only by a consuming worker.
    #[serde(default, deserialize_with = "occupied_string")]
    pub consumer_filter_subject: Option<String>,
    /// DLQ subject, required only by a consuming worker.
    #[serde(default, deserialize_with = "occupied_string")]
    pub dlq_subject: Option<String>,
    /// Maximum deliveries held through handling and settlement.
    pub consumer_concurrency: u32,
}

impl Default for MessagingConfig {
    fn default() -> Self {
        Self {
            urls: Vec::new(),
            credentials: None,
            root_ca_path: None,
            allow_plaintext: false,
            allow_unauthenticated: false,
            source_stream: None,
            max_payload_bytes: ByteSize::mib(1),
            consumer_durable: None,
            consumer_filter_subject: None,
            dlq_subject: None,
            consumer_concurrency: 1,
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
        required(
            "messaging.source_stream",
            &self.source_stream,
            "an active producer",
        )?;
        Ok(())
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
            &self.consumer_durable,
            "an active consumer",
        )?;
        required(
            "messaging.consumer_filter_subject",
            &self.consumer_filter_subject,
            "an active consumer",
        )?;
        required(
            "messaging.dlq_subject",
            &self.dlq_subject,
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
        if self
            .root_ca_path
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(ValidationError::new(
                "messaging.root_ca_path",
                "cannot be empty when set",
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
        int_range(
            "messaging.consumer_concurrency",
            u64::from(self.consumer_concurrency),
            1,
            u64::from(u32::MAX),
        )?;
        let resident = u64::from(self.consumer_concurrency)
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
        if self.credentials.is_none() && !self.allow_unauthenticated {
            return Err(ValidationError::new(
                "messaging.credentials",
                "is required unless messaging.allow_unauthenticated = true",
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
    value: &'a Option<String>,
    requirement: &str,
) -> Result<&'a str, ValidationError> {
    value
        .as_deref()
        .ok_or_else(|| ValidationError::new(key, format!("is required for {requirement}")))
}

fn is_local_development(app_env: &str) -> bool {
    matches!(app_env, "local" | "development")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "focused configuration tests use unwrap only after the asserted contract succeeds"
)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_inactive_and_within_the_delivery_budget() {
        let config = MessagingConfig::default();
        assert!(!config.is_active());
        assert_eq!(config.max_payload_bytes, ByteSize::mib(1));
        assert_eq!(config.consumer_concurrency, 1);
        config.validate("production").unwrap();
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
            consumer_concurrency: 64,
            ..MessagingConfig::default()
        };
        assert_eq!(
            config.validate("production").unwrap_err().key,
            "messaging.consumer_concurrency"
        );
    }

    #[test]
    fn rejects_an_empty_root_ca_path() {
        let config = MessagingConfig {
            root_ca_path: Some(PathBuf::new()),
            ..MessagingConfig::default()
        };
        assert_eq!(
            config.validate("production").unwrap_err().key,
            "messaging.root_ca_path"
        );
    }
}
