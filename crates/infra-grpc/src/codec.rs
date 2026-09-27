use std::marker::PhantomData;
use std::sync::Arc;

use bytes::Buf as _;
use prost::Message;
use prost_reflect::ReflectMessage;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::{Status, codec::BufferSettings};

use crate::call::CallState;

const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;
const MESSAGE_LIMIT_DETAIL: &str = "message exceeds transport limit";

/// Client-only tonic-prost codec that enforces the fixed decoded and encoded
/// message limit before the upstream codec can allocate or serialize beyond it.
/// It deliberately carries no inbound call state or validation capability.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct BoundedClientCodec<Encode, Decode> {
    marker: PhantomData<(Encode, Decode)>,
}

impl<Encode, Decode> std::fmt::Debug for BoundedClientCodec<Encode, Decode> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedClientCodec")
            .finish_non_exhaustive()
    }
}

impl<Encode, Decode> Codec for BoundedClientCodec<Encode, Decode>
where
    Encode: Message + Send + 'static,
    Decode: Message + Default + Send + 'static,
{
    type Encode = Encode;
    type Decode = Decode;
    type Encoder = BoundedEncoder<Encode>;
    type Decoder = BoundedDecoder<Decode>;

    fn encoder(&mut self) -> Self::Encoder {
        BoundedEncoder {
            inner: tonic_prost::ProstCodec::<Encode, Decode>::raw_encoder(BufferSettings::default()),
        }
    }

    fn decoder(&mut self) -> Self::Decoder {
        BoundedDecoder {
            inner: tonic_prost::ProstCodec::<Encode, Decode>::raw_decoder(BufferSettings::default()),
        }
    }
}

pub struct BoundedEncoder<T> {
    inner: tonic_prost::ProstEncoder<T>,
}

impl<T> std::fmt::Debug for BoundedEncoder<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedEncoder")
            .finish_non_exhaustive()
    }
}

impl<T: Message> Encoder for BoundedEncoder<T> {
    type Item = T;
    type Error = Status;

    fn encode(
        &mut self,
        item: Self::Item,
        destination: &mut EncodeBuf<'_>,
    ) -> Result<(), Self::Error> {
        if item.encoded_len() > MAX_MESSAGE_BYTES {
            return Err(Status::resource_exhausted(MESSAGE_LIMIT_DETAIL));
        }
        self.inner.encode(item, destination)
    }

    fn buffer_settings(&self) -> BufferSettings {
        self.inner.buffer_settings()
    }
}

pub struct BoundedDecoder<T> {
    inner: tonic_prost::ProstDecoder<T>,
}

impl<T> std::fmt::Debug for BoundedDecoder<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundedDecoder")
            .finish_non_exhaustive()
    }
}

impl<T: Message + Default> Decoder for BoundedDecoder<T> {
    type Item = T;
    type Error = Status;

    fn decode(&mut self, source: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        if source.remaining() > MAX_MESSAGE_BYTES {
            return Err(Status::resource_exhausted(MESSAGE_LIMIT_DETAIL));
        }
        self.inner.decode(source)
    }

    fn buffer_settings(&self) -> BufferSettings {
        self.inner.buffer_settings()
    }
}

/// Server-only prost codec that captures the active governed call state.
/// Generated client code deliberately keeps tonic-prost's stock codec because
/// client decodes never carry inbound server validation state.
#[doc(hidden)]
#[derive(Clone)]
pub struct ValidatedCodec<Encode, Decode> {
    state: Option<Arc<CallState>>,
    marker: PhantomData<(Encode, Decode)>,
}

impl<Encode, Decode> std::fmt::Debug for ValidatedCodec<Encode, Decode> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedCodec")
            .finish_non_exhaustive()
    }
}

impl<Encode, Decode> Default for ValidatedCodec<Encode, Decode> {
    fn default() -> Self {
        Self {
            state: CallState::current(),
            marker: PhantomData,
        }
    }
}

impl<Encode, Decode> Codec for ValidatedCodec<Encode, Decode>
where
    Encode: Message + Send + 'static,
    Decode: Message + Default + ReflectMessage + Send + 'static,
{
    type Encode = Encode;
    type Decode = Decode;
    type Encoder = tonic_prost::ProstEncoder<Encode>;
    type Decoder = ValidatingDecoder<Decode>;

    fn encoder(&mut self) -> Self::Encoder {
        tonic_prost::ProstCodec::<Encode, Decode>::raw_encoder(BufferSettings::default())
    }

    fn decoder(&mut self) -> Self::Decoder {
        ValidatingDecoder {
            inner: tonic_prost::ProstCodec::<Encode, Decode>::raw_decoder(BufferSettings::default()),
            state: self.state.clone(),
        }
    }
}

pub struct ValidatingDecoder<T> {
    inner: tonic_prost::ProstDecoder<T>,
    state: Option<Arc<CallState>>,
}

impl<T> std::fmt::Debug for ValidatingDecoder<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatingDecoder")
            .finish_non_exhaustive()
    }
}

impl<T> Decoder for ValidatingDecoder<T>
where
    T: Message + Default + ReflectMessage + Send + 'static,
{
    type Item = T;
    type Error = Status;

    fn decode(&mut self, source: &mut DecodeBuf<'_>) -> Result<Option<Self::Item>, Self::Error> {
        let Some(message) = self.inner.decode(source)? else {
            return Ok(None);
        };
        let Some(state) = &self.state else {
            return Err(Status::internal(service_failure::SANITIZED_DETAIL));
        };
        state.validate(&message).map(|()| Some(message))
    }

    fn buffer_settings(&self) -> BufferSettings {
        self.inner.buffer_settings()
    }
}
