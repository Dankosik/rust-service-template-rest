//! Standard Webhooks signing and durable work on the existing jobs queue.

use base64::{display::Base64Display, engine::general_purpose::STANDARD};
use serde::Serializer;
use serde_with::SerializeAs;

struct StreamingBase64;

impl<T: AsRef<[u8]>> SerializeAs<T> for StreamingBase64 {
    fn serialize_as<S: Serializer>(source: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&Base64Display::new(source.as_ref(), &STANDARD))
    }
}

pub mod protocol;
// template:begin webhooks:webhooks-outbound-module
pub mod outbound;
// template:end webhooks:webhooks-outbound-module
// template:begin inbound-webhooks:webhooks-inbound-module
pub mod inbound;
// template:end inbound-webhooks:webhooks-inbound-module
