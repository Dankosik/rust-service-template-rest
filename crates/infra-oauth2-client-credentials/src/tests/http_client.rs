use super::*;
use std::assert_matches;

#[tokio::test]
async fn a_client_requiring_a_subject_refuses_a_request_without_one_before_any_io() {
    let fixture = Fixture::new().await;
    let credentials = fixture.credentials(&[], None);
    // A reusable service token is cached, so a fallback would have one to send.
    credentials
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    assert_eq!(fixture.resource_requests().len(), 1);

    let client = credentials
        .http(fixture.resource_client())
        .require_on_behalf_of();
    let error = client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap_err();
    assert_matches!(error, Error::SubjectRequired);
    assert_eq!(fixture.token_requests().len(), 1);
    assert_eq!(fixture.resource_requests().len(), 1);

    fixture.token_json("200 OK", &exchange_response("exchanged"));
    client
        .execute(
            fixture.on_behalf_of_request("alice"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(
        form_value(&form_pairs(&fixture.token_requests()[1].body), "grant_type"),
        GRANT_TYPE_TOKEN_EXCHANGE
    );
    assert_eq!(
        fixture.resource_requests()[1].header("authorization"),
        Some("Bearer exchanged")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn an_elapsed_deadline_wins_over_request_cancellation_during_token_wait() {
    let fixture = Fixture::new().await;
    let gate = fixture.block_tokens();
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let operation = OperationContext::with_timeout(Duration::from_secs(1));
    let request_context = OperationContext::unbounded();
    let mut request = fixture.request();
    request.extensions_mut().insert(request_context.clone());
    let mut exchange = Box::pin(client.execute_with_context(request, &operation));
    tokio::select! { () = fixture.token_received() => {}, result = &mut exchange => panic!("response must be gated: {result:?}"), }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    request_context.cancel();
    assert_matches!(
        exchange.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::time::resume();
    drop(gate);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn an_elapsed_deadline_wins_over_request_cancellation_before_token_work() {
    let fixture = Fixture::new().await;
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let operation = OperationContext::with_timeout(Duration::ZERO);
    let request_context = OperationContext::unbounded();
    request_context.cancel();
    let mut request = fixture.request();
    request.extensions_mut().insert(request_context);

    assert_matches!(
        client.execute_with_context(request, &operation).await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    assert!(fixture.token_requests().is_empty());
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn resource_local_ceiling_includes_token_wait_and_resource_wait() {
    let fixture = Fixture::new().await;
    let token_gate = fixture.block_tokens();
    let resource_gate = fixture.block_resources();
    let client = fixture.credentials(&[], None).http(
        Client::try_new_for_test_http(
            &fixture.origin,
            infra_outbound_http::Limits {
                operation_timeout: Duration::from_secs(3),
                ..TOKEN_LIMITS
            },
        )
        .unwrap(),
    );
    let context = OperationContext::with_timeout(Duration::from_secs(30));
    let mut call = Box::pin(client.execute_with_context(fixture.request(), &context));
    tokio::select! { () = fixture.token_received() => {}, result = &mut call => panic!("token must be gated: {result:?}"), }
    advance(Duration::from_secs(2)).await;
    token_gate.add_permits(1);
    tokio::select! { () = fixture.resource_received() => {}, result = &mut call => panic!("resource must be gated: {result:?}"), }
    advance(Duration::from_secs(2)).await;
    let result = poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx))).await;
    assert_matches!(
        result,
        Poll::Ready(Err(Error::Resource(infra_outbound_http::Error::Timeout)))
    );
    drop(call);
    assert_eq!(fixture.token_requests().len(), 1);
    assert_eq!(fixture.resource_requests().len(), 1);
    resource_gate.add_permits(1);
    fixture.finish().await;
}

#[tokio::test]
async fn token_failures_are_sanitized_and_never_dispatch_the_resource() {
    let fixture = Fixture::new().await;
    for (response, expected) in [
        (
            b"HTTP/1.1 302 Found\r\nlocation: http://redirected.example/token\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::Rejected(Rejection::Other),
        ),
        (
            response("503 Service Unavailable", b"provider-secret-body"),
            AcquisitionError::Unavailable,
        ),
        (
            response("429 Too Many Requests", b"provider-secret-body"),
            AcquisitionError::Unavailable,
        ),
        (
            b"HTTP/1.1 429 Too Many Requests\r\ncontent-length: 1048577\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::Unavailable,
        ),
        (
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 1048577\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::Unavailable,
        ),
        (
            b"HTTP/1.1 429 Too Many Requests\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{".to_vec(),
            AcquisitionError::Unavailable,
        ),
        (
            b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{".to_vec(),
            AcquisitionError::Unavailable,
        ),
        (
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{".to_vec(),
            AcquisitionError::Transport,
        ),
        (
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 1048577\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::ResponseLimit,
        ),
        (
            response(
                "400 Bad Request",
                br#"{"error":"invalid_client","error_description":"provider-secret-body"}"#,
            ),
            AcquisitionError::Rejected(Rejection::InvalidClient),
        ),
        (
            response(
                "400 Bad Request",
                br#"{"error":"provider-secret-body"}"#,
            ),
            AcquisitionError::Rejected(Rejection::Other),
        ),
        (
            b"HTTP/1.1 400 Bad Request\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{".to_vec(),
            AcquisitionError::Rejected(Rejection::Other),
        ),
        (
            b"HTTP/1.1 400 Bad Request\r\ncontent-length: 1048577\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::Rejected(Rejection::Other),
        ),
        (
            json_response(
                "200 OK",
                &serde_json::json!({"access_token":"token", "token_type":"MAC", "expires_in":3600}),
            ),
            AcquisitionError::InvalidResponse,
        ),
        (
            b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_vec(),
            AcquisitionError::InvalidResponse,
        ),
        (
            // RFC 6749 section 5.1 defines a number; a string is not admitted.
            json_response(
                "200 OK",
                &serde_json::json!({"access_token":"token", "token_type":"Bearer", "expires_in":"3600"}),
            ),
            AcquisitionError::InvalidResponse,
        ),
    ] {
        fixture.token_raw(response);
        let error = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap_err();
        assert_matches!(&error, Error::Acquisition(actual) if *actual == expected);
        assert!(!format!("{error:?}").contains("provider-secret-body"));
        assert!(!error.to_string().contains("provider-secret-body"));
    }
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn token_acquisition_stops_at_the_fetch_deadline_without_dispatching_the_resource() {
    let fixture = Fixture::new().await;
    let gate = fixture.block_tokens();
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let mut timed = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut timed => panic!("response must be gated: {result:?}"), }
    tokio::time::pause();
    tokio::time::advance(FETCH_TIMEOUT).await;
    assert_matches!(
        timed.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::time::resume();
    drop(gate);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn caller_authorization_conflict_refuses_before_token_or_resource_io() {
    let fixture = Fixture::new().await;
    for mut conflicting in [fixture.request(), fixture.on_behalf_of_request("subject")] {
        conflicting.headers_mut().insert(
            header::AUTHORIZATION,
            "Bearer caller-token".parse().unwrap(),
        );
        assert_matches!(
            fixture
                .credentials(&[], None)
                .http(fixture.resource_client())
                .execute(conflicting, deadline(Duration::from_secs(10)))
                .await,
            Err(Error::AuthorizationConflict)
        );
    }
    assert!(fixture.token_requests().is_empty());
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn resource_401_and_403_pass_through_and_only_401_evicts_the_token() {
    let fixture = Fixture::new().await;
    for (status, follow_up_fetches) in [("401 Unauthorized", 1), ("403 Forbidden", 0)] {
        fixture.resource_status("200 OK");
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let before_tokens = fixture.token_requests().len();
        let before_resources = fixture.resource_requests().len();
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
        advance(EVICTION_MIN_AGE).await;
        fixture.resource_status(status);
        assert_eq!(
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap()
                .status()
                .as_u16(),
            status[..3].parse::<u16>().unwrap()
        );
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        assert_eq!(fixture.resource_requests().len(), before_resources + 2);
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
        assert_eq!(
            fixture.token_requests().len(),
            before_tokens + 1 + follow_up_fetches
        );
        assert_eq!(fixture.resource_requests().len(), before_resources + 3);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn an_oversized_401_evicts_the_token_without_hiding_the_body_error() {
    let fixture = Fixture::new().await;
    let client = fixture.credentials(&[], None).http(
        Client::try_new_for_test_http(
            &fixture.origin,
            infra_outbound_http::Limits {
                response_body_bytes: 8,
                ..TOKEN_LIMITS
            },
        )
        .unwrap(),
    );
    let call = || client.execute(fixture.request(), deadline(Duration::from_secs(10)));
    call().await.unwrap();
    advance(EVICTION_MIN_AGE).await;
    *fixture.state.resource_response.lock().unwrap() =
        response("401 Unauthorized", b"oversized response");
    assert_matches!(
        call().await,
        Err(Error::Resource(
            infra_outbound_http::Error::ResponseBodyTooLarge
        ))
    );
    assert_eq!(fixture.resource_requests().len(), 2);
    assert_eq!(fixture.token_requests().len(), 1);

    fixture.resource_status("200 OK");
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "replacement", "token_type": "Bearer", "expires_in": 60}),
    );
    call().await.unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(
        fixture.resource_requests()[2].header("authorization"),
        Some("Bearer replacement")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_resource_that_refuses_every_token_costs_one_token_request_per_eviction_age() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "refused", "token_type": "Bearer", "expires_in": 3600}),
    );
    fixture.resource_status("401 Unauthorized");
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    for token_requests in [1, 2] {
        // Every call is refused, yet a token younger than the eviction age
        // is kept instead of being requested again.
        for _ in 0..3 {
            let response = client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(fixture.token_requests().len(), token_requests);
        }
        advance(EVICTION_MIN_AGE).await;
        // Old enough now: this 401 evicts it for the next round.
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    fixture.finish().await;
}

#[tokio::test]
async fn a_late_401_does_not_evict_a_newer_token() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let credentials = fixture.credentials(&[], None);
    let client = credentials.http(fixture.resource_client());
    // Old enough that only its replacement keeps the late 401 from evicting.
    credentials
        .service_token(AcquisitionDeadline::caller(deadline(Duration::from_secs(
            10,
        ))))
        .await
        .unwrap();
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status("401 Unauthorized");
    let gate = fixture.block_resources();
    let mut stale = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.resource_received() => {}, result = &mut stale => panic!("response must be gated: {result:?}"), }

    *credentials.cached() = Cached::default();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    credentials
        .service_token(AcquisitionDeadline::caller(deadline(Duration::from_secs(
            10,
        ))))
        .await
        .unwrap();

    gate.add_permits(1);
    assert_eq!(stale.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    *fixture.state.resource_gate.lock().unwrap() = None;
    fixture.resource_status("200 OK");
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(
        fixture
            .resource_requests()
            .last()
            .unwrap()
            .header("authorization"),
        Some("Bearer second")
    );
    fixture.finish().await;
}
