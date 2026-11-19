use std::num::NonZeroU32;
use std::path::PathBuf;

use bytesize::{ByteSize, KIB, MIB};
use secrecy::SecretString;
use serde::Deserialize;
use url::Url;

use crate::app::local_development_only;
use crate::de::{BlankAsNone, BlankAsNoneVerbatim, StrictBool, StrictCount, StrictString};
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
#[serde_with::serde_as]
#[derive(derive_more::Debug, Deserialize, educe::Educe)]
#[educe(Default)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct MessagingInput {
    #[serde_as(as = "UrlList")]
    #[debug("{} servers", urls.len())]
    urls: Vec<String>,
    #[serde(default)]
    #[serde_as(as = "BlankAsNoneVerbatim")]
    credentials: Option<SecretString>,
    /// Path to a NATS credentials file, the alternative to `credentials` for
    /// a platform that rotates it: the client reads the file again for every
    /// connection. Its target must be a regular file of at most 1 MiB.
    /// Missing, empty, or whitespace-only is unset (`None`).
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    credentials_file: Option<PathBuf>,
    /// Optional PEM root CA path for the operator-selected broker. Missing,
    /// empty, or whitespace-only is unset (`None`).
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    root_ca_path: Option<PathBuf>,
    /// Permit `nats://` with `app.allow_insecure_development`.
    #[serde_as(as = "StrictBool")]
    allow_plaintext: bool,
    /// Perform TLS before INFO; every configured URL must use `tls://`.
    #[serde_as(as = "StrictBool")]
    tls_first: bool,
    /// The operator declares the private network the broker trust boundary,
    /// as `grpc.security = "plaintext"` does for gRPC: `nats://` is admitted
    /// in every environment. Credentials stay required unless the separate
    /// development exception for unauthenticated connections is enabled.
    #[serde_as(as = "StrictBool")]
    trusted_network: bool,
    /// Permit a connection without credentials with `app.allow_insecure_development`.
    #[serde_as(as = "StrictBool")]
    allow_unauthenticated: bool,
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    source_stream: Option<String>,
    #[educe(Default(expression = DEFAULT_MAX_PAYLOAD))]
    max_payload_bytes: ByteSize,
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    consumer_durable: Option<String>,
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    consumer_filter_subject: Option<String>,
    #[serde(default)]
    #[serde_as(as = "BlankAsNone")]
    dlq_subject: Option<String>,
    #[serde_as(as = "StrictCount")]
    #[educe(Default(expression = NonZeroU32::MIN))]
    consumer_concurrency: NonZeroU32,
}

#[derive(Clone, derive_more::Debug)]
pub struct MessagingConfig {
    #[debug("{} servers", urls.len())]
    pub urls: Vec<String>,
    pub credentials: Option<MessagingCredentials>,
    pub root_ca_path: Option<PathBuf>,
    pub plaintext_admitted: bool,
    pub tls_first: bool,
    pub source_stream: Option<String>,
    pub max_payload_bytes: ByteSize,
    pub consumer_durable: Option<String>,
    pub consumer_filter_subject: Option<String>,
    pub dlq_subject: Option<String>,
    pub consumer_concurrency: NonZeroU32,
}

impl MessagingInput {
    pub(crate) fn admit(
        self,
        local_development: bool,
        v: &mut Violations,
    ) -> Result<MessagingConfig, Rejected> {
        v.check(local_development_only(
            "messaging.allow_plaintext",
            self.allow_plaintext,
            local_development,
        ));
        v.check(local_development_only(
            "messaging.allow_unauthenticated",
            self.allow_unauthenticated,
            local_development,
        ));
        let credentials = match (self.credentials, self.credentials_file) {
            (Some(_), Some(_)) => Err(v.reject(ValidationError::new(
                "messaging.credentials_file",
                "cannot be set together with messaging.credentials",
            ))),
            (Some(inline), None) => Ok(Some(MessagingCredentials::Inline(inline))),
            (None, Some(file)) => Ok(Some(MessagingCredentials::File(file))),
            (None, None) => Ok(self
                .allow_unauthenticated
                .then_some(MessagingCredentials::Unauthenticated)),
        };
        let plaintext_admitted = self.allow_plaintext || self.trusted_network;
        validate_urls(&self.urls, self.tls_first, plaintext_admitted, v);
        v.check(
            int_range(
                "messaging.max_payload_bytes",
                self.max_payload_bytes.as_u64(),
                1..=MAX_RESIDENT_DELIVERY_BYTES - DELIVERY_OVERHEAD_BYTES,
            )
            .and_then(|()| {
                validate_resident_delivery_bound(self.max_payload_bytes, self.consumer_concurrency)
            }),
        );
        Ok(MessagingConfig {
            urls: self.urls,
            credentials: credentials?,
            root_ca_path: self.root_ca_path,
            plaintext_admitted,
            tls_first: self.tls_first,
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
    pub fn producer_source_stream(&self) -> Result<&str, ValidationError> {
        if self.urls.is_empty() {
            return Err(missing("messaging.urls", PRODUCER_ACTIVE));
        }
        if self.credentials.is_none() {
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

fn validate_resident_delivery_bound(
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

enum BrokerScheme {
    Plaintext,
    Tls,
}

fn validate_urls(urls: &[String], tls_first: bool, plaintext_admitted: bool, v: &mut Violations) {
    let mut plaintext = false;
    let mut all_valid = true;
    for (index, raw) in urls.iter().enumerate() {
        match validate_broker_url(index, raw) {
            Ok(BrokerScheme::Plaintext) => plaintext = true,
            Ok(BrokerScheme::Tls) => {}
            Err(error) => {
                v.reject(error);
                all_valid = false;
            }
        }
    }
    if !all_valid || !plaintext {
        return;
    }
    if tls_first {
        v.reject(ValidationError::new(
            "messaging.tls_first",
            "requires every configured URL to use tls://",
        ));
    } else if !plaintext_admitted {
        v.reject(ValidationError::new(
            "messaging.allow_plaintext",
            "must be true for nats:// URLs in local/development; set messaging.trusted_network = true elsewhere",
        ));
    }
}

fn validate_broker_url(index: usize, raw: &str) -> Result<BrokerScheme, ValidationError> {
    let key = format!("messaging.urls[{index}]");
    non_blank(&key, raw)?;
    if shared_rules::uri::has_whitespace_or_control(raw) {
        return Err(ValidationError::new(
            key,
            "must not contain whitespace or controls",
        ));
    }
    let Ok(url) = Url::parse(raw) else {
        return Err(ValidationError::new(key, "must be a valid NATS URL"));
    };
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
        "nats" => Ok(BrokerScheme::Plaintext),
        "tls" => Ok(BrokerScheme::Tls),
        _ => Err(ValidationError::new(key, "must use nats:// or tls://")),
    }
}

#[derive(Clone, Debug)]
pub enum MessagingCredentials {
    Inline(SecretString),
    File(PathBuf),
    Unauthenticated,
}

#[derive(Clone, Copy, Debug)]
pub struct ConsumerKeys<'a> {
    pub source_stream: &'a str,
    pub durable: &'a str,
    pub filter_subject: &'a str,
    pub dlq_subject: &'a str,
}

struct UrlList;

impl<'de> serde_with::DeserializeAs<'de, Vec<String>> for UrlList {
    fn deserialize_as<D>(deserializer: D) -> Result<Vec<String>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[serde_with::serde_as]
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Text(#[serde_as(as = "StrictString")] String),
            List(#[serde_as(as = "Vec<StrictString>")] Vec<String>),
        }

        let input = Input::deserialize(deserializer).map_err(|_| {
            serde::de::Error::invalid_value(
                serde::de::Unexpected::Other("a non-string value"),
                &"a list of strings or a comma-separated string",
            )
        })?;
        Ok(match input {
            Input::Text(value) => value.split(',').map(ToOwned::to_owned).collect(),
            Input::List(urls) => urls,
        })
    }
}

#[cfg(test)]
mod tests;
