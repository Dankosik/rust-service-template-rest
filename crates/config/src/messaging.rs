use std::num::NonZeroU32;
use std::path::PathBuf;

use bytesize::{ByteSize, KIB, MIB};
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::app::local_development_only;
use crate::de::{StrictString, blank_as_none, blank_secret_as_none};
use crate::validate::{
    Rejected, ValidationError, Violations, int_range, missing, non_blank, required,
};

const DELIVERY_OVERHEAD_BYTES: u64 = 8 * KIB;
const MAX_RESIDENT_DELIVERY_MIB: u64 = 64;
const MAX_RESIDENT_DELIVERY_BYTES: u64 = MAX_RESIDENT_DELIVERY_MIB * MIB;

const PRODUCER_ACTIVE: &str = "a producer is active";
const CONSUMER_ACTIVE: &str = "a consumer is active";

const DEFAULT_MAX_PAYLOAD: ByteSize = ByteSize::kib(256);

#[expect(
    clippy::struct_excessive_bools,
    reason = "NATS trust, authentication and TLS-first are independent operator capabilities"
)]
#[derive(derive_more::Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct MessagingInput {
    #[serde(deserialize_with = "url_list")]
    #[debug("{} servers", urls.len())]
    urls: Vec<String>,
    #[serde(default, deserialize_with = "blank_secret_as_none")]
    credentials: Option<SecretString>,
    /// Path to a NATS credentials file, the alternative to `credentials` for
    /// a platform that rotates it: the client reads the file again for every
    /// connection. Its target must be a regular file of at most 1 MiB.
    /// Missing, empty, or whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    credentials_file: Option<PathBuf>,
    /// Optional PEM root CA path for the operator-selected broker. Missing,
    /// empty, or whitespace-only is unset (`None`).
    #[serde(default, deserialize_with = "blank_as_none")]
    root_ca_path: Option<PathBuf>,
    /// Permit `nats://` with `app.allow_insecure_development`.
    #[serde(deserialize_with = "crate::de::boolean")]
    allow_plaintext: bool,
    /// Perform TLS before INFO; every configured URL must use `tls://`.
    #[serde(deserialize_with = "crate::de::boolean")]
    tls_first: bool,
    /// The operator declares the private network the broker trust boundary,
    /// as `grpc.security = "plaintext"` does for gRPC: `nats://` is admitted
    /// in every environment. Credentials stay required unless the separate
    /// development exception for unauthenticated connections is enabled.
    #[serde(deserialize_with = "crate::de::boolean")]
    trusted_network: bool,
    /// Permit a connection without credentials with `app.allow_insecure_development`.
    #[serde(deserialize_with = "crate::de::boolean")]
    allow_unauthenticated: bool,
    #[serde(default, deserialize_with = "blank_as_none")]
    source_stream: Option<String>,
    max_payload_bytes: ByteSize,
    #[serde(default, deserialize_with = "blank_as_none")]
    consumer_durable: Option<String>,
    #[serde(default, deserialize_with = "blank_as_none")]
    consumer_filter_subject: Option<String>,
    #[serde(default, deserialize_with = "blank_as_none")]
    dlq_subject: Option<String>,
    #[serde(deserialize_with = "crate::de::unsigned_count")]
    consumer_concurrency: NonZeroU32,
}

impl Default for MessagingInput {
    fn default() -> Self {
        let MessagingConfig {
            urls,
            credentials: _,
            root_ca_path,
            allow_plaintext,
            tls_first,
            trusted_network,
            allow_unauthenticated,
            source_stream,
            max_payload_bytes,
            consumer_durable,
            consumer_filter_subject,
            dlq_subject,
            consumer_concurrency,
        } = MessagingConfig::default();
        Self {
            urls,
            credentials: None,
            credentials_file: None,
            root_ca_path,
            allow_plaintext,
            tls_first,
            trusted_network,
            allow_unauthenticated,
            source_stream,
            max_payload_bytes,
            consumer_durable,
            consumer_filter_subject,
            dlq_subject,
            consumer_concurrency,
        }
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "NATS trust, authentication and TLS-first are independent operator capabilities"
)]
#[derive(Clone, derive_more::Debug)]
pub struct MessagingConfig {
    #[debug("{} servers", urls.len())]
    pub urls: Vec<String>,
    pub credentials: Option<MessagingCredentials>,
    pub root_ca_path: Option<PathBuf>,
    pub allow_plaintext: bool,
    pub tls_first: bool,
    pub trusted_network: bool,
    pub allow_unauthenticated: bool,
    pub source_stream: Option<String>,
    pub max_payload_bytes: ByteSize,
    pub consumer_durable: Option<String>,
    pub consumer_filter_subject: Option<String>,
    pub dlq_subject: Option<String>,
    pub consumer_concurrency: NonZeroU32,
}

impl Default for MessagingConfig {
    fn default() -> Self {
        Self {
            urls: Vec::new(),
            credentials: None,
            root_ca_path: None,
            allow_plaintext: false,
            tls_first: false,
            trusted_network: false,
            allow_unauthenticated: false,
            source_stream: None,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD,
            consumer_durable: None,
            consumer_filter_subject: None,
            dlq_subject: None,
            consumer_concurrency: NonZeroU32::MIN,
        }
    }
}

impl MessagingInput {
    pub(crate) fn admit(
        self,
        insecure_development: bool,
        v: &mut Violations,
    ) -> Result<MessagingConfig, Rejected> {
        let plaintext = v.take(local_development_only(
            "messaging.allow_plaintext",
            self.allow_plaintext,
            insecure_development,
        ));
        let unauthenticated = v.take(local_development_only(
            "messaging.allow_unauthenticated",
            self.allow_unauthenticated,
            insecure_development,
        ));
        let credentials = match (self.credentials, self.credentials_file) {
            (Some(_), Some(_)) => Err(v.reject(ValidationError::new(
                "messaging.credentials_file",
                "cannot be set together with messaging.credentials",
            ))),
            (Some(inline), None) => Ok(Some(MessagingCredentials::Inline(inline))),
            (None, Some(file)) => Ok(Some(MessagingCredentials::File(file))),
            (None, None) => Ok(None),
        };
        let urls = validate_urls(
            &self.urls,
            self.tls_first,
            self.allow_plaintext || self.trusted_network,
            v,
        );
        let payload = v.take(int_range(
            "messaging.max_payload_bytes",
            self.max_payload_bytes.as_u64(),
            1..=MAX_RESIDENT_DELIVERY_BYTES - DELIVERY_OVERHEAD_BYTES,
        ));
        let concurrency = payload.and_then(|()| {
            v.take(resident_delivery_bound(
                self.max_payload_bytes,
                self.consumer_concurrency,
            ))
        });
        plaintext?;
        unauthenticated?;
        urls?;
        concurrency?;
        Ok(MessagingConfig {
            urls: self.urls,
            credentials: credentials?,
            root_ca_path: self.root_ca_path,
            allow_plaintext: self.allow_plaintext,
            tls_first: self.tls_first,
            trusted_network: self.trusted_network,
            allow_unauthenticated: self.allow_unauthenticated,
            source_stream: self.source_stream,
            max_payload_bytes: self.max_payload_bytes,
            consumer_durable: self.consumer_durable,
            consumer_filter_subject: self.consumer_filter_subject,
            dlq_subject: self.dlq_subject,
            consumer_concurrency: self.consumer_concurrency,
        })
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

    pub fn producer_source_stream(&self) -> Result<&str, ValidationError> {
        if !self.is_active() {
            return Err(missing("messaging.urls", PRODUCER_ACTIVE));
        }
        if self.credentials.is_none() && !self.allow_unauthenticated {
            return Err(ValidationError::new(
                "messaging.credentials",
                "one of messaging.credentials or messaging.credentials_file is required unless messaging.allow_unauthenticated = true",
            ));
        }
        required(
            "messaging.source_stream",
            self.source_stream.as_deref(),
            PRODUCER_ACTIVE,
        )
    }

    pub fn consumer_keys(&self) -> Result<ConsumerKeys<'_>, ValidationError> {
        Ok(ConsumerKeys {
            source_stream: self.producer_source_stream()?,
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
}

fn resident_delivery_bound(
    max_payload_bytes: ByteSize,
    consumer_concurrency: NonZeroU32,
) -> Result<(), ValidationError> {
    let resident = u64::from(consumer_concurrency.get())
        * (max_payload_bytes.as_u64() + DELIVERY_OVERHEAD_BYTES);
    if resident > MAX_RESIDENT_DELIVERY_BYTES {
        return Err(ValidationError::new(
            "messaging.consumer_concurrency",
            format!(
                "{consumer_concurrency} × (messaging.max_payload_bytes {} + {} KiB headers) = {resident} bytes exceeds the {MAX_RESIDENT_DELIVERY_MIB} MiB resident delivery bound",
                max_payload_bytes.as_u64(),
                DELIVERY_OVERHEAD_BYTES / KIB,
            ),
        ));
    }
    Ok(())
}

fn validate_urls(
    urls: &[String],
    tls_first: bool,
    plaintext_admitted: bool,
    v: &mut Violations,
) -> Result<(), Rejected> {
    let mut plaintext = false;
    let mut rejected = Ok(());
    for (index, raw) in urls.iter().enumerate() {
        match url_scheme(index, raw) {
            Ok(scheme) => plaintext |= scheme == "nats",
            Err(error) => rejected = Err(v.reject(error)),
        }
    }
    rejected?;
    if plaintext && tls_first {
        return Err(v.reject(ValidationError::new(
            "messaging.tls_first",
            "requires every configured URL to use tls://",
        )));
    }
    if plaintext && !plaintext_admitted {
        return Err(v.reject(ValidationError::new(
            "messaging.allow_plaintext",
            "must be true for nats:// URLs in local/development; set messaging.trusted_network = true elsewhere",
        )));
    }
    Ok(())
}

fn url_scheme(index: usize, raw: &str) -> Result<String, ValidationError> {
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
        scheme @ ("tls" | "nats") => Ok(scheme.to_owned()),
        _ => Err(ValidationError::new(key, "must use nats:// or tls://")),
    }
}

#[derive(Clone, Debug)]
pub enum MessagingCredentials {
    Inline(SecretString),
    File(PathBuf),
}

#[derive(Clone, Copy, Debug)]
pub struct ConsumerKeys<'a> {
    pub source_stream: &'a str,
    pub durable: &'a str,
    pub filter_subject: &'a str,
    pub dlq_subject: &'a str,
}

fn url_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Input {
        Text(StrictString),
        List(Vec<StrictString>),
    }

    let input = Input::deserialize(deserializer).map_err(|_| {
        serde::de::Error::invalid_value(
            serde::de::Unexpected::Other("a non-string value"),
            &"a list of strings or a comma-separated string",
        )
    })?;
    Ok(match input {
        Input::Text(value) => value.0.split(',').map(ToOwned::to_owned).collect(),
        Input::List(urls) => urls.into_iter().map(|url| url.0).collect(),
    })
}

#[cfg(test)]
mod tests;
