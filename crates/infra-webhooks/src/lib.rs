use base64::{display::Base64Display, engine::general_purpose::STANDARD};
use serde::{Deserializer, Serializer};
use serde_with::{DeserializeAs, SerializeAs, base64::Base64};

struct StreamingBase64;

impl<T: AsRef<[u8]>> SerializeAs<T> for StreamingBase64 {
    fn serialize_as<S: Serializer>(source: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Base64Display::new(source.as_ref(), &STANDARD))
    }
}

impl<'de, T> DeserializeAs<'de, T> for StreamingBase64
where
    Base64: DeserializeAs<'de, T>,
{
    fn deserialize_as<D: Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
        <Base64 as DeserializeAs<'de, T>>::deserialize_as(deserializer)
    }
}

pub mod protocol;
// template:begin webhooks:webhooks-outbound-module
pub mod outbound;
// template:end webhooks:webhooks-outbound-module
// template:begin inbound-webhooks:webhooks-inbound-module
pub mod inbound;
// template:end inbound-webhooks:webhooks-inbound-module
