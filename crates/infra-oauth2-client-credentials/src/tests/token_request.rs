use super::*;
use std::assert_matches;

#[derive(serde::Deserialize)]
struct DecodedAssertion {
    iss: String,
    sub: String,
    aud: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    jti: String,
}

fn decode_assertion(
    fixture: &Fixture,
    request: &CapturedRequest,
) -> (jsonwebtoken::Header, DecodedAssertion) {
    let pairs = form_pairs(&request.body);
    let assertion = form_value(&pairs, "client_assertion");
    let header = jsonwebtoken::decode_header(assertion).unwrap();
    let mut validation = jsonwebtoken::Validation::new(header.alg);
    validation.set_audience(&[ASSERTION_AUDIENCE]);
    let key =
        jsonwebtoken::DecodingKey::from_rsa_pem(fixture.key.public_key_pem().as_bytes()).unwrap();
    let data = jsonwebtoken::decode::<DecodedAssertion>(assertion, &key, &validation).unwrap();
    (header, data.claims)
}

fn assert_token_form(body: &[u8], grant_fields: &str) {
    let body = std::str::from_utf8(body).unwrap();
    let (fields, assertion) = body.split_once("&client_assertion=").unwrap();
    assert_eq!(
        fields,
        format!(
            "{grant_fields}&client_id=client%3Aid\
             &client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"
        )
    );
    assert!(!assertion.contains('&'));
    assert!(jsonwebtoken::decode_header(assertion).is_ok());
}

#[tokio::test]
async fn each_grant_encodes_its_own_target_in_the_token_request() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        StatusCode::OK,
        &serde_json::json!({
            "access_token": "token+/_~.=",
            "token_type": "bEaReR",
            "expires_in": 60,
            "refresh_token": "private-refresh",
        }),
    );
    let registration = prepare_registration(fixture.options());
    let (service_tokens, driver) = registration.service_tokens(
        service_target(&["write", "read+all", "write"]),
        fixture.resource_origin(),
    );
    fixture.drive(driver);
    let response = service_tokens
        .http(fixture.resource_client())
        .unwrap()
        .execute(&timeout_context(Duration::from_secs(10)), fixture.request())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token_request = fixture.token_requests().pop().unwrap();
    assert_eq!(token_request.target, TOKEN_PATH);
    assert!(token_request.header("authorization").is_none());
    assert_token_form(
        &token_request.body,
        "grant_type=client_credentials&scope=write+read%2Ball+write",
    );
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer token+/_~.=")
    );

    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    registration
        .exchange_tokens(
            exchange_target(&["read"], Some("https://api.example/é?a=b&c=d")),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .unwrap()
        .execute(
            &timeout_context(Duration::from_secs(10)),
            fixture.request(),
            &subject("subject+/="),
        )
        .await
        .unwrap();
    let token_request = fixture.token_requests().pop().unwrap();
    assert_eq!(token_request.target, TOKEN_PATH);
    assert!(token_request.header("authorization").is_none());
    assert_token_form(
        &token_request.body,
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange\
         &subject_token=subject%2B%2F%3D\
         &subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token\
         &requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token\
         &scope=read&audience=https%3A%2F%2Fapi.example%2F%C3%A9%3Fa%3Db%26c%3Dd",
    );
    assert_eq!(
        fixture.resource_requests()[1].header("authorization"),
        Some("Bearer exchanged")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn two_targets_on_one_registration_keep_each_grants_tokens_separate() {
    let fixture = Fixture::new().await;
    let registration = prepare_registration(fixture.options());
    let bindings =
        [("catalog", "catalog-scope"), ("billing", "billing-scope")].map(|(audience, scope)| {
            let (service, driver) =
                registration.service_tokens(service_target(&[scope]), fixture.resource_origin());
            fixture.drive(driver);
            let exchange = registration
                .exchange_tokens(
                    exchange_target(&[], Some(audience)),
                    fixture.resource_origin(),
                    EXCHANGE_CAPACITY,
                )
                .unwrap();
            (
                audience,
                service.http(fixture.resource_client()).unwrap(),
                exchange.http(fixture.resource_client()).unwrap(),
            )
        });

    for (audience, service, exchange) in &bindings {
        fixture.token_json(
            StatusCode::OK,
            &exchange_response(&format!("{audience}-service")),
        );
        fixture.send(service).await.unwrap();
        fixture.token_json(
            StatusCode::OK,
            &exchange_response(&format!("{audience}-exchange")),
        );
        fixture
            .send_exchange(exchange, "same-subject")
            .await
            .unwrap();
    }
    let token_requests = fixture.token_requests();
    assert_eq!(token_requests.len(), 4);
    for (request, key, value) in [
        (&token_requests[0], "scope", "catalog-scope"),
        (&token_requests[1], "audience", "catalog"),
        (&token_requests[2], "scope", "billing-scope"),
        (&token_requests[3], "audience", "billing"),
    ] {
        assert_eq!(form_value(&form_pairs(&request.body), key), value);
    }
    let resources = fixture.resource_requests();
    let headers: Vec<_> = resources
        .iter()
        .map(|request| request.header("authorization").unwrap())
        .collect();
    assert_eq!(
        headers,
        [
            "Bearer catalog-service",
            "Bearer catalog-exchange",
            "Bearer billing-service",
            "Bearer billing-exchange"
        ]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn absent_scope_is_omitted_from_the_client_credentials_form() {
    let fixture = Fixture::new().await;
    fixture
        .service_client()
        .execute(&timeout_context(Duration::from_secs(10)), fixture.request())
        .await
        .unwrap();
    assert_token_form(
        &fixture.token_requests()[0].body,
        "grant_type=client_credentials",
    );
    fixture.finish().await;
}

#[tokio::test]
async fn assertion_header_and_claims_are_verifiable_and_two_requests_differ_in_jti() {
    let fixture = Fixture::new().await;
    let mut signing_windows = Vec::new();
    for _ in 0..2 {
        let before = jsonwebtoken::get_current_timestamp();
        fixture
            .service_client()
            .execute(&timeout_context(Duration::from_secs(10)), fixture.request())
            .await
            .unwrap();
        signing_windows.push(before..=jsonwebtoken::get_current_timestamp());
    }
    let requests = fixture.token_requests();
    assert_eq!(requests.len(), 2);
    let decoded = requests
        .iter()
        .map(|request| decode_assertion(&fixture, request))
        .collect::<Vec<_>>();
    for ((header, claims), signing_window) in decoded.iter().zip(signing_windows) {
        assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("key-1"));
        assert_eq!(header.typ.as_deref(), Some("client-authentication+jwt"));
        assert_eq!(claims.iss, "client:id");
        assert_eq!(claims.sub, "client:id");
        assert_eq!(claims.aud, ASSERTION_AUDIENCE);
        assert_eq!(claims.exp - claims.iat, 60);
        assert_eq!(claims.iat, claims.nbf);
        assert!(signing_window.contains(&(claims.iat + 10)));
        assert_ne!(claims.jti, "");
    }
    assert_ne!(decoded[0].1.jti, decoded[1].1.jti);
    fixture.finish().await;
}

#[tokio::test]
async fn exchange_without_the_access_token_issued_type_is_refused() {
    let fixture = Fixture::new().await;
    for issued_token_type in [None, Some("urn:ietf:params:oauth:token-type:jwt")] {
        let mut body = serde_json::json!({
            "access_token": "x",
            "token_type": "Bearer",
            "expires_in": 60,
        });
        if let Some(value) = issued_token_type {
            body["issued_token_type"] = value.into();
        }
        fixture.token_json(StatusCode::OK, &body);
        let result = fixture
            .exchange_client()
            .execute(
                &timeout_context(Duration::from_secs(10)),
                fixture.request(),
                &subject("subject"),
            )
            .await;
        assert_matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn malformed_bearer_tokens_are_refused_before_resource_dispatch() {
    let fixture = Fixture::new().await;
    let malformed = ["", "control\u{7f}character", "split token", " leading"];
    for token in malformed {
        fixture.token_json(
            StatusCode::OK,
            &serde_json::json!({"access_token": token, "token_type": "BEARER", "expires_in": 60}),
        );
        let result = fixture
            .service_client()
            .execute(&timeout_context(Duration::from_secs(10)), fixture.request())
            .await;
        assert_matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        );
    }
    assert_eq!(fixture.token_request_count(), malformed.len());
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn short_lived_and_omitted_lifetime_service_tokens_are_request_only() {
    let fixture = Fixture::new().await;
    for (body, fetches_for_two_calls) in [
        (
            serde_json::json!({"access_token": "short", "token_type": "Bearer", "expires_in": 10}),
            2,
        ),
        (
            serde_json::json!({"access_token": "no-expiry", "token_type": "Bearer"}),
            2,
        ),
    ] {
        fixture.token_json(StatusCode::OK, &body);
        let client = fixture.service_client();
        let before = fixture.token_request_count();
        for _ in 0..2 {
            fixture.send(&client).await.unwrap();
        }
        assert_eq!(
            fixture.token_request_count(),
            before + fetches_for_two_calls
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_service_and_exchanged_lifetimes_refuse_before_resource_dispatch() {
    let fixture = Fixture::new().await;
    for (delegated, lifetime) in [
        (false, serde_json::Value::from(u64::MAX)),
        (true, serde_json::Value::from(u64::MAX)),
        (false, serde_json::Value::Null),
    ] {
        let mut response = exchange_response("invalid-lifetime");
        response["expires_in"] = lifetime;
        fixture.token_json(StatusCode::OK, &response);
        let error = if delegated {
            fixture
                .exchange_client()
                .execute(
                    &timeout_context(Duration::from_secs(10)),
                    fixture.request(),
                    &subject("subject"),
                )
                .await
                .unwrap_err()
        } else {
            fixture
                .service_client()
                .execute(&timeout_context(Duration::from_secs(10)), fixture.request())
                .await
                .unwrap_err()
        };
        assert_matches!(error, Error::Acquisition(AcquisitionError::InvalidResponse));
    }
    assert_eq!(fixture.token_request_count(), 3);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn exchange_rejects_a_token_when_processing_crosses_the_attempt_deadline() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchange-token"));
    let resource = Client::try_new_for_test_http(
        fixture.origin.as_str(),
        TOKEN_LIMITS
            .with_operation_timeout(Duration::from_secs(20))
            .unwrap(),
    )
    .unwrap();
    let client = fixture
        .exchange_tokens(&[], None, EXCHANGE_CAPACITY)
        .http(resource)
        .unwrap();
    let recorder = ObserveMetricRegistration(|key: &metrics::Key| {
        if key.name() == "http_client_request_duration_seconds"
            && key
                .labels()
                .any(|label| label.key() == "http.request.method" && label.value() == "POST")
            && key
                .labels()
                .any(|label| label.key() == "outbound.outcome" && label.value() == "response")
        {
            advance_during_metric(Duration::from_secs(5));
        }
    });
    let observation = metrics::set_default_local_recorder(&recorder);
    let result = client
        .execute(
            &timeout_context(Duration::from_secs(20)),
            fixture.request(),
            &subject("subject"),
        )
        .await;
    drop(observation);

    assert_matches!(
        result,
        Err(Error::Acquisition(AcquisitionError::Unavailable))
    );
    assert_eq!(fixture.token_request_count(), 1);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn a_token_that_expires_after_acquisition_is_not_sent_to_the_resource() {
    let fixture = Fixture::new().await;
    let mut response = exchange_response("short-lived");
    response["expires_in"] = 1.into();
    fixture.token_json(StatusCode::OK, &response);
    let client = fixture.exchange_client();
    let recorder = ObserveMetricRegistration(|key: &metrics::Key| {
        if key.name() == "oauth2_token_acquisitions_total"
            && key
                .labels()
                .any(|label| label.key() == "outcome" && label.value() == "success")
        {
            advance_during_metric(Duration::from_secs(1));
        }
    });
    let observation = metrics::set_default_local_recorder(&recorder);
    let result = fixture.send_exchange(&client, "subject").await;
    drop(observation);

    assert_matches!(
        result,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    );
    assert_eq!(fixture.token_request_count(), 1);
    assert_eq!(fixture.resource_request_count(), 0);
    fixture.finish().await;
}

fn advance_during_metric(duration: Duration) {
    tokio::time::pause();
    let mut advance = std::pin::pin!(tokio::time::advance(duration));
    let _ = advance
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
    tokio::time::resume();
}

struct ObserveMetricRegistration<F>(F);

impl<F: Fn(&metrics::Key)> metrics::Recorder for ObserveMetricRegistration<F> {
    fn describe_counter(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }

    fn describe_gauge(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }

    fn describe_histogram(
        &self,
        _: metrics::KeyName,
        _: Option<metrics::Unit>,
        _: metrics::SharedString,
    ) {
    }

    fn register_counter(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Counter {
        (self.0)(key);
        metrics::Counter::noop()
    }

    fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
        metrics::Gauge::noop()
    }

    fn register_histogram(
        &self,
        key: &metrics::Key,
        _: &metrics::Metadata<'_>,
    ) -> metrics::Histogram {
        (self.0)(key);
        metrics::Histogram::noop()
    }
}

#[tokio::test]
async fn cancellation_during_token_http_completion_is_recorded() {
    for delegated in [false, true] {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let caller = OperationContext::with_timeout(Duration::from_secs(10));
        let outcomes = Mutex::new(Vec::new());
        let recorder = ObserveMetricRegistration(|key: &metrics::Key| match key.name() {
            "http_client_request_duration_seconds" => caller.cancel(),
            "oauth2_token_acquisitions_total" => {
                let outcome = key.labels().find(|label| label.key() == "outcome").unwrap();
                outcomes.lock().unwrap().push(outcome.value().to_owned());
            }
            _ => {}
        });
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let result = if delegated {
            fixture.token_json(StatusCode::OK, &exchange_response("replacement"));
            let client = fixture.exchange_client();
            client.execute(&caller, request, &subject("subject")).await
        } else {
            let client = fixture.service_client();
            client.execute(&caller, request).await
        };
        assert_matches!(result, Err(Error::Acquisition(AcquisitionError::Cancelled)));
        assert_eq!(*outcomes.lock().unwrap(), ["cancelled"]);
        assert_eq!(fixture.resource_request_count(), 0);
        fixture.finish().await;
    }
}

async fn drive_cancelled_provider_completion(
    fixture: &Fixture,
    gate: &Arc<Semaphore>,
    mut first: Pin<Box<dyn Future<Output = Result<Response<Bytes>, Error>> + '_>>,
    mut survivor: Pin<Box<dyn Future<Output = Result<Response<Bytes>, Error>> + '_>>,
) {
    tokio::select! {
        () = fixture.token_received() => {}
        result = &mut first => panic!("provider response must be gated: {result:?}"),
    }
    poll_pending(survivor.as_mut()).await;
    gate.add_permits(1);
    assert_matches!(
        first.await,
        Err(Error::Acquisition(AcquisitionError::Cancelled))
    );
    assert_eq!(fixture.resource_request_count(), 0);

    fixture.token_json(StatusCode::OK, &exchange_response("replacement"));
    gate.add_permits(1);
    assert_eq!(survivor.await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn cancellation_at_provider_completion_leaves_a_live_caller_its_own_attempt() {
    for delegated in [false, true] {
        let fixture = Fixture::new().await;
        let request = fixture.request();
        let caller = OperationContext::with_timeout(Duration::from_secs(10));
        let first = request.clone();
        let recorder = ObserveMetricRegistration(|key: &metrics::Key| {
            if key.name() == "oauth2_token_acquisitions_total"
                && key
                    .labels()
                    .any(|label| label.key() == "outcome" && label.value() == "invalid_client")
            {
                caller.cancel();
            }
        });
        let _recorder = metrics::set_default_local_recorder(&recorder);
        fixture.token_json(
            StatusCode::BAD_REQUEST,
            &serde_json::json!({"error": "invalid_client"}),
        );
        let gate = fixture.block_tokens();
        if delegated {
            let client = fixture.exchange_client();
            let subject_token = subject("subject");
            drive_cancelled_provider_completion(
                &fixture,
                &gate,
                Box::pin(client.execute(&caller, first, &subject_token)),
                Box::pin(fixture.send_exchange_request(&client, request, "subject")),
            )
            .await;
        } else {
            let client = fixture.service_client();
            drive_cancelled_provider_completion(
                &fixture,
                &gate,
                Box::pin(client.execute(&caller, first)),
                Box::pin(fixture.send_request(&client, request)),
            )
            .await;
        }
        assert_eq!(fixture.token_request_count(), 2);
        assert_eq!(fixture.resource_request_count(), 1);
        fixture.finish().await;
    }
}

#[derive(Default)]
struct RecordedMetrics {
    acquisitions: BTreeMap<(String, String), u64>,
    outbound_samples: usize,
}

impl RecordedMetrics {
    fn drain(&mut self, snapshotter: &metrics_util::debugging::Snapshotter) {
        use metrics_util::debugging::DebugValue;

        for (key, _, _, value) in snapshotter.snapshot().into_vec() {
            match value {
                DebugValue::Counter(count)
                    if count > 0 && key.key().name() == "oauth2_token_acquisitions_total" =>
                {
                    let label = |name: &str| {
                        key.key()
                            .labels()
                            .find(|label| label.key() == name)
                            .unwrap()
                            .value()
                            .to_owned()
                    };
                    *self
                        .acquisitions
                        .entry((label("grant"), label("outcome")))
                        .or_default() += count;
                }
                DebugValue::Histogram(samples)
                    if key.key().name() == "http_client_request_duration_seconds" =>
                {
                    self.outbound_samples += samples.len();
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn every_token_request_is_counted_once_under_its_grant_and_closed_outcome() {
    let recorder = metrics_util::debugging::DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let mut captured = RecordedMetrics::default();
    let fixture = Fixture::new().await;
    let far = timeout_context(Duration::from_secs(10));
    let reused = fixture.service_client();
    for _ in 0..2 {
        reused.execute(&far, fixture.request()).await.unwrap();
    }
    fixture.token_raw(response(
        StatusCode::BAD_REQUEST,
        br#"{"error":"invalid_client"}"#,
    ));
    fixture
        .service_client()
        .execute(&far, fixture.request())
        .await
        .unwrap_err();
    fixture.token_raw(response(
        StatusCode::BAD_REQUEST,
        br#"{"error":"invalid_target"}"#,
    ));
    fixture
        .exchange_client()
        .execute(&far, fixture.request(), &subject("subject"))
        .await
        .unwrap_err();
    let gate = fixture.block_tokens();
    let timed = fixture.service_client();
    let short = OperationContext::with_timeout(Duration::from_secs(1));
    let mut timed = Box::pin(timed.execute(&short, fixture.request()));
    fixture.drive_until_token_received(timed.as_mut()).await;
    advance(Duration::from_secs(1)).await;
    assert_matches!(
        timed.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    let registration = prepare_registration(HttpOptions {
        provider_concurrency: nonzero(1),
        ..fixture.options()
    });
    let (service_tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), fixture.resource_origin());
    fixture.drive(driver);
    let dropped_client = service_tokens.http(fixture.resource_client()).unwrap();
    let exchange_client = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .unwrap();
    let mut dropped = Box::pin(dropped_client.execute(&far, fixture.request()));
    fixture.drive_until_token_received(dropped.as_mut()).await;
    captured.drain(&snapshotter);
    let before_capacity = captured.outbound_samples;
    assert!(before_capacity > 0);
    assert_matches!(
        exchange_client
            .execute(&far, fixture.request(), &subject("at-capacity"))
            .await,
        Err(Error::Acquisition(AcquisitionError::AtCapacity))
    );
    captured.drain(&snapshotter);
    assert_eq!(captured.outbound_samples, before_capacity);
    let mut waiter = Box::pin(dropped_client.execute(&far, fixture.request()));
    poll_pending(waiter.as_mut()).await;
    drop(waiter);
    drop(dropped);
    drop(gate);
    fixture.finish().await;
    captured.drain(&snapshotter);
    let count = |grant: &str, outcome: &str| ((grant.to_owned(), outcome.to_owned()), 1);
    assert_eq!(
        captured.acquisitions.into_iter().collect::<Vec<_>>(),
        [
            count("client_credentials", "cancelled"),
            count("client_credentials", "invalid_client"),
            count("client_credentials", "success"),
            count("client_credentials", "timeout"),
            count("token_exchange", "capacity"),
            count("token_exchange", "invalid_target"),
        ]
    );
}
