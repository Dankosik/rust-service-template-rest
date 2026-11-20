use std::error::Error as _;

use super::*;
use crate::{
    DiscoveryError, DiscoveryTransportError, PreparationError, TransportBuildFailure,
    TransportFailure, discovery::HttpDiscoveryOptions, verify_oidc_metadata,
};

fn discovery_options(issuer: String, fixture: &Fixture) -> HttpDiscoveryOptions {
    let options = fixture.options();
    HttpDiscoveryOptions {
        issuer,
        token_url: options.token_url,
        client_id: options.client_id,
        private_key: options.auth.private_key,
        key_id: options.auth.key_id,
        algorithm: options.auth.algorithm,
        provider_concurrency: options.provider_concurrency,
    }
}

#[tokio::test]
async fn one_secret_free_realm_lookup_verifies_a_separate_token_origin() {
    let issuer_peer = Fixture::new().await;
    let provider = Fixture::new().await;
    let issuer = issuer_peer.origin.join("realms/operations/").unwrap();
    let options = discovery_options(issuer.to_string(), &provider);
    let metadata = serde_json::json!({
        "issuer": issuer.as_str(),
        "token_endpoint": options.token_url.replacen("http:", "HTTP:", 1),
    });
    *issuer_peer.state.resource_response.lock().unwrap() = json_response(StatusCode::OK, &metadata);
    let registration = Registration::prepare_with_oidc_discovery_for_test_http(options)
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
    assert_eq!(provider.token_request_count(), 2);
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

#[derive(Clone, Copy)]
enum Expected {
    Metadata(&'static str),
    Status(StatusCode),
    Decode(&'static str),
    ResponseLimit,
}

#[tokio::test]
async fn inconsistent_unavailable_or_unbounded_metadata_refuses_registration() {
    let peer = Fixture::new().await;
    let issuer = peer.origin.join("realms/operations").unwrap().to_string();
    let options = discovery_options(issuer.clone(), &peer);
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
    let oversized_unavailable = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        TOKEN_LIMITS.response_body_bytes() + 1,
    );
    for (body, expected) in [
        (
            metadata(
                &issuer.replacen("http:", "HTTP:", 1),
                options.token_url.as_str(),
            ),
            Expected::Metadata("issuer differs from configured issuer"),
        ),
        (
            metadata(&issuer, peer.origin.join("other").unwrap().as_str()),
            Expected::Metadata("token_endpoint is invalid or differs from token_url"),
        ),
        (
            metadata(&issuer, &options.token_url.replacen("://", "://@", 1)),
            Expected::Metadata("token_endpoint is invalid or differs from token_url"),
        ),
        (
            json_response(StatusCode::OK, &serde_json::json!({"issuer": issuer})),
            Expected::Decode("missing field"),
        ),
        (
            json_response(
                StatusCode::OK,
                &serde_json::json!({"issuer": 8_240_081, "token_endpoint": options.token_url.as_str()}),
            ),
            Expected::Decode("8240081"),
        ),
        (
            json_response(
                StatusCode::OK,
                &serde_json::json!([issuer, options.token_url.as_str()]),
            ),
            Expected::Metadata("invalid issuer metadata"),
        ),
        (redirect.into_bytes(), Expected::Status(StatusCode::FOUND)),
        (
            response(StatusCode::NOT_FOUND, b"private-response-body"),
            Expected::Status(StatusCode::NOT_FOUND),
        ),
        (
            oversized_unavailable.into_bytes(),
            Expected::Status(StatusCode::SERVICE_UNAVAILABLE),
        ),
        (
            response(
                StatusCode::OK,
                &vec![b' '; TOKEN_LIMITS.response_body_bytes() + 1],
            ),
            Expected::ResponseLimit,
        ),
    ] {
        *peer.state.resource_response.lock().unwrap() = body;
        let attempts = peer.resource_request_count();
        let error = Registration::prepare_with_oidc_discovery_for_test_http(options.clone())
            .await
            .unwrap_err();
        assert_discovery_error(&error, expected);
        assert_redacted(&error, &[peer.origin.as_str(), "private-response-body"]);
        assert_eq!(peer.resource_request_count(), attempts + 1);
        assert_eq!(peer.token_request_count(), 0);
    }
    peer.finish().await;
}

fn assert_discovery_error(error: &DiscoveryError, expected: Expected) {
    assert!(error.to_string().contains("discovery"));
    match expected {
        Expected::Metadata(reason) => std::assert_matches!(
            error,
            DiscoveryError::Metadata(actual) if *actual == reason
        ),
        Expected::Status(status) => {
            std::assert_matches!(error, DiscoveryError::Status(actual) if *actual == status);
            assert!(error.to_string().contains(status.as_str()));
        }
        Expected::Decode(detail) => {
            std::assert_matches!(error, DiscoveryError::Decode(_));
            let cause = error
                .diagnostic_cause()
                .unwrap()
                .downcast_ref::<serde_json::Error>()
                .unwrap();
            assert!(cause.to_string().contains(detail));
            assert_redacted(error, &[detail]);
        }
        Expected::ResponseLimit => {
            std::assert_matches!(
                error,
                DiscoveryError::Transport(failure) if failure.kind() == TransportFailure::ResponseLimit
            );
            std::assert_matches!(
                error
                    .diagnostic_cause()
                    .unwrap()
                    .downcast_ref::<infra_outbound_http::Error>(),
                Some(infra_outbound_http::Error::ResponseBodyTooLarge)
            );
        }
    }
}

#[tokio::test]
async fn discovery_deadline_is_a_distinct_transport_category() {
    let peer = Fixture::new().await;
    let issuer = peer.origin.join("realms/operations").unwrap().to_string();
    let options = discovery_options(issuer, &peer);
    let gate = peer.block_resources();
    let setup = Registration::prepare_with_oidc_discovery_for_test_http(options);
    tokio::pin!(setup);
    poll_pending(setup.as_mut()).await;
    tokio::select! {
        () = peer.resource_received() => {}
        result = &mut setup => panic!("discovery completed before receiving the request: {result:?}"),
    }
    advance(FETCH_TIMEOUT).await;
    let error = setup.await.unwrap_err();
    std::assert_matches!(
        &error,
        DiscoveryError::Transport(failure) if failure.kind() == TransportFailure::Timeout
    );
    std::assert_matches!(
        error
            .diagnostic_cause()
            .unwrap()
            .downcast_ref::<infra_outbound_http::Error>(),
        Some(infra_outbound_http::Error::Timeout)
    );
    assert!(error.to_string().contains("timed out"));
    gate.add_permits(1);
    peer.finish().await;
}

#[tokio::test]
async fn preflight_reports_a_safe_category_when_connection_closes_before_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let issuer = IssuerUrl::parse(&format!("https://{address}/realms/private-realm")).unwrap();
    let endpoint = TokenUrl::parse(&format!("{}/token", issuer.as_str())).unwrap();
    let close_connection = async {
        let (socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        drop(socket);
    };
    let (result, ()) = tokio::join!(verify_oidc_metadata(&issuer, &endpoint), close_connection);
    let error = result.unwrap_err();
    std::assert_matches!(
        &error,
        DiscoveryError::Transport(failure) if failure.kind() == TransportFailure::Transport
    );
    assert!(error.to_string().contains("transport failed"));
    assert_eq!(
        std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<TransportFailure>()),
        Some(&TransportFailure::Transport),
    );
    assert_redacted(&error, &[&address.to_string(), "private-realm"]);
}

#[test]
fn startup_diagnostics_retain_causes_outside_the_redacted_error_chain() {
    const PRIVATE_DETAIL: &str = "sensitive-provider-detail";
    let source = || Box::new(std::io::Error::other(PRIVATE_DETAIL));
    let build_failure = PreparationError::from(TransportBuildFailure::new(
        "token_url",
        infra_outbound_http::TlsError::for_test(source()),
    ));
    assert!(
        build_failure
            .to_string()
            .contains("TLS configuration failed")
    );
    assert_redacted(&build_failure, &[PRIVATE_DETAIL]);
    assert_eq!(
        build_failure
            .diagnostic_cause()
            .unwrap()
            .source()
            .unwrap()
            .to_string(),
        PRIVATE_DETAIL
    );
    let issuer_failure = DiscoveryError::from(TransportBuildFailure::issuer(
        infra_outbound_http::TlsError::for_test(source()),
    ));
    assert!(issuer_failure.to_string().contains("for issuer"));
    assert_redacted(&issuer_failure, &[PRIVATE_DETAIL]);
    assert_eq!(
        issuer_failure
            .diagnostic_cause()
            .and_then(std::error::Error::source)
            .unwrap()
            .to_string(),
        PRIVATE_DETAIL
    );
    let request_failure = DiscoveryError::Transport(DiscoveryTransportError::new(
        infra_outbound_http::Error::Transport { source: source() },
    ));
    assert_redacted(&request_failure, &[PRIVATE_DETAIL]);
    assert_eq!(
        request_failure
            .diagnostic_cause()
            .and_then(std::error::Error::source)
            .unwrap()
            .to_string(),
        PRIVATE_DETAIL
    );
}

fn assert_redacted(error: &(dyn std::error::Error + 'static), values: &[&str]) {
    for error in shared_rules::error_chain::causes(error) {
        for &value in values {
            assert!(!error.to_string().contains(value));
            assert!(!format!("{error:?}").contains(value));
        }
    }
}
