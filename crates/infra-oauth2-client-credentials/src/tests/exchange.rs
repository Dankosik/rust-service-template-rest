use super::*;
use std::assert_matches;

#[tokio::test]
async fn an_exchanged_token_is_reused_per_subject_but_not_across_subjects() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("first"));
    let client = fixture.client();
    fixture
        .send_request(&client, fixture.on_behalf_of_request("alice"))
        .await
        .unwrap();
    fixture
        .send_request(&client, fixture.on_behalf_of_request("alice"))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    fixture.token_json(StatusCode::OK, &exchange_response("second"));
    fixture
        .send_request(&client, fixture.on_behalf_of_request("bob"))
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
async fn the_configured_capacity_bounds_how_many_subjects_keep_their_exchanged_token() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    let credentials = fixture.prepare_with(|options| options.exchange_cache_capacity = 1);
    let client = credentials.http(fixture.resource_client());
    let cache = &credentials.0.inner.exchange.cache;
    for subject in ["alice", "bob"] {
        fixture
            .send_request(&client, fixture.on_behalf_of_request(subject))
            .await
            .unwrap();
    }
    assert_eq!(fixture.token_requests().len(), 2);
    cache.run_pending_tasks().await;
    assert!(cache.entry_count() <= 1);
    for subject in ["alice", "bob"] {
        fixture
            .send_request(&client, fixture.on_behalf_of_request(subject))
            .await
            .unwrap();
        cache.run_pending_tasks().await;
    }
    assert!(fixture.token_requests().len() > 2);
    assert!(cache.entry_count() <= 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_capacity_that_does_not_divide_the_payload_target_keeps_every_configured_subject() {
    const SUBJECTS: u32 = 5;
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    let credentials = fixture.prepare_with(|options| options.exchange_cache_capacity = SUBJECTS);
    let client = credentials.http(fixture.resource_client());
    let cache = &credentials.0.inner.exchange.cache;
    for _ in 0..2 {
        for subject in 0..SUBJECTS {
            fixture
                .send_request(
                    &client,
                    fixture.on_behalf_of_request(&format!("subject-{subject}")),
                )
                .await
                .unwrap();
            cache.run_pending_tasks().await;
        }
    }
    assert_eq!(fixture.token_requests().len(), SUBJECTS as usize);
    assert_eq!(cache.entry_count(), u64::from(SUBJECTS));
    fixture.finish().await;
}

#[tokio::test]
async fn exchanged_cache_evicts_large_payloads_before_the_entry_target() {
    const PAYLOAD_TARGET: usize = 16 * 1024 * 1024;
    let fixture = Fixture::new().await;
    let credentials = fixture.credentials(&[], None);
    let client = credentials.http(fixture.resource_client());
    let prefix = "x".repeat(512 * 1024 - 16);
    for index in 0..33 {
        fixture.token_json(
            StatusCode::OK,
            &exchange_response(&format!("{prefix}-{index:02}")),
        );
        fixture
            .send_request(
                &client,
                fixture.on_behalf_of_request(&format!("subject-{index}")),
            )
            .await
            .unwrap();
    }
    let cache = &credentials.0.inner.exchange.cache;
    cache.run_pending_tasks().await;
    let payload = cache
        .iter()
        .map(|(_, token)| token.header.as_bytes().len())
        .sum::<usize>();
    assert!(
        cache.entry_count() < 33,
        "payload pressure retained every entry"
    );
    assert!(payload <= PAYLOAD_TARGET, "retained {payload} bytes");
    assert!(cache.weighted_size() <= PAYLOAD_TARGET as u64);
    fixture.finish().await;
}

#[tokio::test]
async fn a_short_lived_exchanged_token_serves_its_own_request_after_one_exchange() {
    let fixture = Fixture::new().await;
    let mut short = exchange_response("short");
    short["expires_in"] = serde_json::json!(5);
    fixture.token_json(StatusCode::OK, &short);
    let client = fixture.client();
    for expected_exchanges in 1..=2 {
        fixture
            .send_request(&client, fixture.on_behalf_of_request("alice"))
            .await
            .unwrap();
        assert_eq!(fixture.token_requests().len(), expected_exchanges);
    }
    fixture.finish().await;
}

#[tokio::test]
async fn an_exchanged_token_without_lifetime_is_request_only() {
    let fixture = Fixture::new().await;
    let mut response = exchange_response("request-only");
    response.as_object_mut().unwrap().remove("expires_in");
    fixture.token_json(StatusCode::OK, &response);
    let client = fixture.client();
    for _ in 0..2 {
        fixture
            .send_request(&client, fixture.on_behalf_of_request("subject"))
            .await
            .unwrap();
    }
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(fixture.resource_requests().len(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn an_exchanged_token_past_its_reuse_cutoff_is_exchanged_once_more() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("first"));
    let client = fixture.client();
    let execute = || async {
        fixture
            .send_request(&client, fixture.on_behalf_of_request("alice"))
            .await
            .unwrap();
    };
    execute().await;
    // Only the Tokio clock moves past the cutoff of a sixty-second token, so
    // the cache still holds the entry and the reuse check alone refuses it.
    advance(Duration::from_secs(51)).await;
    fixture.token_json(StatusCode::OK, &exchange_response("second"));
    execute().await;
    execute().await;
    assert_eq!(fixture.token_requests().len(), 2);
    let authorizations = fixture
        .resource_requests()
        .into_iter()
        .map(|request| request.header("authorization").unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        authorizations,
        ["Bearer first", "Bearer second", "Bearer second"]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn concurrent_on_behalf_of_calls_for_one_subject_share_one_exchange() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("shared"));
    let client = fixture
        .prepare_with(|options| options.provider_concurrency = 1)
        .http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader =
        Box::pin(fixture.send_request(&client, fixture.on_behalf_of_request("subject")));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut waiter =
        Box::pin(fixture.send_request(&client, fixture.on_behalf_of_request("subject")));
    poll_pending(waiter.as_mut()).await;
    assert_eq!(fixture.token_requests().len(), 1);
    gate.add_permits(2);
    let (leader, waiter) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(leader, waiter)
    })
    .await
    .unwrap();
    leader.unwrap();
    waiter.unwrap();
    assert_eq!(fixture.token_requests().len(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn provider_capacity_is_shared_across_grants_but_hits_and_waiters_do_not_consume_it() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("shared"));
    let credentials = fixture.prepare_with(|options| options.provider_concurrency = 2);
    let client = credentials.http(fixture.resource_client());
    fixture
        .send_request(&client, fixture.on_behalf_of_request("cached"))
        .await
        .unwrap();
    let gate = fixture.block_tokens();
    let mut service = Box::pin(fixture.send(&client));
    tokio::select! { () = fixture.token_received() => {}, result = &mut service => panic!("service must be gated: {result:?}"), }
    let other_binding = credentials.clone().http(fixture.resource_client());
    let mut exchange =
        Box::pin(fixture.send_request(&other_binding, fixture.on_behalf_of_request("active")));
    tokio::select! { () = fixture.token_received() => {}, result = &mut exchange => panic!("exchange must be gated: {result:?}"), }
    let mut waiter =
        Box::pin(fixture.send_request(&client, fixture.on_behalf_of_request("active")));
    poll_pending(waiter.as_mut()).await;
    drop(waiter);
    for _ in 0..2 {
        assert_matches!(
            fixture
                .send_request(&client, fixture.on_behalf_of_request("refused"))
                .await,
            Err(Error::Acquisition(AcquisitionError::AtCapacity))
        );
    }
    assert_matches!(
        client
            .execute(fixture.on_behalf_of_request("expired"), Instant::now())
            .await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    assert_eq!(fixture.token_requests().len(), 3);
    assert_eq!(fixture.resource_requests().len(), 1);
    fixture
        .send_request(&client, fixture.on_behalf_of_request("cached"))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 3);
    gate.add_permits(2);
    let (service, exchange) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(service, exchange)
    })
    .await
    .unwrap();
    service.unwrap();
    exchange.unwrap();
    *fixture.state.token_gate.lock().unwrap() = None;
    fixture
        .send_request(&client, fixture.on_behalf_of_request("refused"))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 4);
    assert_eq!(fixture.resource_requests().len(), 5);
    fixture.finish().await;
}

#[tokio::test]
async fn cancelling_an_exchange_releases_capacity_without_releasing_a_live_waiters_leader() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("replacement"));
    let credentials = fixture.prepare_with(|options| options.provider_concurrency = 1);
    let client = credentials.http(fixture.resource_client());
    let gate = fixture.block_tokens();
    let mut leader =
        Box::pin(fixture.send_request(&client, fixture.on_behalf_of_request("subject")));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("leader must be gated: {result:?}"), }
    let context = OperationContext::with_timeout(Duration::from_secs(10));
    let mut request = fixture.on_behalf_of_request("subject");
    request.extensions_mut().insert(context.clone());
    let mut waiter = Box::pin(fixture.send_request(&client, request));
    poll_pending(waiter.as_mut()).await;
    context.cancel();
    assert_matches!(
        waiter.await,
        Err(Error::Acquisition(AcquisitionError::Cancelled))
    );
    assert_matches!(
        fixture.send(&client).await,
        Err(Error::Acquisition(AcquisitionError::AtCapacity))
    );
    assert_matches!(
        client.execute(fixture.request(), Instant::now()).await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    assert_eq!(fixture.token_requests().len(), 1);
    assert!(fixture.resource_requests().is_empty());
    drop(leader);
    *fixture.state.token_gate.lock().unwrap() = None;
    fixture.send(&client).await.unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(fixture.resource_requests().len(), 1);
    drop(gate);
    fixture.finish().await;
}

#[tokio::test]
async fn a_waiter_gets_its_own_attempt_budget_after_its_leader_times_out() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    let resource = Client::try_new_for_test_http(
        &fixture.origin,
        infra_outbound_http::Limits {
            operation_timeout: Duration::from_secs(10),
            ..TOKEN_LIMITS
        },
    )
    .unwrap();
    let client = fixture
        .prepare_with(|options| options.provider_concurrency = 1)
        .http(resource);
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(client.execute(
        fixture.on_behalf_of_request("subject"),
        deadline(Duration::from_secs(4)),
    ));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("response must be gated: {result:?}"), }
    let mut waiter =
        Box::pin(fixture.send_request(&client, fixture.on_behalf_of_request("subject")));
    poll_pending(waiter.as_mut()).await;
    advance(Duration::from_secs(4)).await;
    assert_matches!(
        leader.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    tokio::select! { () = fixture.token_received() => {}, result = &mut waiter => panic!("replacement must be gated: {result:?}"), }
    advance(Duration::from_secs(2)).await;
    gate.add_permits(2);
    waiter.await.unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(fixture.resource_requests().len(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_resource_401_evicts_only_that_subjects_exchanged_token() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("shared"));
    let client = fixture.client();
    fixture
        .send_request(&client, fixture.on_behalf_of_request("alice"))
        .await
        .unwrap();
    fixture
        .send_request(&client, fixture.on_behalf_of_request("bob"))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status(StatusCode::UNAUTHORIZED);
    fixture
        .send_request(&client, fixture.on_behalf_of_request("alice"))
        .await
        .unwrap();
    fixture.resource_status(StatusCode::OK);
    let before = fixture.token_requests().len();
    fixture
        .send_request(&client, fixture.on_behalf_of_request("alice"))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), before + 1);
    fixture
        .send_request(&client, fixture.on_behalf_of_request("bob"))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), before + 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_failed_exchange_is_not_cached() {
    let fixture = Fixture::new().await;
    fixture.token_raw(response(StatusCode::INTERNAL_SERVER_ERROR, b"{}"));
    let client = fixture
        .prepare_with(|options| options.provider_concurrency = 1)
        .http(fixture.resource_client());
    for _ in 0..2 {
        let result = fixture
            .send_request(&client, fixture.on_behalf_of_request("subject"))
            .await;
        assert_matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::Unavailable))
        );
    }
    assert_eq!(fixture.token_requests().len(), 2);
    fixture.finish().await;
}
