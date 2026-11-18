use std::error::Error as _;

use super::*;
use crate::{ConfigurationError, PreparationError};

#[tokio::test]
async fn one_secret_free_realm_lookup_verifies_a_separate_token_origin() {
    let issuer_peer = Fixture::new().await;
    let provider = Fixture::new().await;
    let issuer = issuer_peer.origin.join("realms/operations/").unwrap();
    let options = Options {
        assertion_audience: issuer.to_string(),
        ..provider.options()
    };
    let metadata = serde_json::json!({
        "issuer": issuer.as_str(),
        "token_endpoint": options.token_url.replacen("http:", "HTTP:", 1),
    });
    *issuer_peer.state.resource_response.lock().unwrap() = json_response(StatusCode::OK, &metadata);
    let registration = Registration::prepare_keycloak_for_test_http(options)
        .await
        .unwrap();
    for scope in ["catalog", "billing"] {
        let (tokens, driver) =
            registration.service_tokens(service_target(&[scope]), provider.resource_origin());
        provider.drive(driver);
        let client = tokens.http(provider.resource_client()).unwrap();
        assert_eq!(
            provider.send(&client).await.unwrap().status(),
            StatusCode::OK
        );
    }
    assert_eq!(provider.token_requests().len(), 2);
    let requests = issuer_peer.resource_requests();
    assert_eq!(requests.len(), 1);
    let discovery = &requests[0];
    assert_eq!(discovery.method, "GET");
    assert_eq!(
        discovery.target,
        "/realms/operations/.well-known/openid-configuration"
    );
    assert!(discovery.header("authorization").is_none());
    assert_eq!(discovery.body.len(), 0);
    issuer_peer.finish().await;
    provider.finish().await;
}

#[tokio::test]
async fn inconsistent_unavailable_or_unbounded_metadata_refuses_registration() {
    let peer = Fixture::new().await;
    let issuer = peer.origin.join("realms/operations").unwrap().to_string();
    let options = Options {
        assertion_audience: issuer.clone(),
        ..peer.options()
    };
    std::assert_matches!(
        Registration::prepare_keycloak(options.clone())
            .await
            .unwrap_err(),
        PreparationError::Configuration(ConfigurationError {
            key: "assertion_audience",
            ..
        })
    );
    std::assert_matches!(
        Registration::prepare_keycloak(Options {
            assertion_audience: ASSERTION_AUDIENCE.to_owned(),
            ..options.clone()
        })
        .await
        .unwrap_err(),
        PreparationError::Configuration(ConfigurationError {
            key: "token_url",
            ..
        })
    );
    assert!(peer.resource_requests().is_empty());
    let metadata = |issuer: &str, endpoint: &str| {
        json_response(
            StatusCode::OK,
            &serde_json::json!({
                "issuer": issuer, "token_endpoint": endpoint,
            }),
        )
    };
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        peer.endpoint
    );
    for (body, reason) in [
        (
            metadata(&issuer.replacen("http:", "HTTP:", 1), &options.token_url),
            "issuer differs from assertion_audience",
        ),
        (
            metadata(&issuer, peer.origin.join("other").unwrap().as_str()),
            "token_endpoint is invalid or differs from token_url",
        ),
        (
            metadata(&issuer, &options.token_url.replacen("://", "://@", 1)),
            "token_endpoint is invalid or differs from token_url",
        ),
        (
            json_response(StatusCode::OK, &serde_json::json!({"issuer": issuer})),
            "invalid issuer metadata",
        ),
        (
            json_response(
                StatusCode::OK,
                &serde_json::json!([issuer, options.token_url]),
            ),
            "invalid issuer metadata",
        ),
        (
            redirect.into_bytes(),
            "issuer discovery did not return HTTP 200",
        ),
        (
            response(
                StatusCode::OK,
                &vec![b' '; TOKEN_LIMITS.response_body_bytes + 1],
            ),
            "response body is too large",
        ),
    ] {
        *peer.state.resource_response.lock().unwrap() = body;
        let attempts = peer.resource_requests().len();
        let error = Registration::prepare_keycloak_for_test_http(options.clone())
            .await
            .unwrap_err();
        assert!(
            error.to_string().ends_with(&format!("({reason})")),
            "{error}"
        );
        if reason == "response body is too large" {
            std::assert_matches!(
                error
                    .source()
                    .unwrap()
                    .downcast_ref::<infra_outbound_http::Error>(),
                Some(infra_outbound_http::Error::ResponseBodyTooLarge)
            );
        }
        assert_eq!(peer.resource_requests().len(), attempts + 1);
        assert!(peer.token_requests().is_empty());
    }
    peer.finish().await;
}

#[tokio::test]
async fn a_discovery_deadline_returns_a_timeout_cause() {
    let peer = Fixture::new().await;
    let gate = peer.block_resources();
    let options = Options {
        assertion_audience: peer.origin.join("realms/operations").unwrap().to_string(),
        ..peer.options()
    };
    let (result, ()) = tokio::join!(
        Registration::prepare_keycloak_for_test_http(options),
        async {
            peer.resource_received().await;
            advance(FETCH_TIMEOUT).await;
        }
    );
    let error = result.unwrap_err();
    assert_eq!(
        error.to_string(),
        "OAuth2 discovery request failed (operation timed out)"
    );
    std::assert_matches!(
        error
            .source()
            .unwrap()
            .downcast_ref::<infra_outbound_http::Error>(),
        Some(infra_outbound_http::Error::Timeout)
    );
    gate.add_permits(1);
    peer.finish().await;
}

#[test]
fn startup_diagnostics_retain_nested_causes_without_printing_them() {
    const PRIVATE_DETAIL: &str = "sensitive-provider-detail";
    let source = || Box::new(std::io::Error::other(PRIVATE_DETAIL));
    for (error, message) in [
        (
            PreparationError::Transport {
                endpoint: "assertion_audience",
                source: infra_outbound_http::BuildError::Tls { source: source() },
            },
            "OAuth2 transport construction failed for assertion_audience (outbound HTTP TLS configuration failed)",
        ),
        (
            PreparationError::DiscoveryRequest {
                source: infra_outbound_http::Error::Transport { source: source() },
            },
            "OAuth2 discovery request failed (HTTP transport failed)",
        ),
    ] {
        assert_eq!(error.to_string(), message);
        assert!(!format!("{error:?}").contains(PRIVATE_DETAIL));
        let cause = error.source().and_then(std::error::Error::source).unwrap();
        assert_eq!(cause.to_string(), PRIVATE_DETAIL);
    }
}
