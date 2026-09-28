//! Tonic's stock prost codec with a smaller per-call buffer.
//!
//! Tonic allocates `buffer_size` for each call's decoder and encoder up front
//! and grows by that step for larger messages. Its 8 KiB default costs 16 KiB
//! per open call; 2 KiB keeps an idle stream at about 6 KiB with no measured
//! CPU change for these small messages.

use std::marker::PhantomData;

use tonic::codec::{BufferSettings, Codec};
use tonic_prost::{ProstDecoder, ProstEncoder};

const BUFFER_SIZE: usize = 2 * 1024;
/// Tonic's default: an encoded stream yields a chunk once this much is buffered.
const YIELD_THRESHOLD: usize = 32 * 1024;

/// `codec_path` of the generated services and clients.
#[derive(Debug, Clone)]
pub struct ContractCodec<T, U>(PhantomData<(T, U)>);

impl<T, U> Default for ContractCodec<T, U> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, U> Codec for ContractCodec<T, U>
where
    T: prost::Message + Send + 'static,
    U: prost::Message + Default + Send + 'static,
{
    type Encode = T;
    type Decode = U;
    type Encoder = ProstEncoder<T>;
    type Decoder = ProstDecoder<U>;

    fn encoder(&mut self) -> Self::Encoder {
        ProstEncoder::new(BufferSettings::new(BUFFER_SIZE, YIELD_THRESHOLD))
    }

    fn decoder(&mut self) -> Self::Decoder {
        ProstDecoder::new(BufferSettings::new(BUFFER_SIZE, YIELD_THRESHOLD))
    }
}
