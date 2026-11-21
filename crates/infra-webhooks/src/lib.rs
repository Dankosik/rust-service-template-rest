pub mod protocol;
// template:begin webhooks:webhooks-outbound-module
pub mod outbound;
// template:end webhooks:webhooks-outbound-module
// template:begin inbound-webhooks:webhooks-inbound-module
pub mod inbound;
// template:end inbound-webhooks:webhooks-inbound-module
#[cfg(feature = "test-support")]
pub mod test_support;

pub(crate) mod base64_bytes {
    use std::fmt;

    use base64::{
        DecodeError, DecodeSliceError, Engine as _, display::Base64Display,
        engine::general_purpose::STANDARD_PAD_INDIFFERENT,
    };
    use bytes::Bytes;
    use serde::{Deserializer, Serializer, de};

    pub(crate) fn serialize<T: AsRef<[u8]>, S: Serializer>(
        source: &T,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Base64Display::new(
            source.as_ref(),
            &STANDARD_PAD_INDIFFERENT,
        ))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Bytes, D::Error> {
        deserialize_with(deserializer, decode_bytes)
    }

    pub(crate) fn deserialize_with<'de, T, D: Deserializer<'de>>(
        deserializer: D,
        decode: fn(&str) -> Result<T, DecodeError>,
    ) -> Result<T, D::Error> {
        struct Base64Visitor<T>(fn(&str) -> Result<T, DecodeError>);

        impl<T> de::Visitor<'_> for Base64Visitor<T> {
            type Value = T;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a base64 encoded string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                (self.0)(value).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(Base64Visitor(decode))
    }

    pub(crate) fn decode_slice(encoded: &str, out: &mut [u8]) -> Result<usize, DecodeSliceError> {
        STANDARD_PAD_INDIFFERENT.decode_slice(encoded, out)
    }

    pub(crate) fn decode_vec(encoded: &str) -> Result<Vec<u8>, DecodeError> {
        STANDARD_PAD_INDIFFERENT.decode(encoded)
    }

    fn decode_bytes(encoded: &str) -> Result<Bytes, DecodeError> {
        if encoded.is_empty() {
            return Ok(Bytes::new());
        }
        let Some(exact_len) = exact_decoded_len(encoded.as_bytes()) else {
            return decode_vec(encoded).map(Bytes::from);
        };
        let mut decoded = vec![0; exact_len];
        match decode_slice(encoded, &mut decoded) {
            Ok(len) => {
                decoded.truncate(len);
                Ok(Bytes::from(decoded))
            }
            Err(DecodeSliceError::DecodeError(error)) => Err(error),
            Err(DecodeSliceError::OutputSliceTooSmall) => decode_vec(encoded).map(Bytes::from),
        }
    }

    fn exact_decoded_len(encoded: &[u8]) -> Option<usize> {
        let unpadded = match encoded {
            [.., b'=', b'='] => encoded.len() - 2,
            [.., b'='] => encoded.len() - 1,
            _ => encoded.len(),
        };
        let rem = unpadded % 4;
        if rem == 1 {
            return None;
        }
        (unpadded / 4).checked_mul(3)?.checked_add((rem * 3) / 4)
    }

    #[cfg(test)]
    mod tests {
        use base64::{
            DecodeSliceError, Engine as _,
            engine::general_purpose::{STANDARD, STANDARD_NO_PAD, STANDARD_PAD_INDIFFERENT},
        };

        use super::{decode_bytes, decode_slice, exact_decoded_len};

        #[test]
        fn decode_bytes_matches_pad_indifferent_decoder_on_valid_and_malformed_inputs() {
            for len in 0..=66 {
                let raw: Vec<u8> = (0..len)
                    .map(|i| u8::try_from(i * 7 % 256).unwrap())
                    .collect();
                let padded = STANDARD.encode(&raw);
                let unpadded = STANDARD_NO_PAD.encode(&raw);
                assert_eq!(exact_decoded_len(padded.as_bytes()), Some(len));
                assert_eq!(exact_decoded_len(unpadded.as_bytes()), Some(len));
                assert_eq!(decode_bytes(&padded).unwrap().as_ref(), raw.as_slice());
                assert_eq!(decode_bytes(&unpadded).unwrap().as_ref(), raw.as_slice());
                if let Some(partial) = padded.strip_suffix('=') {
                    assert_eq!(exact_decoded_len(partial.as_bytes()), Some(len));
                    assert_eq!(decode_bytes(partial).unwrap().as_ref(), raw.as_slice());
                }
            }

            assert_eq!(
                decode_slice("AAA==", &mut [0; 2]),
                Err(DecodeSliceError::OutputSliceTooSmall)
            );
            for malformed in [
                "=",
                "==",
                "===",
                "====",
                "=====",
                "A",
                "A=",
                "A==",
                "A===",
                "AB=",
                "ABC",
                "AAA==",
                "AAAA=",
                "AAAA==",
                "AAAA===",
                "AAAA====",
                "AAA===",
                "AAAAA=",
                "AAAAAAA==",
                "AAAA=!",
                "!!!!",
                "AA=A",
                "AB!=",
            ] {
                assert_eq!(
                    decode_bytes(malformed).map(|bytes| bytes.to_vec()),
                    STANDARD_PAD_INDIFFERENT.decode(malformed),
                    "input {malformed:?}"
                );
            }
        }
    }
}
