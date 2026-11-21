// template:begin webhooks:webhooks-outbound-test-support
use infra_outbound_http::Client;
use url::Url;

use crate::outbound::{Endpoint, EndpointError};
use crate::protocol::KeyRing;

pub fn endpoint_with_client(
    destination: Url,
    client: Client,
    keys: KeyRing,
) -> Result<Endpoint, EndpointError> {
    Endpoint::with_client(destination, client, keys)
}
// template:end webhooks:webhooks-outbound-test-support
