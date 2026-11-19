use super::*;
use std::assert_matches;

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
    for (lifetime, quiet_until, refreshed_by) in [
        (11, Duration::from_millis(700), Duration::from_millis(850)),
        (60, Duration::from_secs(37), Duration::from_secs(39)),
        (3600, Duration::from_secs(3285), Duration::from_secs(3325)),
    ] {
        let fixture = Fixture::new().await;
        fixture.token_json(
            StatusCode::OK,
            &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": lifetime}),
        );
        let client = fixture.service_client();
        let acquired = Instant::now();
        let execute = || async {
            fixture.send(&client).await.unwrap();
        };
        execute().await;
        fixture.token_received().await;

        advance((acquired + quiet_until).saturating_duration_since(Instant::now())).await;
        execute().await;
        assert_no_token_request(&fixture).await;

        fixture.token_json(
            StatusCode::OK,
            &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 3600}),
        );
        advance((acquired + refreshed_by).saturating_duration_since(Instant::now())).await;
        execute().await;
        fixture.token_received().await;
        assert_eq!(fixture.token_request_count(), 2, "{lifetime} s");
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
    fixture.token_json(StatusCode::OK, &response);
    let client = fixture.exchange_client();
    let execute = || async {
        fixture
            .send_request(&client, fixture.on_behalf_of_request("alice"))
            .await
            .unwrap();
    };
    execute().await;
    fixture.token_received().await;
    advance(Duration::from_secs(3321)).await;
    execute().await;
    assert_no_token_request(&fixture).await;
    assert_eq!(fixture.token_request_count(), 1);
    let last = fixture.resource_requests().pop().unwrap();
    assert_eq!(last.header("authorization"), Some("Bearer first"));
    fixture.finish().await;
}

#[tokio::test]
async fn a_cached_token_is_refreshed_after_its_reuse_cutoff() {
    let fixture = Fixture::new().await;
    let client = fixture.service_client();
    for _ in 0..2 {
        fixture.send(&client).await.unwrap();
    }
    assert_eq!(fixture.token_request_count(), 1);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(51)).await;
    tokio::time::resume();
    fixture.send(&client).await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
    assert_eq!(fixture.resource_request_count(), 3);
    fixture.finish().await;
}

#[tokio::test]
async fn short_lived_background_replacements_get_their_own_early_refresh() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let client = fixture.service_client();
    fixture.send(&client).await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3321)).await;
    tokio::time::resume();
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 40}),
    );
    let gate = fixture.block_tokens();
    for _ in 0..2 {
        fixture.send(&client).await.unwrap();
    }
    fixture.token_received().await;
    assert_eq!(fixture.token_request_count(), 2);
    let second_requested = Instant::now();
    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            fixture.send(&client).await.unwrap();
            let requests = fixture.resource_requests();
            if requests.last().unwrap().header("authorization") == Some("Bearer second") {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.token_request_count(), 2);
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

    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "third", "token_type": "Bearer", "expires_in": 40}),
    );
    advance((second_requested + Duration::from_secs(25)).saturating_duration_since(Instant::now()))
        .await;
    fixture.send(&client).await.unwrap();
    fixture.token_received().await;
    assert_eq!(fixture.token_request_count(), 3);
    let last = fixture.resource_requests().pop().unwrap();
    assert_eq!(last.header("authorization"), Some("Bearer second"));
    gate.add_permits(1);
    fixture.finish().await;
}

#[tokio::test]
async fn dropping_registration_stops_inflight_refresh_and_closes_warm_clients() {
    let fixture = Fixture::new().await;
    let registration = prepare_registration(fixture.options());
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    let service = service_tokens.http(fixture.resource_client()).unwrap();
    let exchange = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .unwrap();
    let driver = tokio::spawn(driver.run(std::future::pending()));
    let mut response = exchange_response("warm-token");
    response["expires_in"] = 3600.into();
    fixture.token_json(StatusCode::OK, &response);
    let delegated = || fixture.on_behalf_of_request("alice");
    fixture.send(&service).await.unwrap();
    fixture.send_request(&exchange, delegated()).await.unwrap();

    advance(Duration::from_secs(3321)).await;
    let gate = fixture.block_tokens();
    fixture.send(&service).await.unwrap();
    fixture.token_received().await;
    fixture.send_request(&exchange, delegated()).await.unwrap();
    assert_eq!(fixture.token_request_count(), 3);
    assert_eq!(fixture.resource_request_count(), 4);

    let service_clone = service.clone();
    let exchange_clone = exchange.clone();
    drop(registration);
    tokio::time::timeout(Duration::from_secs(2), driver)
        .await
        .unwrap()
        .unwrap();

    assert_closed_calls(&service_clone, || fixture.request()).await;
    assert_closed_calls(&exchange_clone, delegated).await;
    assert_eq!(fixture.token_request_count(), 3);
    assert_eq!(fixture.resource_request_count(), 4);
    drop(gate);
    fixture.finish().await;
}

#[tokio::test]
async fn dropping_registration_stops_a_bound_http_call_during_token_acquisition() {
    let fixture = Fixture::new().await;
    let registration = prepare_registration(fixture.options());
    let (credentials, _driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    let client = credentials.http(fixture.resource_client()).unwrap();
    let context = OperationContext::unbounded();
    let client = client.with_context(&context);
    let _gate = fixture.block_tokens();
    let mut call = Box::pin(client.execute(fixture.request()));
    tokio::select! {
        () = fixture.token_received() => {}
        result = &mut call => panic!("token must be gated: {result:?}"),
    }
    drop(registration);
    let error = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .unwrap()
        .unwrap_err();
    assert_matches!(error, Error::Acquisition(AcquisitionError::Unavailable));
    assert_eq!(fixture.token_request_count(), 1);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn the_last_service_handle_stops_inflight_refresh_while_exchange_stays_open() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let registration = prepare_registration(fixture.options());
    let (service, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    let exchange = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .unwrap();
    let client = service.http(fixture.resource_client()).unwrap();
    fixture.send(&client).await.unwrap();
    fixture.token_received().await;
    fixture.token_completed().await;
    advance(Duration::from_secs(3321)).await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "replacement", "token_type": "Bearer", "expires_in": 3600}),
    );
    let gate = fixture.block_tokens();
    fixture.send(&client).await.unwrap();
    let mut driver = Box::pin(tokio::spawn(driver.run(std::future::pending())));
    fixture.token_received().await;
    drop(service);
    poll_pending(driver.as_mut()).await;
    gate.add_permits(1);
    fixture.token_completed().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            fixture.send(&client).await.unwrap();
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
    advance(Duration::from_secs(3321)).await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "final", "token_type": "Bearer", "expires_in": 3600}),
    );
    let final_gate = fixture.block_tokens();
    fixture.send(&client).await.unwrap();
    fixture.token_received().await;
    drop(client);
    tokio::time::timeout(Duration::from_secs(2), driver)
        .await
        .unwrap()
        .unwrap();
    *fixture.state.token_gate.lock().unwrap() = None;
    final_gate.add_permits(1);
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    fixture
        .send_request(&exchange, fixture.on_behalf_of_request("alice"))
        .await
        .unwrap();
    fixture.finish().await;
}

#[tokio::test]
async fn dropping_the_service_driver_closes_only_service_clients() {
    let fixture = Fixture::new().await;
    let registration = prepare_registration(fixture.options());
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    let service = service_tokens.http(fixture.resource_client()).unwrap();
    let exchange = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .unwrap();
    fixture.send(&service).await.unwrap();
    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    fixture
        .send_request(&exchange, fixture.on_behalf_of_request("warm"))
        .await
        .unwrap();

    drop(driver);
    assert_closed_calls(&service.clone(), || fixture.request()).await;
    assert_eq!(fixture.token_request_count(), 2);
    assert_eq!(fixture.resource_request_count(), 2);

    fixture.token_json(
        StatusCode::OK,
        &exchange_response("exchanged-after-shutdown"),
    );
    for subject in ["warm", "cold"] {
        fixture
            .send_request(&exchange, fixture.on_behalf_of_request(subject))
            .await
            .unwrap();
    }
    assert_eq!(fixture.token_request_count(), 3);
    let resources = fixture.resource_requests();
    assert_eq!(
        resources[2].header("authorization"),
        Some("Bearer exchanged")
    );
    assert_eq!(
        resources[3].header("authorization"),
        Some("Bearer exchanged-after-shutdown")
    );
    fixture.finish().await;
}

async fn assert_closed_calls(client: &AuthenticatedClient, request: impl Fn() -> Request<Bytes>) {
    let cancelled = OperationContext::unbounded();
    cancelled.cancel();
    for (after, request_context, expected) in [
        (Duration::from_secs(10), None, AcquisitionError::Unavailable),
        (Duration::ZERO, None, AcquisitionError::Timeout),
        (
            Duration::from_secs(10),
            Some(OperationContext::with_timeout(Duration::ZERO)),
            AcquisitionError::Timeout,
        ),
        (
            Duration::from_secs(10),
            Some(cancelled),
            AcquisitionError::Cancelled,
        ),
    ] {
        let mut request = request();
        if let Some(context) = request_context {
            request.extensions_mut().insert(context);
        }
        let context = OperationContext::with_timeout(after);
        let error = client
            .with_context(&context)
            .execute(request)
            .await
            .unwrap_err();
        assert_matches!(error, Error::Acquisition(actual) if actual == expected);
    }
}

#[tokio::test]
async fn queued_refresh_spends_its_original_budget_waiting_for_foreground_acquisition() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 3600}),
    );
    let registration = prepare_registration(fixture.options());
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    service_tokens
        .service_token(acquisition_deadline(&token_context()))
        .await
        .unwrap();
    fixture.token_received().await;
    let refresh = service_tokens.fetch_lock().lock().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3321)).await;
    tokio::time::resume();
    service_tokens
        .service_token(acquisition_deadline(&token_context()))
        .await
        .unwrap();
    assert!(
        service_tokens.cached().refresh_pending,
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
async fn refreshes_without_a_cacheable_replacement_keep_the_token_and_back_off() {
    for (refresh_response, quiet_until, retry_by) in [
        (
            response(StatusCode::SERVICE_UNAVAILABLE, b"{}"),
            Duration::from_secs(29),
            34,
        ),
        (
            json_response(
                StatusCode::OK,
                &serde_json::json!({"access_token": "request-only", "token_type": "Bearer"}),
            ),
            Duration::from_millis(33_500),
            38,
        ),
    ] {
        let fixture = Fixture::new().await;
        fixture.token_json(
            StatusCode::OK,
            &serde_json::json!({"access_token": "kept", "token_type": "Bearer", "expires_in": 3600}),
        );
        let service_tokens = fixture.service_tokens(&[]);
        let client = service_tokens.http(fixture.resource_client()).unwrap();
        fixture.send(&client).await.unwrap();
        fixture.token_raw(refresh_response);
        let gate = fixture.block_tokens();
        advance(Duration::from_secs(3321)).await;
        let refresh_requested = Instant::now();
        fixture.send(&client).await.unwrap();
        fixture.token_received().await;
        advance(Duration::from_secs(4)).await;
        gate.add_permits(1);
        let refresh =
            tokio::time::timeout(Duration::from_secs(2), service_tokens.fetch_lock().lock())
                .await
                .unwrap();
        drop(refresh);

        advance((refresh_requested + quiet_until).saturating_duration_since(Instant::now())).await;
        fixture.send(&client).await.unwrap();
        assert_no_token_request(&fixture).await;

        advance(
            (refresh_requested + Duration::from_secs(retry_by))
                .saturating_duration_since(Instant::now()),
        )
        .await;
        fixture.send(&client).await.unwrap();
        fixture.token_received().await;
        assert_eq!(fixture.token_request_count(), 3);
        assert!(
            fixture
                .resource_requests()
                .iter()
                .all(|request| request.header("authorization") == Some("Bearer kept"))
        );
        gate.add_permits(1);
        fixture.finish().await;
    }
}

#[tokio::test]
async fn a_refresh_failure_survives_an_eligible_401_eviction() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "old", "token_type": "Bearer", "expires_in": 60}),
    );
    let client = fixture.service_client();
    fixture.send(&client).await.unwrap();
    fixture.token_received().await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(39)).await;
    tokio::time::resume();
    fixture.token_raw(response(StatusCode::SERVICE_UNAVAILABLE, b"{}"));
    fixture.resource_status(StatusCode::UNAUTHORIZED);
    let gate = fixture.block_tokens();
    assert_eq!(
        fixture.send(&client).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.token_received().await;
    let mut next = Box::pin(fixture.send(&client));
    poll_pending(next.as_mut()).await;
    gate.add_permits(1);
    assert_matches!(
        next.await,
        Err(Error::Acquisition(AcquisitionError::Unavailable))
    );
    assert_eq!(fixture.token_request_count(), 2);
    assert_eq!(fixture.resource_request_count(), 2);
    fixture.finish().await;
}

#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<String>>>);

impl Events {
    fn install(&self) -> tracing::subscriber::DefaultGuard {
        use tracing_subscriber::layer::SubscriberExt as _;
        tracing::subscriber::set_default(tracing_subscriber::Registry::default().with(self.clone()))
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Events {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
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
}

#[tokio::test]
async fn a_failed_background_refresh_is_logged_with_its_error_type() {
    let events = Events::default();
    let _subscriber = events.install();
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "kept", "token_type": "Bearer", "expires_in": 3600}),
    );
    let client = fixture.service_client();
    fixture.send(&client).await.unwrap();
    fixture.token_raw(response(
        StatusCode::BAD_REQUEST,
        br#"{"error":"invalid_client","error_description":"provider-secret-body"}"#,
    ));
    advance(Duration::from_secs(3321)).await;
    fixture.send(&client).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while events.0.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    fixture.finish().await;
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            r#"error.type="invalid_client" message=oauth2_background_refresh_failed server.address="127.0.0.1""#
        ]
    );
}

#[tokio::test]
async fn capacity_refused_background_refresh_keeps_the_token_and_retry_spacing() {
    let events = Events::default();
    let _subscriber = events.install();
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token":"kept", "token_type":"Bearer", "expires_in":3600}),
    );
    let registration = prepare_registration(Options {
        provider_concurrency: 1,
        ..fixture.options()
    });
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    fixture.drive(driver);
    let client = service_tokens.http(fixture.resource_client()).unwrap();
    fixture.send(&client).await.unwrap();
    advance(Duration::from_secs(3321)).await;
    let gate = fixture.block_tokens();
    let exchange_client = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .unwrap();
    let mut exchange =
        Box::pin(fixture.send_request(&exchange_client, fixture.on_behalf_of_request("active")));
    tokio::select! { () = fixture.token_received() => {}, result = &mut exchange => panic!("exchange must be gated: {result:?}"), }
    fixture.send(&client).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while events.0.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.token_request_count(), 2);
    drop(exchange);
    drop(gate);
    *fixture.state.token_gate.lock().unwrap() = None;
    fixture.send(&client).await.unwrap();
    assert_eq!(fixture.token_request_count(), 2);
    advance(Duration::from_secs(34)).await;
    fixture.send(&client).await.unwrap();
    fixture.token_received().await;
    let refresh = tokio::time::timeout(Duration::from_secs(2), service_tokens.fetch_lock().lock())
        .await
        .unwrap();
    drop(refresh);
    assert_eq!(fixture.token_request_count(), 3);
    assert!(
        fixture
            .resource_requests()
            .iter()
            .all(|request| request.header("authorization") == Some("Bearer kept"))
    );
    fixture.finish().await;
    assert_eq!(
        *events.0.lock().unwrap(),
        [
            r#"error.type="capacity" message=oauth2_background_refresh_failed server.address="127.0.0.1""#
        ]
    );
}
