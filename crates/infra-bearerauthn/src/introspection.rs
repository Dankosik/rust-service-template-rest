//! RFC 7662 opaque-token introspection.

use std::{
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use secrecy::ExposeSecret;
use tokio::{sync::Semaphore, time::Instant};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use url::Url;

use crate::{
    BearerToken, Failure, IntrospectionOptions, Principal, Verifier,
    claims::{ClaimPolicy, validate_introspection_claims},
    provider::{ProviderClient, parse_provider_url, reserve_request_deadline},
};

const MAX_IN_FLIGHT_EXCHANGES: usize = 32;

/// Builds the opaque-token verifier without performing provider I/O.
///
/// # Errors
///
/// Returns [`Failure::Unavailable`] when the configured endpoint or private
/// provider transport cannot be prepared.
pub fn prepare_introspection(
    options: IntrospectionOptions,
    tracker: TaskTracker,
    cancel: CancellationToken,
) -> Result<Verifier, Failure> {
    let provider = ProviderClient::new(tracker, cancel)?;
    prepare_with_provider(options, provider)
}

/// Prepares the real introspection verifier with the narrowly admitted local
/// TLS transport used by cross-crate tests.
///
/// # Errors
///
/// Returns [`Failure::Unavailable`] when the fixture transport or configured
/// endpoint is unsuitable for verification.
#[cfg(any(test, feature = "test-support"))]
pub fn prepare_introspection_with_fixture(
    options: IntrospectionOptions,
    fixture: crate::test_support::FixtureTransport,
) -> Result<Verifier, Failure> {
    prepare_with_provider(options, fixture.into_provider())
}

fn prepare_with_provider(
    options: IntrospectionOptions,
    provider: ProviderClient,
) -> Result<Verifier, Failure> {
    let policy = ClaimPolicy::new(options.issuer, options.audience);
    let endpoint = parse_provider_url(&options.endpoint)?;

    Ok(Verifier::Introspection(IntrospectionVerifier {
        endpoint,
        client_id: options.client_id,
        client_secret: options.client_secret,
        policy: Arc::new(policy),
        provider,
        permits: Arc::new(Semaphore::new(MAX_IN_FLIGHT_EXCHANGES)),
    }))
}

/// An uncached, bounded client for one configured introspection endpoint.
#[derive(Clone)]
pub struct IntrospectionVerifier {
    endpoint: Url,
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
            // The enclosing HTTP timeout is the authority for an exhausted
            // request budget. Do no provider work and leave it to produce 504.
            tokio::time::sleep_until(deadline).await;
            return Err(Failure::Unavailable);
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
                &self.endpoint,
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
    use tokio_util::{sync::CancellationToken, task::TaskTracker};

    use super::{ClaimPolicy, IntrospectionVerifier, basic_authorization, form_body};
    use crate::{
        Failure, parse_bearer,
        provider::{ProviderClient, parse_provider_url, reserve_request_deadline},
    };

    fn verifier_with_permits(permits: usize) -> (IntrospectionVerifier, Arc<Semaphore>) {
        let permits = Arc::new(Semaphore::new(permits));
        let verifier = IntrospectionVerifier {
            endpoint: parse_provider_url("https://127.0.0.1/introspect").unwrap(),
            client_id: "fixture-client".to_owned(),
            client_secret: secrecy::SecretString::from("fixture-secret"),
            policy: Arc::new(ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                "api".to_owned(),
            )),
            provider: ProviderClient::new(TaskTracker::new(), CancellationToken::new()).unwrap(),
            permits: permits.clone(),
        };
        (verifier, permits)
    }

    #[test]
    fn client_secret_basic_encodes_each_component_before_base64() {
        assert_eq!(basic_authorization("a:b", "c d"), b"Basic YSUzQWI6Yytk");
    }

    #[test]
    fn form_body_keeps_the_token_out_of_the_url_and_uses_access_token_hint() {
        let token = parse_bearer([b"Bearer a+b/=".as_slice()]).unwrap();

        assert_eq!(
            form_body(&token).unwrap(),
            "token=a%2Bb%2F%3D&token_type_hint=access_token"
        );
    }

    #[test]
    fn endpoint_requires_an_exact_https_destination_without_user_info() {
        assert!(parse_provider_url("https://provider.example/oauth/introspect").is_ok());
        for value in [
            "http://provider.example/introspect",
            "https://client:secret@provider.example/introspect",
            "https://provider.example/introspect#fragment",
            "https://provider.example/introspect?query=value",
            " https://provider.example/introspect",
            "https://@provider.example/introspect",
            "not a url",
        ] {
            assert_eq!(parse_provider_url(value), Err(Failure::Unavailable));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reserves_parent_response_time_before_provider_work() {
        let now = Instant::now();
        assert_eq!(
            reserve_request_deadline(now, now + std::time::Duration::from_millis(100)),
            None
        );
        assert_eq!(
            reserve_request_deadline(now, now + std::time::Duration::from_secs(10)),
            Some(now + std::time::Duration::from_secs(3))
        );
    }

    #[tokio::test]
    async fn capacity_denial_is_immediate_and_starts_no_provider_exchange() {
        let (verifier, permits) = verifier_with_permits(32);
        let held = (0..32)
            .map(|_| permits.clone().try_acquire_owned().unwrap())
            .collect::<Vec<_>>();
        let token = parse_bearer([b"Bearer opaque".as_slice()]).unwrap();

        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                verifier.verify(&token, Instant::now() + std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap(),
            Err(Failure::Unavailable)
        );
        drop(held);
        assert_eq!(permits.available_permits(), 32);
    }

    #[tokio::test]
    async fn provider_failure_releases_the_admission_permit() {
        let (verifier, permits) = verifier_with_permits(1);
        let token = parse_bearer([b"Bearer opaque".as_slice()]).unwrap();

        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await,
            Err(Failure::Unavailable)
        );
        assert_eq!(permits.available_permits(), 1);
    }
}
