use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use temporalio_common::data_converters::{
    DataConverter, DefaultFailureConverter, DefaultPayloadCodec, ErasedSerdePayloadConverter,
    PayloadCodec, PayloadConversionError, PayloadConverter, SerializationContextData,
};
use temporalio_common::protos::temporal::api::common::v1::Payload;
use temporalio_common::protos::{ENCODING_PAYLOAD_KEY, JSON_ENCODING_VAL};

const BINARY_NULL_ENCODING_VAL: &str = "binary/null";

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    #[error("payload JSON serialization failed")]
    Serialize,
    #[error("payload JSON deserialization failed")]
    Decode,
    #[error("payload exceeds maximum byte budget")]
    TooLarge { bytes: usize },
    #[error("invalid payload metadata")]
    InvalidEncoding,
}

#[derive(Debug)]
struct DecodePayloadError(PayloadError);

impl fmt::Display for DecodePayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for DecodePayloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[derive(Debug)]
struct EncodeCodecError;

impl fmt::Display for EncodeCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("payload codec encode failed")
    }
}

impl std::error::Error for EncodeCodecError {}

#[must_use]
pub(crate) fn encode_codec_error() -> PayloadConversionError {
    PayloadConversionError::EncodingError(Box::new(EncodeCodecError))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClassifiedPayloadError {
    Encode(PayloadError),
    EncodeCodec,
    Decode(PayloadError),
    Unknown,
}

pub(crate) struct FiniteBoundedJsonConverter {
    max_payload_bytes: usize,
}

impl FiniteBoundedJsonConverter {
    #[must_use]
    pub(crate) const fn new(max_payload_bytes: usize) -> Self {
        Self { max_payload_bytes }
    }
}

impl ErasedSerdePayloadConverter for FiniteBoundedJsonConverter {
    fn to_payload(
        &self,
        _context: &SerializationContextData,
        value: &dyn erased_serde::Serialize,
    ) -> Result<Payload, PayloadConversionError> {
        let encoded = match json_payload::to_vec_bounded(value, self.max_payload_bytes) {
            Ok(bytes) => bytes,
            Err(json_payload::EncodeError::TooLarge { bytes }) => {
                return Err(PayloadConversionError::EncodingError(Box::new(
                    PayloadError::TooLarge { bytes },
                )));
            }
            Err(json_payload::EncodeError::Serialize(_)) => {
                return Err(PayloadConversionError::EncodingError(Box::new(
                    PayloadError::Serialize,
                )));
            }
        };
        if encoded.as_slice() == b"null" {
            return Ok(Payload {
                metadata: HashMap::from([(
                    ENCODING_PAYLOAD_KEY.to_owned(),
                    BINARY_NULL_ENCODING_VAL.as_bytes().to_vec(),
                )]),
                data: Vec::new(),
                external_payloads: Vec::new(),
            });
        }
        Ok(Payload {
            metadata: HashMap::from([(
                ENCODING_PAYLOAD_KEY.to_owned(),
                JSON_ENCODING_VAL.as_bytes().to_vec(),
            )]),
            data: encoded,
            external_payloads: Vec::new(),
        })
    }

    fn from_payload(
        &self,
        _context: &SerializationContextData,
        payload: Payload,
    ) -> Result<Box<dyn erased_serde::Deserializer<'static>>, PayloadConversionError> {
        let encoding = payload
            .metadata
            .get(ENCODING_PAYLOAD_KEY)
            .map(Vec::as_slice);
        let value = if encoding == Some(JSON_ENCODING_VAL.as_bytes()) {
            if payload.data.len() > self.max_payload_bytes {
                return Err(PayloadConversionError::EncodingError(Box::new(
                    DecodePayloadError(PayloadError::TooLarge {
                        bytes: payload.data.len(),
                    }),
                )));
            }
            serde_json::from_slice::<serde_json::Value>(&payload.data).map_err(|_| {
                PayloadConversionError::EncodingError(Box::new(DecodePayloadError(
                    PayloadError::Decode,
                )))
            })?
        } else if encoding == Some(BINARY_NULL_ENCODING_VAL.as_bytes()) {
            if !payload.data.is_empty() {
                return Err(PayloadConversionError::EncodingError(Box::new(
                    DecodePayloadError(PayloadError::InvalidEncoding),
                )));
            }
            serde_json::Value::Null
        } else {
            return Err(PayloadConversionError::EncodingError(Box::new(
                DecodePayloadError(PayloadError::InvalidEncoding),
            )));
        };
        Ok(Box::new(<dyn erased_serde::Deserializer<'_>>::erase(value)))
    }
}

struct SharedPayloadCodec(Arc<dyn PayloadCodec + Send + Sync>);

impl PayloadCodec for SharedPayloadCodec {
    fn encode(
        &self,
        context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        let future = self.0.encode(context, payloads);
        Box::pin(async move {
            future
                .await
                .map_err(|error| match classify_payload_error(&error) {
                    ClassifiedPayloadError::Encode(PayloadError::TooLarge { bytes }) => {
                        PayloadConversionError::EncodingError(Box::new(PayloadError::TooLarge {
                            bytes,
                        }))
                    }
                    _ => encode_codec_error(),
                })
        })
    }

    fn decode(
        &self,
        context: &SerializationContextData,
        payloads: Vec<Payload>,
    ) -> BoxFuture<'static, Result<Vec<Payload>, PayloadConversionError>> {
        let future = self.0.decode(context, payloads);
        Box::pin(async move {
            future.await.map_err(|_| {
                PayloadConversionError::EncodingError(Box::new(DecodePayloadError(
                    PayloadError::Decode,
                )))
            })
        })
    }
}

#[must_use]
pub(crate) fn build_data_converter(
    max_payload_bytes: usize,
    codec: Option<Arc<dyn PayloadCodec + Send + Sync>>,
) -> DataConverter {
    let payload_converter = PayloadConverter::composite([
        PayloadConverter::UseWrappers,
        PayloadConverter::Serde(Arc::new(FiniteBoundedJsonConverter::new(max_payload_bytes))),
    ]);
    let failure_converter = DefaultFailureConverter::new(false);
    match codec {
        Some(codec) => DataConverter::new(
            payload_converter,
            failure_converter,
            SharedPayloadCodec(codec),
        ),
        None => DataConverter::new(payload_converter, failure_converter, DefaultPayloadCodec),
    }
}

pub(crate) fn classify_payload_error(error: &PayloadConversionError) -> ClassifiedPayloadError {
    match error {
        PayloadConversionError::EncodingError(inner) => {
            if let Some(decoded) = inner.downcast_ref::<DecodePayloadError>() {
                return ClassifiedPayloadError::Decode(decoded.0);
            }
            if inner.is::<EncodeCodecError>() {
                return ClassifiedPayloadError::EncodeCodec;
            }
            if let Some(payload_error) = inner.downcast_ref::<PayloadError>() {
                return ClassifiedPayloadError::Encode(*payload_error);
            }
            for cause in shared_rules::error_chain::causes(inner.as_ref()) {
                if let Some(decoded) = cause.downcast_ref::<DecodePayloadError>() {
                    return ClassifiedPayloadError::Decode(decoded.0);
                }
                if cause.is::<EncodeCodecError>() {
                    return ClassifiedPayloadError::EncodeCodec;
                }
                if let Some(payload_error) = cause.downcast_ref::<PayloadError>() {
                    return ClassifiedPayloadError::Encode(*payload_error);
                }
            }
            ClassifiedPayloadError::Unknown
        }
        _ => ClassifiedPayloadError::Unknown,
    }
}
