//! RFC 7662 opaque-token introspection.

use std::{
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use secrecy::ExposeSecret;
use tokio::{sync::Semaphore, time::Instant};

use crate::{
    BearerToken, Failure, IntrospectionOptions, PreparationError, Principal, Verifier,
    claims::{ClaimPolicy, validate_introspection_claims},
    provider::{ProviderClient, reserve_request_deadline},
};

/// Builds the opaque-token verifier without performing provider I/O.
pub fn prepare_introspection(options: IntrospectionOptions) -> Result<Verifier, PreparationError> {
    let provider = ProviderClient::new()?;
    prepare_with_provider(options, provider)
}

/// Prepares a verifier through fixture-only local TLS transport.
#[cfg(any(test, feature = "test-support"))]
pub fn prepare_introspection_with_fixture(
    options: IntrospectionOptions,
    fixture: crate::test_support::FixtureTransport,
) -> Result<Verifier, PreparationError> {
    prepare_with_provider(options, fixture.into_provider())
}

fn prepare_with_provider(
    options: IntrospectionOptions,
    provider: ProviderClient,
) -> Result<Verifier, PreparationError> {
    if options.audiences.is_empty() || options.audiences.iter().any(String::is_empty) {
        return Err(PreparationError::new(
            crate::PreparationPhase::Options,
            crate::PreparationReason::Parse,
        ));
    }
    Ok(Verifier::Introspection(IntrospectionVerifier {
        endpoint: options.endpoint,
        client_id: options.client_id,
        client_secret: options.client_secret,
        policy: Arc::new(ClaimPolicy::new(
            options.issuer.as_str().to_owned(),
            options.audiences,
        )),
        provider,
        permits: Arc::new(Semaphore::new(options.provider_concurrency.get())),
    }))
}

/// An uncached, bounded client for one configured introspection endpoint.
#[derive(Clone)]
pub struct IntrospectionVerifier {
    endpoint: crate::ProviderUrl,
    client_id: String,
    client_secret: secrecy::SecretString,
    policy: Arc<ClaimPolicy>,
    provider: ProviderClient,
    permits: Arc<Semaphore>,
}

impl fmt::Debug for IntrospectionVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IntrospectionVerifier([REDACTED])")
    }
}

impl IntrospectionVerifier {
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
        deadline: Instant,
    ) -> Result<Principal, Failure> {
        let Some(provider_deadline) = reserve_request_deadline(Instant::now(), deadline) else {
            return Err(Failure::Timeout);
        };
        let _permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Unavailable)?;
        let body = form_body(token)?;
        let authorization =
            basic_authorization(&self.client_id, self.client_secret.expose_secret());
        let response = self
            .provider
            .post_form_json(
                self.endpoint.url(),
                &authorization,
                body.as_bytes(),
                provider_deadline,
            )
            .await?;
        validate_introspection_claims(&response, &self.policy, now_epoch_seconds()?)
    }
}

fn now_epoch_seconds() -> Result<u64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| Failure::Unavailable)
}

fn form_body(token: &BearerToken<'_>) -> Result<String, Failure> {
    let value = std::str::from_utf8(token.as_bytes()).map_err(|_| Failure::Invalid)?;
    Ok(url::form_urlencoded::Serializer::new(String::new())
        .append_pair("token", value)
        .append_pair("token_type_hint", "access_token")
        .finish())
}

fn basic_authorization(client_id: &str, client_secret: &str) -> Vec<u8> {
    let mut credential = form_component(client_id);
    credential.push(':');
    credential.push_str(&form_component(client_secret));
    let encoded = STANDARD.encode(credential);
    let mut authorization = b"Basic ".to_vec();
    authorization.extend_from_slice(encoded.as_bytes());
    authorization
}

fn form_component(value: &str) -> String {
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(value, "")
        .finish();
    encoded[..encoded.len().saturating_sub(1)].to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::{sync::Semaphore, time::Instant};

    use super::{ClaimPolicy, IntrospectionVerifier, basic_authorization, form_body};
    use crate::{Failure, ProviderUrl, parse_bearer, provider::ProviderClient};

    #[test]
    fn client_secret_basic_encodes_each_component_before_base64() {
        assert_eq!(basic_authorization("a:b", "c d"), b"Basic YSUzQWI6Yytk");
    }

    #[test]
    fn form_body_keeps_the_token_out_of_the_url() {
        let token = parse_bearer([b"Bearer a+b/=".as_slice()], 32 * 1024).unwrap();
        assert_eq!(
            form_body(&token).unwrap(),
            "token=a%2Bb%2F%3D&token_type_hint=access_token"
        );
    }

    #[tokio::test]
    async fn exhausted_configured_capacity_rejects_before_provider_io() {
        let verifier = IntrospectionVerifier {
            endpoint: ProviderUrl::parse("https://127.0.0.1/introspect").unwrap(),
            client_id: "client".to_owned(),
            client_secret: secrecy::SecretString::from("secret"),
            policy: Arc::new(ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            )),
            provider: ProviderClient::new().unwrap(),
            permits: Arc::new(Semaphore::new(0)),
        };
        let token = parse_bearer([b"Bearer opaque".as_slice()], 32 * 1024).unwrap();
        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await,
            Err(Failure::Unavailable)
        );
    }
}
