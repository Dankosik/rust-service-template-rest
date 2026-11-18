use super::*;

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
    assert_eq!(
        Registration::prepare_keycloak(options.clone())
            .await
            .unwrap_err()
            .key,
        "assertion_audience"
    );
    assert_eq!(
        Registration::prepare_keycloak(Options {
            assertion_audience: ASSERTION_AUDIENCE.to_owned(),
            ..options.clone()
        })
        .await
        .unwrap_err()
        .key,
        "token_url"
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
            "issuer discovery request failed",
        ),
    ] {
        *peer.state.resource_response.lock().unwrap() = body;
        let attempts = peer.resource_requests().len();
        let error = Registration::prepare_keycloak_for_test_http(options.clone())
            .await
            .unwrap_err();
        assert_eq!(error.reason, reason);
        assert_eq!(peer.resource_requests().len(), attempts + 1);
        assert!(peer.token_requests().is_empty());
    }
    peer.finish().await;
}
