//! Stock prost encoding and decoding with smaller per-call Tonic buffers.
//!
//! Tonic allocates `buffer_size` for each call's decoder and encoder up front
//! and grows for larger messages; this setting is not a message-size limit.
//! The 2 KiB choice reduced held-stream memory in the measurements recorded in
//! `docs/grpc-decisions.md`; 1 KiB increased instructions for 1 KiB messages.
//! Keep that tradeoff when changing contracts or upgrading Tonic.
//! Encoding, decoding and decode errors remain owned by `tonic_prost`.

use std::marker::PhantomData;

use tonic::codec::{BufferSettings, Codec};
use tonic_prost::{ProstDecoder, ProstEncoder};

const BUFFER_SIZE_BYTES: usize = 2 * 1024;
// Preserve Tonic's default batching of ready streaming messages. A pending
// source flushes a smaller batch without waiting for this threshold.
const STREAM_YIELD_THRESHOLD_BYTES: usize = 32 * 1024;

/// Buffer policy selected by `tools/grpc-codegen` for generated clients and servers.
///
/// `Outbound` is encoded and `Inbound` is decoded. Clients send requests and
/// receive responses; servers use the opposite direction.
#[derive(Debug, Clone)]
pub struct ContractCodec<Outbound, Inbound>(PhantomData<(Outbound, Inbound)>);

impl<Outbound, Inbound> Default for ContractCodec<Outbound, Inbound> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<Outbound, Inbound> Codec for ContractCodec<Outbound, Inbound>
where
    Outbound: prost::Message + Send + 'static,
    Inbound: prost::Message + Default + Send + 'static,
{
    type Encode = Outbound;
    type Decode = Inbound;
    type Encoder = ProstEncoder<Outbound>;
    type Decoder = ProstDecoder<Inbound>;

    fn encoder(&mut self) -> Self::Encoder {
        ProstEncoder::new(BufferSettings::new(
            BUFFER_SIZE_BYTES,
            STREAM_YIELD_THRESHOLD_BYTES,
        ))
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProstDecoder::new(BufferSettings::new(
            BUFFER_SIZE_BYTES,
            STREAM_YIELD_THRESHOLD_BYTES,
        ))
    }
}
