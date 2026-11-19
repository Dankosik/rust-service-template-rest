use super::*;
use std::assert_matches;

#[tokio::test]
async fn an_exchanged_token_is_reused_per_subject_but_not_across_subjects() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("first"));
    let client = fixture.exchange_client();
    fixture.send_exchange(&client, "alice").await.unwrap();
    fixture.send_exchange(&client, "alice").await.unwrap();
    assert_eq!(fixture.token_request_count(), 1);
    fixture.token_json(StatusCode::OK, &exchange_response("second"));
    fixture.send_exchange(&client, "bob").await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
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
    let exchange_tokens = fixture.exchange_tokens(&[], None, 1);
    let client = exchange_tokens.http(fixture.resource_client()).unwrap();
    let cache = exchange_tokens.cache();
    for subject in ["alice", "bob"] {
        fixture.send_exchange(&client, subject).await.unwrap();
    }
    assert_eq!(fixture.token_request_count(), 2);
    cache.run_pending_tasks().await;
    assert!(cache.entry_count() <= 1);
    for subject in ["alice", "bob"] {
        fixture.send_exchange(&client, subject).await.unwrap();
        cache.run_pending_tasks().await;
    }
    assert!(fixture.token_request_count() > 2);
    assert!(cache.entry_count() <= 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_capacity_that_does_not_divide_the_payload_target_keeps_every_configured_subject() {
    const SUBJECTS: u32 = 5;
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    let exchange_tokens = fixture.exchange_tokens(&[], None, SUBJECTS);
    let client = exchange_tokens.http(fixture.resource_client()).unwrap();
    let cache = exchange_tokens.cache();
    for _ in 0..2 {
        for subject in 0..SUBJECTS {
            fixture
                .send_exchange(&client, &format!("subject-{subject}"))
                .await
                .unwrap();
            cache.run_pending_tasks().await;
        }
    }
    assert_eq!(fixture.token_request_count(), SUBJECTS as usize);
    assert_eq!(cache.entry_count(), u64::from(SUBJECTS));
    fixture.finish().await;
}

#[tokio::test]
async fn exchanged_cache_evicts_large_payloads_before_the_entry_target() {
    const PAYLOAD_TARGET: usize = 16 * 1024 * 1024;
    let fixture = Fixture::new().await;
    let exchange_tokens = fixture.exchange_tokens(&[], None, EXCHANGE_CAPACITY);
    let client = exchange_tokens.http(fixture.resource_client()).unwrap();
    let prefix = "x".repeat(512 * 1024 - 16);
    for index in 0..33 {
        fixture.token_json(
            StatusCode::OK,
            &exchange_response(&format!("{prefix}-{index:02}")),
        );
        fixture
            .send_exchange(&client, &format!("subject-{index}"))
            .await
            .unwrap();
    }
    let cache = exchange_tokens.cache();
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
async fn short_or_missing_lifetime_makes_each_exchange_token_request_only() {
    for expires_in in [Some(5), None] {
        let fixture = Fixture::new().await;
        let mut response = exchange_response("request-only");
        if let Some(expires_in) = expires_in {
            response["expires_in"] = serde_json::json!(expires_in);
        } else {
            response.as_object_mut().unwrap().remove("expires_in");
        }
        fixture.token_json(StatusCode::OK, &response);
        let client = fixture.exchange_client();
        for requests in 1..=2 {
            fixture.send_exchange(&client, "subject").await.unwrap();
            assert_eq!(fixture.token_request_count(), requests, "{expires_in:?}");
            assert_eq!(fixture.resource_request_count(), requests, "{expires_in:?}");
        }
        fixture.finish().await;
    }
}

#[tokio::test]
async fn a_waiter_exchanges_again_instead_of_taking_its_leaders_request_only_token() {
    let fixture = Fixture::new().await;
    let mut response = exchange_response("request-only");
    response.as_object_mut().unwrap().remove("expires_in");
    fixture.token_json(StatusCode::OK, &response);
    let client = fixture.exchange_client();
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(fixture.send_exchange(&client, "subject"));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("leader must be gated: {result:?}"), }
    let mut waiter = Box::pin(fixture.send_exchange(&client, "subject"));
    poll_pending(waiter.as_mut()).await;
    assert_eq!(fixture.token_request_count(), 1);
    gate.add_permits(1);
    leader.await.unwrap();
    tokio::select! { () = fixture.token_received() => {}, result = &mut waiter => panic!("waiter must exchange again: {result:?}"), }
    gate.add_permits(1);
    waiter.await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
    fixture.finish().await;
}

#[tokio::test]
async fn an_exchanged_token_past_its_reuse_cutoff_is_exchanged_once_more() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("first"));
    let client = fixture.exchange_client();
    let execute = || async {
        fixture.send_exchange(&client, "alice").await.unwrap();
    };
    execute().await;
    advance(Duration::from_secs(51)).await;
    fixture.token_json(StatusCode::OK, &exchange_response("second"));
    execute().await;
    execute().await;
    assert_eq!(fixture.token_request_count(), 2);
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
        .exchange_tokens_with(
            Options {
                provider_concurrency: 1,
                ..fixture.options()
            },
            &[],
            None,
            EXCHANGE_CAPACITY,
        )
        .http(fixture.resource_client())
        .unwrap();
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(fixture.send_exchange(&client, "subject"));
    fixture.drive_until_token_received(leader.as_mut()).await;
    let mut waiter = Box::pin(fixture.send_exchange(&client, "subject"));
    poll_pending(waiter.as_mut()).await;
    assert_eq!(fixture.token_request_count(), 1);
    gate.add_permits(2);
    let (leader, waiter) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(leader, waiter)
    })
    .await
    .unwrap();
    leader.unwrap();
    waiter.unwrap();
    assert_eq!(fixture.token_request_count(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn provider_capacity_is_shared_across_grants_but_hits_and_waiters_do_not_consume_it() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("shared"));
    let registration = prepare_registration(Options {
        provider_concurrency: 2,
        ..fixture.options()
    });
    let exchange_tokens = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap();
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    fixture.drive(driver);
    let client = exchange_tokens.http(fixture.resource_client()).unwrap();
    fixture.send_exchange(&client, "cached").await.unwrap();
    let gate = fixture.block_tokens();
    let service_client = service_tokens.http(fixture.resource_client()).unwrap();
    let mut service = Box::pin(fixture.send(&service_client));
    tokio::select! { () = fixture.token_received() => {}, result = &mut service => panic!("service must be gated: {result:?}"), }
    let other_binding = exchange_tokens
        .clone()
        .http(fixture.resource_client())
        .unwrap();
    let mut exchange = Box::pin(fixture.send_exchange(&other_binding, "active"));
    tokio::select! { () = fixture.token_received() => {}, result = &mut exchange => panic!("exchange must be gated: {result:?}"), }
    let mut waiter = Box::pin(fixture.send_exchange(&client, "active"));
    poll_pending(waiter.as_mut()).await;
    drop(waiter);
    for _ in 0..2 {
        assert_matches!(
            fixture.send_exchange(&client, "refused").await,
            Err(Error::Acquisition(AcquisitionError::AtCapacity))
        );
    }
    assert_matches!(
        client
            .execute(
                fixture.request(),
                &subject_token("expired"),
                &context(Duration::ZERO),
            )
            .await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    assert_eq!(fixture.token_request_count(), 3);
    assert_eq!(fixture.resource_request_count(), 1);
    fixture.send_exchange(&client, "cached").await.unwrap();
    assert_eq!(fixture.token_request_count(), 3);
    gate.add_permits(2);
    let (service, exchange) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(service, exchange)
    })
    .await
    .unwrap();
    service.unwrap();
    exchange.unwrap();
    *fixture.state.token_gate.lock().unwrap() = None;
    fixture.send_exchange(&client, "refused").await.unwrap();
    assert_eq!(fixture.token_request_count(), 4);
    assert_eq!(fixture.resource_request_count(), 5);
    fixture.finish().await;
}

#[tokio::test]
async fn cancelling_an_exchange_releases_capacity_without_releasing_a_live_waiters_leader() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("replacement"));
    let registration = prepare_registration(Options {
        provider_concurrency: 1,
        ..fixture.options()
    });
    let exchange_tokens = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap();
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    fixture.drive(driver);
    let client = exchange_tokens.http(fixture.resource_client()).unwrap();
    let service_client = service_tokens.http(fixture.resource_client()).unwrap();
    let gate = fixture.block_tokens();
    let mut leader = Box::pin(fixture.send_exchange(&client, "subject"));
    tokio::select! { () = fixture.token_received() => {}, result = &mut leader => panic!("leader must be gated: {result:?}"), }
    let wait_context = context(Duration::from_secs(10));
    let subject = subject_token("subject");
    let mut waiter = Box::pin(client.execute(fixture.request(), &subject, &wait_context));
    poll_pending(waiter.as_mut()).await;
    wait_context.cancel();
    assert_matches!(
        waiter.await,
        Err(Error::Acquisition(AcquisitionError::Cancelled))
    );
    assert_matches!(
        fixture.send(&service_client).await,
        Err(Error::Acquisition(AcquisitionError::AtCapacity))
    );
    assert_matches!(
        service_client
            .execute(fixture.request(), &context(Duration::ZERO))
            .await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    assert_eq!(fixture.token_request_count(), 1);
    assert_eq!(fixture.resource_request_count(), 0);
    drop(leader);
    *fixture.state.token_gate.lock().unwrap() = None;
    fixture.send(&service_client).await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
    assert_eq!(fixture.resource_request_count(), 1);
    drop(gate);
    fixture.finish().await;
}

#[tokio::test]
async fn a_waiter_gets_its_own_attempt_budget_after_its_leader_times_out() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    let resource = Client::try_new_for_test_http(
        fixture.origin.as_str(),
        infra_outbound_http::Limits {
            operation_timeout: Duration::from_secs(10),
            ..TOKEN_LIMITS
        },
    )
    .unwrap();
    let client = fixture
        .exchange_tokens_with(
            Options {
                provider_concurrency: 1,
                ..fixture.options()
            },
            &[],
            None,
            EXCHANGE_CAPACITY,
        )
        .http(resource)
        .unwrap();
    let gate = fixture.block_tokens();
    let leader_context = context(Duration::from_secs(4));
    let subject = subject_token("subject");
    let mut leader = Box::pin(client.execute(fixture.request(), &subject, &leader_context));
    fixture.drive_until_token_received(leader.as_mut()).await;
    let mut waiter = Box::pin(fixture.send_exchange(&client, "subject"));
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
    assert_eq!(fixture.token_request_count(), 2);
    assert_eq!(fixture.resource_request_count(), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_resource_401_evicts_only_that_subjects_exchanged_token() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("shared"));
    let client = fixture.exchange_client();
    fixture.send_exchange(&client, "alice").await.unwrap();
    fixture.send_exchange(&client, "bob").await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
    advance(EVICTION_MIN_AGE).await;
    fixture.resource_status(StatusCode::UNAUTHORIZED);
    fixture.send_exchange(&client, "alice").await.unwrap();
    fixture.resource_status(StatusCode::OK);
    let before = fixture.token_request_count();
    fixture.send_exchange(&client, "alice").await.unwrap();
    assert_eq!(fixture.token_request_count(), before + 1);
    fixture.send_exchange(&client, "bob").await.unwrap();
    assert_eq!(fixture.token_request_count(), before + 1);
    fixture.finish().await;
}

#[tokio::test]
async fn a_failed_exchange_is_not_cached() {
    let fixture = Fixture::new().await;
    fixture.token_raw(response(StatusCode::INTERNAL_SERVER_ERROR, b"{}"));
    let client = fixture
        .exchange_tokens_with(
            Options {
                provider_concurrency: 1,
                ..fixture.options()
            },
            &[],
            None,
            EXCHANGE_CAPACITY,
        )
        .http(fixture.resource_client())
        .unwrap();
    for _ in 0..2 {
        let result = fixture.send_exchange(&client, "subject").await;
        assert_matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::Unavailable))
        );
    }
    assert_eq!(fixture.token_request_count(), 2);
    fixture.finish().await;
}
