use super::*;

/// The share arithmetic is `shared_rules::jitter`'s; this owner adds the
/// unjittered fallback when the random source fails.
#[test]
fn refresh_spread_spans_its_window_and_vanishes_on_source_failure() {
    for window in [
        Duration::ZERO,
        Duration::from_secs(3),
        Duration::from_secs(30),
    ] {
        assert_eq!(
            crate::credentials::refresh_spread(window, Ok(0)),
            Duration::ZERO
        );
        assert_eq!(
            crate::credentials::refresh_spread(window, Ok(u16::MAX)),
            window
        );
        assert_eq!(
            crate::credentials::refresh_spread(window, Err(aws_lc_rs::error::Unspecified)),
            Duration::ZERO
        );
    }
}

/// Asserts that no token request arrives within a short real-time window.
async fn assert_no_token_request(fixture: &Fixture) {
    assert!(
        tokio::time::timeout(Duration::from_millis(100), fixture.token_received())
            .await
            .is_err(),
        "a token request arrived"
    );
}

#[tokio::test]
async fn a_service_token_is_refreshed_within_its_sampled_lead_before_the_reuse_cutoff() {
    // The lead is a quarter of the reuse window (lifetime minus ten seconds),
    // at most five minutes, less up to a tenth of itself: 11 s reuses for 1 s
    // and refreshes from 750–775 ms, 60 s from 37.5–38.75 s, 3600 s from
    // 3290–3320 s. Each row checks just before the earliest and just after
    // the latest eligibility, both inside the reuse window.
    for (lifetime, quiet_until, refreshed_by) in [
        (11, Duration::from_millis(700), Duration::from_millis(850)),
        (60, Duration::from_secs(37), Duration::from_secs(39)),
        (3600, Duration::from_secs(3285), Duration::from_secs(3325)),
    ] {
        let fixture = Fixture::new().await;
        fixture.token_json(
            "200 OK",
            &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": lifetime}),
        );
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let acquired = Instant::now();
        let execute = || async {
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
        };
        execute().await;
        fixture.token_received().await;

        advance((acquired + quiet_until).saturating_duration_since(Instant::now())).await;
        execute().await;
        assert_no_token_request(&fixture).await;

        fixture.token_json(
            "200 OK",
            &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 3600}),
        );
        advance((acquired + refreshed_by).saturating_duration_since(Instant::now())).await;
        execute().await;
        fixture.token_received().await;
        assert_eq!(fixture.token_requests().len(), 2, "{lifetime} s");
        let last = fixture.resource_requests().pop().unwrap();
        assert_eq!(
            last.header("authorization"),
            Some("Bearer first"),
            "{lifetime} s"
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn an_exchanged_token_is_never_refreshed_in_the_background() {
    let fixture = Fixture::new().await;
    let mut response = exchange_response("first");
    response["expires_in"] = 3600.into();
    fixture.token_json("200 OK", &response);
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let execute = || async {
        client
            .execute(
                fixture.on_behalf_of_request("alice"),
                deadline(Duration::from_secs(10)),
            )
            .await
            .unwrap();
    };
    execute().await;
    fixture.token_received().await;
    // Past every lead a service token of this lifetime would refresh within,
    // still before the 3590 s reuse cutoff.
    advance(Duration::from_secs(3321)).await;
    execute().await;
    assert_no_token_request(&fixture).await;
    assert_eq!(fixture.token_requests().len(), 1);
    let last = fixture.resource_requests().pop().unwrap();
    assert_eq!(last.header("authorization"), Some("Bearer first"));
    fixture.finish().await;
}

#[tokio::test]
async fn a_cached_token_is_refreshed_after_its_reuse_cutoff() {
    let fixture = Fixture::new().await;
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    for _ in 0..2 {
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    assert_eq!(fixture.token_requests().len(), 1);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(51)).await;
    tokio::time::resume();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(fixture.resource_requests().len(), 3);
    fixture.finish().await;
}

#[tokio::test]
async fn near_its_cutoff_a_token_is_replaced_in_the_background_while_callers_reuse_it() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let credentials = fixture.credentials(&[], None);
    let client = credentials.http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    // Inside every sampled lead (270–300 seconds) before the 3590 s cutoff.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3321)).await;
    tokio::time::resume();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 40}),
    );
    let gate = fixture.block_tokens();
    for _ in 0..2 {
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    fixture.token_received().await;
    assert_eq!(fixture.token_requests().len(), 2);
    let completion_started = Instant::now();
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
            let requests = fixture.resource_requests();
            if requests.last().unwrap().header("authorization") == Some("Bearer second") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.token_requests().len(), 2);
    let completion_observed = Instant::now();
    let next = credentials.cached().service.as_ref().unwrap().refresh_after;
    assert!(next >= completion_started + Duration::from_secs(30));
    assert!(next <= completion_observed + Duration::from_secs(33));
    let authorizations = fixture
        .resource_requests()
        .iter()
        .map(|request| request.header("authorization").unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(
        authorizations[..3]
            .iter()
            .all(|value| value == "Bearer first")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn refresh_driver_finishes_after_the_final_credentials_owner_is_released() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let (credentials, driver) = fixture.build(fixture.options(&[], None));
    let client = credentials.http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_received().await;
    fixture.token_completed().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3321)).await;
    tokio::time::resume();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "replacement", "token_type": "Bearer", "expires_in": 3600}),
    );
    let gate = fixture.block_tokens();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    let mut driver = Box::pin(tokio::spawn(driver.run(std::future::pending())));
    fixture.token_received().await;
    drop(credentials);
    poll_pending(driver.as_mut()).await;
    gate.add_permits(1);
    fixture.token_completed().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
            if fixture
                .resource_requests()
                .last()
                .unwrap()
                .header("authorization")
                == Some("Bearer replacement")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    *fixture.state.token_gate.lock().unwrap() = None;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3321)).await;
    tokio::time::resume();
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "final", "token_type": "Bearer", "expires_in": 3600}),
    );
    let final_gate = fixture.block_tokens();
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_received().await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), driver)
        .await
        .unwrap()
        .unwrap();
    drop(gate);
    drop(final_gate);
    fixture.finish().await;
}

#[tokio::test]
async fn a_dropped_refresh_driver_closes_cached_http_calls_before_dispatch() {
    let fixture = Fixture::new().await;
    let (credentials, driver) = fixture.build(fixture.options(&[], None));
    let client = credentials.http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    let token_requests = fixture.token_requests().len();
    let resource_requests = fixture.resource_requests().len();
    drop(driver);
    for (deadline, expected) in [
        (
            deadline(Duration::from_secs(10)),
            AcquisitionError::Unavailable,
        ),
        (Instant::now(), AcquisitionError::Timeout),
    ] {
        let error = client
            .execute(fixture.request(), deadline)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Acquisition(actual) if actual == expected));
    }
    assert_eq!(fixture.token_requests().len(), token_requests);
    assert_eq!(fixture.resource_requests().len(), resource_requests);
    fixture.finish().await;
}

#[tokio::test]
async fn queued_refresh_spends_its_original_budget_waiting_for_foreground_acquisition() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let (credentials, driver) = fixture.build(fixture.options(&[], None));
    credentials
        .service_token(deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_received().await;
    let refresh = credentials.0.inner.service_request.lock().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3321)).await;
    tokio::time::resume();
    credentials
        .service_token(deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert!(
        credentials.cached().refresh_pending,
        "refresh request is queued"
    );
    let (shutdown, shutdown_requested) = oneshot::channel();
    let driver = tokio::spawn(driver.run(async move {
        let _ = shutdown_requested.await;
    }));
    tokio::time::pause();
    tokio::time::advance(FETCH_TIMEOUT).await;
    tokio::time::resume();
    drop(refresh);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), fixture.token_received())
            .await
            .is_err(),
        "queued refresh started a provider request after its original budget"
    );
    shutdown.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), driver)
        .await
        .unwrap()
        .unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn a_failed_background_refresh_keeps_the_token_and_retries_after_a_pause() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "kept", "token_type": "Bearer", "expires_in": 3600}),
    );
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_raw(response("503 Service Unavailable", b"{}"));
    let gate = fixture.block_tokens();
    gate.add_permits(Semaphore::MAX_PERMITS / 2);
    for (advance, token_requests) in [(3321, 2), (0, 2), (34, 3)] {
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(advance)).await;
        tokio::time::resume();
        let before = fixture.token_requests().len();
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
        if token_requests > before {
            fixture.token_received().await;
        }
        assert_eq!(fixture.token_requests().len(), token_requests);
    }
    assert!(
        fixture
            .resource_requests()
            .iter()
            .all(|request| request.header("authorization") == Some("Bearer kept"))
    );
    fixture.finish().await;
}

#[tokio::test]
async fn a_refresh_failure_survives_an_eligible_401_eviction() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "old", "token_type": "Bearer", "expires_in": 60}),
    );
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    client
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    fixture.token_received().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(39)).await;
    tokio::time::resume();
    fixture.token_raw(response("503 Service Unavailable", b"{}"));
    fixture.resource_status("401 Unauthorized");
    let gate = fixture.block_tokens();
    assert_eq!(
        client
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.token_received().await;
    let mut next = Box::pin(client.execute(fixture.request(), deadline(Duration::from_secs(10))));
    poll_pending(next.as_mut()).await;
    gate.add_permits(1);
    assert!(matches!(
        next.await,
        Err(Error::Acquisition(AcquisitionError::Unavailable))
    ));
    assert_eq!(fixture.token_requests().len(), 2);
    assert_eq!(fixture.resource_requests().len(), 2);
    fixture.finish().await;
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<String>>>);

impl tracing::Subscriber for Events {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(Vec<String>);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push(format!("{}={value:?}", field.name()));
            }
        }
        if !event
            .metadata()
            .target()
            .starts_with("infra_oauth2_client_credentials")
        {
            return;
        }
        let mut fields = Fields(Vec::new());
        event.record(&mut fields);
        fields.0.sort();
        self.0.lock().unwrap().push(fields.0.join(" "));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn a_failed_background_refresh_is_logged_with_its_error_type() {
    let events = Events::default();
    tracing::subscriber::with_default(events.clone(), || {
        // Keep callsite interest independent of a sibling test's thread-local
        // subscriber; tracing-core otherwise has a single-dispatcher fast path.
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let fixture = Fixture::new().await;
                fixture.token_json(
                    "200 OK",
                    &serde_json::json!({"access_token": "kept", "token_type": "Bearer", "expires_in": 3600}),
                );
                let client = fixture
                    .credentials(&[], None)
                    .http(fixture.resource_client());
                client
                    .execute(fixture.request(), deadline(Duration::from_secs(10)))
                    .await
                    .unwrap();
                fixture.token_raw(response(
                    "400 Bad Request",
                    br#"{"error":"invalid_client","error_description":"provider-secret-body"}"#,
                ));
                advance(Duration::from_secs(3321)).await;
                // The caller keeps its token; the refresh it starts fails alone.
                client
                    .execute(fixture.request(), deadline(Duration::from_secs(10)))
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while events.0.lock().unwrap().is_empty() {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                fixture.finish().await;
            });
    });
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            r#"error.type="invalid_client" message=oauth2_background_refresh_failed server.address="127.0.0.1""#
        ]
    );
}

#[test]
fn capacity_refused_background_refresh_keeps_the_token_and_retry_spacing() {
    let events = Events::default();
    tracing::subscriber::with_default(events.clone(), || {
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let fixture = Fixture::new().await;
            fixture.token_json("200 OK", &serde_json::json!({"access_token":"kept", "token_type":"Bearer", "expires_in":3600}));
            let credentials = fixture.prepare(Options { provider_concurrency: 1, ..fixture.options(&[], None) });
            let client = credentials.http(fixture.resource_client());
            client.execute(fixture.request(), deadline(Duration::from_secs(10))).await.unwrap();
            advance(Duration::from_secs(3321)).await;
            let gate = fixture.block_tokens();
            let mut exchange = Box::pin(client.execute(fixture.on_behalf_of_request("active"), deadline(Duration::from_secs(10))));
            tokio::select! { () = fixture.token_received() => {}, result = &mut exchange => panic!("exchange must be gated: {result:?}"), }
            client.execute(fixture.request(), deadline(Duration::from_secs(10))).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while events.0.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            assert_eq!(fixture.token_requests().len(), 2);
            drop(exchange);
            drop(gate);
            *fixture.state.token_gate.lock().unwrap() = None;
            client.execute(fixture.request(), deadline(Duration::from_secs(10))).await.unwrap();
            assert_eq!(fixture.token_requests().len(), 2);
            advance(Duration::from_secs(34)).await;
            client.execute(fixture.request(), deadline(Duration::from_secs(10))).await.unwrap();
            fixture.token_received().await;
            let refresh = tokio::time::timeout(Duration::from_secs(2), credentials.0.inner.service_request.lock()).await.unwrap();
            drop(refresh);
            assert_eq!(fixture.token_requests().len(), 3);
            assert!(fixture.resource_requests().iter().all(|request| request.header("authorization") == Some("Bearer kept")));
            fixture.finish().await;
        });
    });
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            r#"error.type="capacity" message=oauth2_background_refresh_failed server.address="127.0.0.1""#
        ]
    );
}
