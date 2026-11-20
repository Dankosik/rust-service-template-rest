pub mod protocol;
// template:begin webhooks:webhooks-outbound-module
pub mod outbound;
// template:end webhooks:webhooks-outbound-module
// template:begin inbound-webhooks:webhooks-inbound-module
pub mod inbound;
// template:end inbound-webhooks:webhooks-inbound-module

pub(crate) mod base64_bytes {
    use std::fmt;
    use std::marker::PhantomData;

    use base64::{
        Engine as _, display::Base64Display, engine::general_purpose::STANDARD_PAD_INDIFFERENT,
    };
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

    pub(crate) fn deserialize<'de, T: From<Vec<u8>>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        struct Base64Visitor<T>(PhantomData<fn() -> T>);

        impl<T: From<Vec<u8>>> de::Visitor<'_> for Base64Visitor<T> {
            type Value = T;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a base64 encoded string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                STANDARD_PAD_INDIFFERENT
                    .decode(value)
                    .map(T::from)
                    .map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_str(Base64Visitor(PhantomData))
    }
}
