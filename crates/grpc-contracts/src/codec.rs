//! Stock prost codec with smaller per-call Tonic buffers.
//!
//! Tonic allocates `buffer_size` per call up front and grows for larger messages; it is not
//! a message-size limit. 2 KiB reduced held-stream memory in the measurements in
//! `docs/decisions/grpc-decisions.md`; 1 KiB increased instructions for 1 KiB messages. Keep
//! that tradeoff when changing contracts or upgrading Tonic.

use std::marker::PhantomData;

use tonic::codec::{BufferSettings, Codec};
use tonic_prost::{ProstDecoder, ProstEncoder};

const BUFFER_SIZE_BYTES: usize = 2 * 1024;
// Preserve Tonic's default batching of ready streaming messages. A pending
// source flushes a smaller batch without waiting for this threshold.
const STREAM_YIELD_THRESHOLD_BYTES: usize = 32 * 1024;

/// `Outbound` is encoded and `Inbound` is decoded; servers use the opposite direction of
/// clients.
#[derive_where::derive_where(Debug, Clone, Default)]
pub struct ContractCodec<Outbound, Inbound>(PhantomData<(Outbound, Inbound)>);

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
