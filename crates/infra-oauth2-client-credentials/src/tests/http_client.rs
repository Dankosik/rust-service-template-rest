use super::*;
use std::assert_matches;

#[tokio::test]
async fn each_grant_sends_its_own_request_type_and_authorization() {
    let fixture = Fixture::new().await;
    let service = fixture.service_client();
    let delegated = fixture.exchange_client();
    let context = OperationContext::with_timeout(Duration::from_secs(10));
    service.execute(&context, fixture.request()).await.unwrap();
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    delegated
        .execute(&context, fixture.request(), &subject("alice"))
        .await
        .unwrap();
    assert_eq!(
        form_value(&form_pairs(&fixture.token_requests()[0].body), "grant_type"),
        "client_credentials"
    );
    assert_eq!(
        form_value(&form_pairs(&fixture.token_requests()[1].body), "grant_type"),
        GRANT_TYPE_TOKEN_EXCHANGE
    );
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer fixture-token")
    );
    assert_eq!(
        fixture.resource_requests()[1].header("authorization"),
        Some("Bearer exchanged")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_resource_target_refuses_before_acquiring_credentials() {
    let fixture = Fixture::new().await;
    let other = Fixture::new().await;
    assert_eq!(
        fixture
            .service_tokens(&[])
            .http(other.resource_client())
            .unwrap_err()
            .key,
        "resource_origin"
    );
    assert_eq!(
        fixture
            .exchange_tokens(&[], None, EXCHANGE_CAPACITY)
            .http(other.resource_client())
            .unwrap_err()
            .key,
        "resource_origin"
    );
    assert_eq!(other.resource_request_count(), 0);
    other.finish().await;
    let client = fixture.service_client();
    let mut foreign = fixture.request();
    *foreign.uri_mut() = "https://other.example/resource".parse().unwrap();
    let mut spoofed = fixture.request();
    spoofed
        .headers_mut()
        .insert(header::HOST, "other.example".parse().unwrap());
    for request in [foreign, spoofed] {
        assert_matches!(
            fixture.send_request(&client, request).await,
            Err(Error::Resource(
                infra_outbound_http::ExchangeError::BeforeResponse(
                    infra_outbound_http::Error::InvalidTarget
                )
            ))
        );
    }
    assert_eq!(fixture.token_request_count(), 0);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn token_http_telemetry_distinguishes_deadline_from_cancellation() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    let fixture = Fixture::new().await;
    let _gate = fixture.block_tokens();
    for (deadline_expires, expected_error, outbound_outcome) in [
        (true, AcquisitionError::Timeout, "error"),
        (false, AcquisitionError::Cancelled, "cancelled"),
    ] {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let client = fixture.service_client();
        let budget = Duration::from_secs(2);
        let context = OperationContext::with_timeout(budget);
        let mut exchange = Box::pin(client.execute(&context, fixture.request()));
        fixture.drive_until_token_received(exchange.as_mut()).await;
        if deadline_expires {
            advance(budget).await;
        } else {
            context.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), exchange)
            .await
            .expect("the request must stop before the provider timeout");
        let Err(Error::Acquisition(error)) = result else {
            panic!("expected a stopped token acquisition");
        };
        assert_eq!(error, expected_error);

        let durations: Vec<_> = snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .filter_map(|(key, _, _, value)| match value {
                DebugValue::Histogram(samples)
                    if key.key().name() == "http_client_request_duration_seconds" =>
                {
                    Some((key.key().clone(), samples.len()))
                }
                _ => None,
            })
            .collect();
        let [(key, 1)] = durations.as_slice() else {
            panic!("expected one token HTTP request sample, got {durations:?}");
        };
        let label = |name: &str| {
            key.labels()
                .find(|label| label.key() == name)
                .map(metrics::Label::value)
        };
        assert_eq!(label("outbound.outcome"), Some(outbound_outcome), "{key:?}");
        assert_eq!(
            label("error.type"),
            deadline_expires.then_some("timeout"),
            "{key:?}"
        );
    }
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn an_elapsed_deadline_wins_over_request_cancellation_during_token_wait() {
    let fixture = Fixture::new().await;
    let gate = fixture.block_tokens();
    let client = fixture.service_client();
    let operation = OperationContext::with_timeout(Duration::from_secs(1));
    let mut exchange = Box::pin(client.execute(&operation, fixture.request()));
    fixture.drive_until_token_received(exchange.as_mut()).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    operation.cancel();
    assert_matches!(
        exchange.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::time::resume();
    drop(gate);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn an_elapsed_deadline_wins_over_request_cancellation_before_token_work() {
    let fixture = Fixture::new().await;
    let client = fixture.service_client();
    let operation = OperationContext::with_timeout(Duration::ZERO);
    operation.cancel();

    assert_matches!(
        client.execute(&operation, fixture.request()).await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    assert_eq!(fixture.token_request_count(), 0);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn resource_local_ceiling_includes_token_wait_and_resource_wait() {
    let fixture = Fixture::new().await;
    let token_gate = fixture.block_tokens();
    let resource_gate = fixture.block_resources();
    let client = fixture
        .service_tokens(&[])
        .http(
            Client::try_new_for_test_http(
                fixture.origin.as_str(),
                TOKEN_LIMITS
                    .with_operation_timeout(Duration::from_secs(3))
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let context = OperationContext::with_timeout(Duration::from_secs(30));
    let mut call = Box::pin(client.execute(&context, fixture.request()));
    tokio::select! { () = fixture.token_received() => {}, result = &mut call => panic!("token must be gated: {result:?}"), }
    advance(Duration::from_secs(2)).await;
    token_gate.add_permits(1);
    tokio::select! { () = fixture.resource_received() => {}, result = &mut call => panic!("resource must be gated: {result:?}"), }
    advance(Duration::from_secs(2)).await;
    let result = poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx))).await;
    assert_matches!(
        result,
        Poll::Ready(Err(Error::Resource(
            infra_outbound_http::ExchangeError::BeforeResponse(infra_outbound_http::Error::Timeout)
        )))
    );
    drop(call);
    assert_eq!(fixture.token_request_count(), 1);
    assert_eq!(fixture.resource_request_count(), 1);
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
            response(StatusCode::SERVICE_UNAVAILABLE, b"provider-secret-body"),
            AcquisitionError::Unavailable,
        ),
        (
            response(StatusCode::TOO_MANY_REQUESTS, b"provider-secret-body"),
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
            b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n".to_vec(),
            AcquisitionError::InvalidResponse,
        ),
        (
            response(
                StatusCode::BAD_REQUEST,
                br#"{"error":"invalid\u005fclient","error_description":"provider-secret-body"}"#,
            ),
            AcquisitionError::Rejected(Rejection::InvalidClient),
        ),
        (
            response(
                StatusCode::BAD_REQUEST,
                br#"{"error":"provider-secret-body"}"#,
            ),
            AcquisitionError::Rejected(Rejection::Other),
        ),
        (
            response(StatusCode::BAD_REQUEST, br#"{"error":{"invalid_client":null}}"#),
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
                StatusCode::OK,
                &serde_json::json!({"access_token":"token", "token_type":"MAC", "expires_in":3600}),
            ),
            AcquisitionError::InvalidResponse,
        ),
        (
            b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}".to_vec(),
            AcquisitionError::InvalidResponse,
        ),
        (
            // RFC 6749 §5.1
            json_response(
                StatusCode::OK,
                &serde_json::json!({"access_token":"token", "token_type":"Bearer", "expires_in":"3600"}),
            ),
            AcquisitionError::InvalidResponse,
        ),
    ] {
        fixture.token_raw(response);
        let error = fixture
            .service_client()
            .execute(&timeout_context(Duration::from_secs(10)), fixture.request())
            .await
            .unwrap_err();
        assert_matches!(&error, Error::Acquisition(actual) if *actual == expected);
        assert!(!format!("{error:?}").contains("provider-secret-body"));
        assert!(!error.to_string().contains("provider-secret-body"));
    }
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn an_accepted_response_with_a_valid_token_is_refused_until_the_provider_responds_ok() {
    let fixture = Fixture::new().await;
    let client = fixture.service_client();
    let token = serde_json::json!({
        "access_token": "issued-token",
        "token_type": "Bearer",
        "expires_in": 60,
    });
    for response in [
        b"HTTP/1.1 202 Accepted\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{".to_vec(),
        b"HTTP/1.1 202 Accepted\r\ncontent-length: 1048577\r\nconnection: close\r\n\r\n".to_vec(),
        json_response(StatusCode::ACCEPTED, &token),
    ] {
        advance(Duration::from_secs(1)).await;
        fixture.token_raw(response);
        assert_matches!(
            fixture.send(&client).await,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        );
    }

    fixture.token_json(StatusCode::OK, &token);
    assert_matches!(
        fixture.send(&client).await,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    );
    assert_eq!(fixture.token_request_count(), 3);
    assert_eq!(fixture.resource_request_count(), 0);

    advance(Duration::from_secs(1)).await;
    fixture.send(&client).await.unwrap();
    assert_eq!(fixture.token_request_count(), 4);
    let resources = fixture.resource_requests();
    assert_eq!(resources.len(), 1);
    assert_eq!(
        resources[0].header("authorization"),
        Some("Bearer issued-token")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn token_acquisition_stops_at_the_fetch_deadline_without_dispatching_the_resource() {
    let fixture = Fixture::new().await;
    let gate = fixture.block_tokens();
    let client = fixture.service_client();
    let mut timed = Box::pin(fixture.send(&client));
    fixture.drive_until_token_received(timed.as_mut()).await;
    tokio::time::pause();
    tokio::time::advance(FETCH_TIMEOUT).await;
    assert_matches!(
        timed.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::time::resume();
    drop(gate);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn a_stopped_call_preserves_cancellation_and_a_live_call_refuses_conflicting_authorization() {
    let fixture = Fixture::new().await;
    let cancelled = OperationContext::unbounded();
    cancelled.cancel();
    let mut conflicting = fixture.request();
    conflicting.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer caller-token".parse().unwrap(),
    );
    let service = fixture.service_client();
    assert_matches!(
        fixture.send_request(&service, conflicting.clone()).await,
        Err(Error::AuthorizationConflict)
    );
    assert_matches!(
        service.execute(&cancelled, conflicting.clone()).await,
        Err(Error::Acquisition(AcquisitionError::Cancelled))
    );
    let exchange = fixture.exchange_client();
    let alice = subject("subject");
    assert_matches!(
        fixture
            .send_exchange_request(&exchange, conflicting.clone(), "subject")
            .await,
        Err(Error::AuthorizationConflict)
    );
    assert_matches!(
        exchange.execute(&cancelled, conflicting, &alice).await,
        Err(Error::Acquisition(AcquisitionError::Cancelled))
    );
    assert_eq!(fixture.token_request_count(), 0);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn resource_401_and_403_pass_through_and_only_401_evicts_the_token() {
    let fixture = Fixture::new().await;
    for (status, follow_up_fetches) in [(StatusCode::UNAUTHORIZED, 1), (StatusCode::FORBIDDEN, 0)] {
        fixture.resource_status(StatusCode::OK);
        let client = fixture.service_client();
        let before_tokens = fixture.token_request_count();
        let before_resources = fixture.resource_request_count();
        fixture.send(&client).await.unwrap();
        advance(EVICTION_MIN_AGE).await;
        fixture.resource_status(status);
        assert_eq!(
            fixture.send(&client).await.unwrap().status().as_u16(),
            status.as_u16()
        );
        assert_eq!(fixture.token_request_count(), before_tokens + 1);
        assert_eq!(fixture.resource_request_count(), before_resources + 2);
        fixture.send(&client).await.unwrap();
        assert_eq!(
            fixture.token_request_count(),
            before_tokens + 1 + follow_up_fetches
        );
        assert_eq!(fixture.resource_request_count(), before_resources + 3);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn an_oversized_401_evicts_the_token_without_hiding_the_body_error() {
    let fixture = Fixture::new().await;
    let client = fixture
        .service_tokens(&[])
        .http(
            Client::try_new_for_test_http(
                fixture.origin.as_str(),
                TOKEN_LIMITS.with_response_body_bytes(8).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let call = || fixture.send(&client);
    call().await.unwrap();
    advance(EVICTION_MIN_AGE).await;
    *fixture.state.resource_response.lock().unwrap() =
        response(StatusCode::UNAUTHORIZED, b"oversized response");
    assert_matches!(
        call().await,
        Err(Error::Resource(
            infra_outbound_http::ExchangeError::AfterResponse {
                status: StatusCode::UNAUTHORIZED,
                source: infra_outbound_http::Error::ResponseBodyTooLarge,
            }
        ))
    );
    assert_eq!(fixture.resource_request_count(), 2);
    assert_eq!(fixture.token_request_count(), 1);

    fixture.resource_status(StatusCode::OK);
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "replacement", "token_type": "Bearer", "expires_in": 60}),
    );
    call().await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
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
        StatusCode::OK,
        &serde_json::json!({"access_token": "refused", "token_type": "Bearer", "expires_in": 3600}),
    );
    fixture.resource_status(StatusCode::UNAUTHORIZED);
    let client = fixture.service_client();
    for token_requests in [1, 2] {
        for _ in 0..3 {
            let response = fixture.send(&client).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(fixture.token_request_count(), token_requests);
        }
        advance(EVICTION_MIN_AGE).await;
        fixture.send(&client).await.unwrap();
    }
    fixture.finish().await;
}

#[tokio::test]
async fn a_late_401_does_not_evict_a_newer_token() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let service_tokens = fixture.service_tokens(&[]);
    let client = service_tokens.http(fixture.resource_client()).unwrap();
    service_tokens
        .service_token(acquisition_deadline(&token_context()))
        .await
        .unwrap();
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status(StatusCode::UNAUTHORIZED);
    let gate = fixture.block_resources();
    let mut stale = Box::pin(fixture.send(&client));
    tokio::select! { () = fixture.resource_received() => {}, result = &mut stale => panic!("response must be gated: {result:?}"), }

    service_tokens.clear_cached();
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    service_tokens
        .service_token(acquisition_deadline(&token_context()))
        .await
        .unwrap();

    gate.add_permits(1);
    assert_eq!(stale.await.unwrap().status(), StatusCode::UNAUTHORIZED);
    *fixture.state.resource_gate.lock().unwrap() = None;
    fixture.resource_status(StatusCode::OK);
    fixture.send(&client).await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
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
