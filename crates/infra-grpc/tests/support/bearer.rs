use infra_bearerauthn::Verifier;
// template:begin oidc-jwt:grpc-transport-test-jwt-import
use infra_bearerauthn::test_support::JwtFixture;
// template:end oidc-jwt:grpc-transport-test-jwt-import
// template:begin oidc-introspection:grpc-transport-test-introspection-import
use super::{ACCEPTED, ProviderFixture, UNSCOPED, verifier_fixture};
// template:end oidc-introspection:grpc-transport-test-introspection-import

#[derive(Clone, Copy)]
pub(super) enum BearerMode {
    // template:begin oidc-introspection:grpc-transport-test-introspection-mode
    Introspection,
    // template:end oidc-introspection:grpc-transport-test-introspection-mode
    // template:begin oidc-jwt:grpc-transport-test-jwt-mode
    Jwt,
    // template:end oidc-jwt:grpc-transport-test-jwt-mode
}

impl BearerMode {
    pub(super) const SELECTED: Self = [
        // template:begin oidc-introspection:grpc-transport-test-select-introspection
        Self::Introspection,
        // template:end oidc-introspection:grpc-transport-test-select-introspection
        // template:begin oidc-jwt:grpc-transport-test-select-jwt
        Self::Jwt,
        // template:end oidc-jwt:grpc-transport-test-select-jwt
    ][0];

    pub(super) fn token(self, scoped: bool) -> &'static str {
        match self {
            // template:begin oidc-introspection:grpc-transport-test-introspection-token-arm
            Self::Introspection => {
                if scoped {
                    ACCEPTED
                } else {
                    UNSCOPED
                }
            }
            // template:end oidc-introspection:grpc-transport-test-introspection-token-arm
            // template:begin oidc-jwt:grpc-transport-test-jwt-token-arm
            Self::Jwt => jwt_token(scoped),
            // template:end oidc-jwt:grpc-transport-test-jwt-token-arm
        }
    }
}

// template:begin oidc-jwt:grpc-transport-test-jwt-token
fn jwt_token(scoped: bool) -> &'static str {
    static SCOPED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static UNSCOPED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let (token, scope) = if scoped {
        (&SCOPED, "echo.read")
    } else {
        (&UNSCOPED, "")
    };
    token.get_or_init(|| JwtFixture::shared().token(scope))
}
// template:end oidc-jwt:grpc-transport-test-jwt-token

#[derive(Default)]
pub(super) struct BearerFixture {
    // template:begin oidc-introspection:grpc-transport-test-provider-field
    pub(super) provider: Option<ProviderFixture>,
    // template:end oidc-introspection:grpc-transport-test-provider-field
}

impl BearerFixture {
    #[allow(
        clippy::unused_async,
        reason = "the introspection profile starts a local provider"
    )]
    pub(super) async fn start() -> (Verifier, Self) {
        match BearerMode::SELECTED {
            // template:begin oidc-introspection:grpc-transport-test-bearer-fixture-introspection
            BearerMode::Introspection => {
                let (verifier, provider) = verifier_fixture().await;
                (
                    verifier,
                    Self {
                        provider: Some(provider),
                    },
                )
            }
            // template:end oidc-introspection:grpc-transport-test-bearer-fixture-introspection
            // template:begin oidc-jwt:grpc-transport-test-bearer-fixture-jwt-arm
            BearerMode::Jwt => (JwtFixture::shared().verifier(), Self::default()),
            // template:end oidc-jwt:grpc-transport-test-bearer-fixture-jwt-arm
        }
    }

    #[allow(
        clippy::unused_async,
        reason = "the introspection profile joins its local provider"
    )]
    pub(super) async fn stop(self) {
        // template:begin oidc-introspection:grpc-transport-test-bearer-fixture-stop
        if let Some(provider) = self.provider {
            provider.stop().await;
        }
        // template:end oidc-introspection:grpc-transport-test-bearer-fixture-stop
    }
}
