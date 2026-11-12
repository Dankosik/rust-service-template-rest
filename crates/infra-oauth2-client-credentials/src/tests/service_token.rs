use super::*;
use std::assert_matches;

#[tokio::test]
async fn zero_or_expired_during_acquisition_never_authorizes_dispatch() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token":"expired", "token_type":"Bearer", "expires_in":0}),
    );
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    assert_matches!(
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    );
    fixture.token_received().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token":"late", "token_type":"Bearer", "expires_in":1}),
    );
    // A new owner isolates expiry admission from the prior completed failure.
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut operation =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut operation => panic!("response must be gated: {result:?}"), }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    gate.add_permits(1);
    assert_matches!(
        operation.await,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    );
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn clones_reuse_one_owner_cache_but_independent_owners_do_not_share() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let credentials = fixture.credentials(&[], None);
    credentials
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    credentials
        .clone()
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    fixture
        .credentials(&[], None)
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    let authorizations = fixture
        .resource_requests()
        .into_iter()
        .map(|request| request.header("authorization").unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        authorizations,
        ["Bearer first", "Bearer first", "Bearer second"]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_waiter_reuses_a_shared_failure_but_not_a_request_only_token() {
    let fixture = Fixture::new().await;
    for (body, expected_error, waiter_fetches) in [
        (
            &serde_json::json!({"access_token": "retained", "token_type": "Bearer", "expires_in": 60}),
            None,
            0,
        ),
        (
            &serde_json::json!({"access_token": "short", "token_type": "Bearer", "expires_in": 10}),
            None,
            1,
        ),
        (
            &serde_json::json!({"error": "provider-secret"}),
            Some(AcquisitionError::InvalidResponse),
            0,
        ),
    ] {
        fixture.token_json("200 OK", body);
        let client = fixture
            .prepare(Options {
                provider_concurrency: 1,
                ..fixture.options(&[], None)
            })
            .http(fixture.resource_client());
        let gate = fixture.block_tokens();
        let before_tokens = fixture.token_requests().len();
        let mut leader =
            Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
        tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
        let mut waiter =
            Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
        poll_pending(waiter.as_mut()).await;
        // The waiter queues behind the leader instead of sending its own request.
        assert_eq!(fixture.token_requests().len(), before_tokens + 1);
        gate.add_permits(2);
        let (leader, waiter) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(leader, waiter)
        })
        .await
        .unwrap();
        for result in [leader, waiter] {
            match expected_error {
                Some(expected) => {
                    assert_matches!(result, Err(Error::Acquisition(error)) if error == expected);
                }
                None => assert!(result.is_ok()),
            }
        }
        assert_eq!(
            fixture.token_requests().len(),
            before_tokens + 1 + waiter_fetches
        );
        *fixture.state.token_gate.lock().unwrap() = None;
        if expected_error.is_some() {
            advance(Duration::from_secs(1)).await;
            fixture.token_json(
                "200 OK",
                &serde_json::json!({"access_token": "recovered", "token_type": "Bearer", "expires_in": 60}),
            );
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
            assert_eq!(fixture.token_requests().len(), before_tokens + 2);
        }
    }
    fixture.finish().await;
}

#[tokio::test]
async fn cancelling_service_token_acquisition_allows_a_waiter_to_retry() {
    let fixture = Fixture::new().await;
    let client = fixture
        .prepare(Options {
            provider_concurrency: 1,
            ..fixture.options(&[], None)
        })
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let context = OperationContext::with_timeout(Duration::from_secs(10));
    let mut leader = Box::pin(client.execute_with_context(fixture.request(), &context));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut survivor =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    poll_pending(survivor.as_mut()).await;
    context.cancel();
    assert_matches!(
        leader.await,
        Err(Error::Acquisition(AcquisitionError::Cancelled))
    );
    tokio::select! { () = fixture.token_received() => {}, result = &mut survivor => panic!("replacement must be gated: {result:?}"), }
    gate.add_permits(2);
    survivor.await.unwrap();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn a_silent_token_fault_releases_admission_for_a_later_restored_call() {
    let fixture = Fixture::new().await;
    let credentials = fixture.credentials(&[], None);
    let client = credentials.http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .expect("available dependency establishes useful work");
    *credentials.cached() = Cached::default();
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(1))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut waiter = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    poll_pending(waiter.as_mut()).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_matches!(
        leader.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::time::resume();
    tokio::select! { () = fixture.token_received() => {}, result = &mut waiter => panic!("replacement must be gated: {result:?}"), }
    gate.add_permits(2);
    waiter.await.unwrap();
    assert_eq!(fixture.token_requests().len(), 3);
    assert_eq!(fixture.resource_requests().len(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn a_provider_silent_past_the_fetch_bound_is_unavailable_until_the_failure_window_ends() {
    let fixture = Fixture::new().await;
    let client = fixture.credentials(&[], None).http(
        Client::try_new_for_test_http(
            &fixture.origin,
            infra_outbound_http::Limits {
                operation_timeout: Duration::from_secs(10),
                ..TOKEN_LIMITS
            },
        )
        .unwrap(),
    );
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut waiter = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    poll_pending(waiter.as_mut()).await;
    tokio::time::pause();
    tokio::time::advance(FETCH_TIMEOUT).await;
    assert_matches!(
        leader.await,
        Err(Error::Acquisition(AcquisitionError::Unavailable))
    );
    tokio::time::resume();
    assert_matches!(
        waiter.await,
        Err(Error::Acquisition(AcquisitionError::Unavailable))
    );
    assert_eq!(fixture.token_requests().len(), 1);
    *fixture.state.token_gate.lock().unwrap() = None;
    advance(Duration::from_secs(1)).await;
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    drop(gate);
    fixture.finish().await;
}

#[tokio::test]
async fn the_fetch_cap_is_unavailable_with_an_unrepresentable_resource_deadline() {
    let fixture = Fixture::new().await;
    let resource = Client::try_new_for_test_http(
        &fixture.origin,
        infra_outbound_http::Limits {
            operation_timeout: Duration::MAX,
            ..TOKEN_LIMITS
        },
    )
    .unwrap();
    let client = fixture.credentials(&[], None).http(resource);
    let context = OperationContext::unbounded();
    let _gate = fixture.block_tokens();

    for request in [fixture.request(), fixture.on_behalf_of_request("subject")] {
        let mut operation = Box::pin(client.execute_with_context(request, &context));
        tokio::select! {
            () = fixture.token_received() => {}
            result = &mut operation => panic!("response must be gated: {result:?}"),
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_matches!(
            operation.await,
            Err(Error::Acquisition(AcquisitionError::Unavailable))
        );
        tokio::time::resume();
    }

    assert_eq!(fixture.token_requests().len(), 2);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn each_waiter_keeps_its_own_deadline_without_cancelling_the_leader() {
    let fixture = Fixture::new().await;
    let client = fixture
        .prepare(Options {
            provider_concurrency: 1,
            ..fixture.options(&[], None)
        })
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut short_waiter =
        Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(1))));
    poll_pending(short_waiter.as_mut()).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_matches!(
        short_waiter.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::time::resume();
    gate.add_permits(1);
    leader.await.unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    assert_eq!(fixture.resource_requests().len(), 1);
    fixture.finish().await;
}
