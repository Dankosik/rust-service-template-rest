use std::num::NonZeroU64;

use bytes::Bytes;

pub(crate) struct BodyEndedEarly;

pub(crate) enum ByteProgress {
    Reading(NonZeroU64),
    AwaitingEof(Option<Bytes>),
}

impl ByteProgress {
    pub(crate) fn new(len: u64) -> Self {
        NonZeroU64::new(len).map_or(Self::AwaitingEof(None), Self::Reading)
    }

    pub(crate) fn remaining(&self) -> u64 {
        match self {
            Self::Reading(remaining) => remaining.get(),
            Self::AwaitingEof(held) => held.as_ref().map_or(0, |bytes| bytes.len() as u64),
        }
    }

    pub(crate) fn accept(&mut self, chunk: Bytes) -> Result<Option<Bytes>, Bytes> {
        let Self::Reading(remaining) = self else {
            return Err(chunk);
        };
        let Some(next) = remaining.get().checked_sub(chunk.len() as u64) else {
            return Err(chunk);
        };
        if let Some(remaining) = NonZeroU64::new(next) {
            *self = Self::Reading(remaining);
            Ok(Some(chunk))
        } else {
            *self = Self::AwaitingEof(Some(chunk));
            Ok(None)
        }
    }

    pub(crate) fn finish(&mut self) -> Result<Option<Bytes>, BodyEndedEarly> {
        match self {
            Self::AwaitingEof(held) => Ok(held.take()),
            Self::Reading(_) => Err(BodyEndedEarly),
        }
    }
}
