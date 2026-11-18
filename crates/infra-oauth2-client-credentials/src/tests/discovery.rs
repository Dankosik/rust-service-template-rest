use super::*;
use crate::{ConfigurationError, KeycloakError, TransportFailure, verify_keycloak_metadata};

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
    use KeycloakError::{Discovery, DiscoveryStatus, DiscoveryTransport};

    let peer = Fixture::new().await;
    let issuer = peer.origin.join("realms/operations").unwrap().to_string();
    let options = Options {
        assertion_audience: issuer.clone(),
        ..peer.options()
    };
    assert!(matches!(
        Registration::prepare_keycloak(options.clone())
            .await
            .unwrap_err(),
        KeycloakError::Configuration(ConfigurationError {
            key: "assertion_audience",
            ..
        })
    ));
    assert!(matches!(
        Registration::prepare_keycloak(Options {
            assertion_audience: ASSERTION_AUDIENCE.to_owned(),
            ..options.clone()
        })
        .await
        .unwrap_err(),
        KeycloakError::Configuration(ConfigurationError {
            key: "token_url",
            ..
        })
    ));
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
    let oversized_unavailable = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        TOKEN_LIMITS.response_body_bytes + 1,
    );
    for (body, expected) in [
        (
            metadata(&issuer.replacen("http:", "HTTP:", 1), &options.token_url),
            Discovery("issuer differs from assertion_audience"),
        ),
        (
            metadata(&issuer, peer.origin.join("other").unwrap().as_str()),
            Discovery("token_endpoint is invalid or differs from token_url"),
        ),
        (
            metadata(&issuer, &options.token_url.replacen("://", "://@", 1)),
            Discovery("token_endpoint is invalid or differs from token_url"),
        ),
        (
            json_response(StatusCode::OK, &serde_json::json!({"issuer": issuer})),
            Discovery("invalid issuer metadata"),
        ),
        (
            json_response(
                StatusCode::OK,
                &serde_json::json!([issuer, options.token_url]),
            ),
            Discovery("invalid issuer metadata"),
        ),
        (redirect.into_bytes(), DiscoveryStatus(StatusCode::FOUND)),
        (
            response(StatusCode::NOT_FOUND, b"private-response-body"),
            DiscoveryStatus(StatusCode::NOT_FOUND),
        ),
        (
            oversized_unavailable.into_bytes(),
            DiscoveryStatus(StatusCode::SERVICE_UNAVAILABLE),
        ),
        (
            response(
                StatusCode::OK,
                &vec![b' '; TOKEN_LIMITS.response_body_bytes + 1],
            ),
            DiscoveryTransport(TransportFailure::ResponseLimit),
        ),
    ] {
        *peer.state.resource_response.lock().unwrap() = body;
        let attempts = peer.resource_requests().len();
        let error = Registration::prepare_keycloak_for_test_http(options.clone())
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(error.to_string().contains("discovery"));
        if let DiscoveryStatus(status) = expected {
            assert!(error.to_string().contains(status.as_str()));
        }
        assert_redacted(&error, &[peer.origin.as_str(), "private-response-body"]);
        assert_eq!(peer.resource_requests().len(), attempts + 1);
        assert!(peer.token_requests().is_empty());
    }
    peer.finish().await;
}

#[tokio::test]
async fn discovery_deadline_is_a_distinct_transport_category() {
    let peer = Fixture::new().await;
    let options = Options {
        assertion_audience: peer.origin.join("realms/operations").unwrap().to_string(),
        ..peer.options()
    };
    let _gate = peer.block_resources();
    let setup = Registration::prepare_keycloak_for_test_http(options);
    tokio::pin!(setup);
    poll_pending(setup.as_mut()).await;
    tokio::select! {
        () = peer.resource_received() => {}
        result = &mut setup => panic!("discovery completed before receiving the request: {result:?}"),
    }
    advance(FETCH_TIMEOUT).await;
    let error = setup.await.unwrap_err();
    assert_eq!(
        error,
        KeycloakError::DiscoveryTransport(TransportFailure::Timeout)
    );
    assert!(error.to_string().contains("timed out"));
    peer.finish().await;
}

#[tokio::test]
async fn preflight_reports_a_safe_category_when_connection_closes_before_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let issuer = format!("https://{address}/realms/private-realm");
    let endpoint = format!("{issuer}/token");
    let close_connection = async {
        let (socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        drop(socket);
    };
    let (result, ()) = tokio::join!(
        verify_keycloak_metadata(&issuer, &endpoint),
        close_connection
    );
    let error = result.unwrap_err();
    assert_eq!(
        error,
        KeycloakError::DiscoveryTransport(TransportFailure::Transport)
    );
    assert!(error.to_string().contains("transport failed"));
    assert_eq!(
        std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<TransportFailure>()),
        Some(&TransportFailure::Transport),
    );
    assert_redacted(&error, &[&address.to_string(), "private-realm"]);
}

fn assert_redacted(error: &KeycloakError, values: &[&str]) {
    for error in shared_rules::error_chain::causes(error) {
        for &value in values {
            assert!(!error.to_string().contains(value));
            assert!(!format!("{error:?}").contains(value));
        }
    }
}
