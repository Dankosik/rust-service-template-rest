use std::marker::PhantomData;

use tonic::codec::{BufferSettings, Codec};
use tonic_prost::{ProstDecoder, ProstEncoder};

const BUFFER_SIZE_BYTES: usize = 2 * 1024;
const STREAM_YIELD_THRESHOLD_BYTES: usize = 32 * 1024;

pub(crate) struct ContractCodec<Outbound, Inbound>(PhantomData<fn(Outbound) -> Inbound>);

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
