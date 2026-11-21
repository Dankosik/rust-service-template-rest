#[cfg(feature = "test-support")]
pub async fn verify_fixture_authorization(
    verifier: &crate::Verifier,
    authorization: &[u8],
) -> Result<crate::Principal, crate::Failure> {
    let token = crate::parse_bearer([authorization])?;
    verifier.verify(&token).await
}

// template:begin oidc-jwt:authn-jwt-test-support
#[cfg(feature = "test-support")]
pub use crate::jwt::JwtFixture;
// template:end oidc-jwt:authn-jwt-test-support

// template:begin oidc-introspection:authn-introspection-test-support
use tokio_util::sync::CancellationToken;

use crate::{ClientBuildError, provider};

pub use crate::introspection::prepare_introspection_with_fixture;

#[derive(derive_more::Debug)]
#[debug("FixtureTransport([REDACTED])")]
pub struct FixtureTransport {
    provider: provider::ProviderClient,
}

impl FixtureTransport {
    pub fn try_new(
        fixture_host: &str,
        fixture_addr: std::net::SocketAddr,
        fixture_root_der: &[u8],
        cancel: CancellationToken,
    ) -> Result<Self, ClientBuildError> {
        provider::new_fixture_client(fixture_host, fixture_addr, fixture_root_der, cancel)
            .map(|provider| Self { provider })
    }

    pub(crate) fn into_provider(self) -> provider::ProviderClient {
        self.provider
    }
}
// template:end oidc-introspection:authn-introspection-test-support
