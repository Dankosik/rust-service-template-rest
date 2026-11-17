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
    let registration = fixture.build(fixture.options());
    let (service_tokens, driver) = registration.service_tokens(token_target(
        &["write", "read+all", "write"],
        Some("https://api.example/é?a=b&c=d"),
    ));
    fixture.drive(driver);
    let response = service_tokens
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = fixture.token_requests().pop().unwrap();
    assert_eq!(token.target, TOKEN_PATH);
    assert!(token.header("authorization").is_none());
    assert_token_form(
        &token.body,
        "grant_type=client_credentials&scope=write+read%2Ball+write\
         &audience=https%3A%2F%2Fapi.example%2F%C3%A9%3Fa%3Db%26c%3Dd",
    );
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer token+/_~.=")
    );

    fixture.token_json(StatusCode::OK, &exchange_response("exchanged"));
    registration
        .exchange_tokens(
            token_target(&["read"], Some("https://api.example/é")),
            EXCHANGE_CAPACITY,
        )
        .unwrap()
        .http(fixture.resource_client())
        .execute(
            fixture.on_behalf_of_request("subject+/="),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let token = fixture.token_requests().pop().unwrap();
    assert_eq!(token.target, TOKEN_PATH);
    assert!(token.header("authorization").is_none());
    assert_token_form(
        &token.body,
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange\
         &subject_token=subject%2B%2F%3D\
         &subject_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token\
         &requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token\
         &scope=read&audience=https%3A%2F%2Fapi.example%2F%C3%A9",
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
    let registration = fixture.build(fixture.options());
    let bindings = ["catalog", "billing"].map(|audience| {
        let target = token_target(&[], Some(audience));
        let (service, driver) = registration.service_tokens(target.clone());
        fixture.drive(driver);
        let exchange = registration
            .exchange_tokens(target, EXCHANGE_CAPACITY)
            .unwrap();
        (
            audience,
            service.http(fixture.resource_client()),
            exchange.http(fixture.resource_client()),
        )
    });

    for (audience, service, exchange) in &bindings {
        for (grant, client, request) in [
            ("service", service, fixture.request()),
            (
                "exchange",
                exchange,
                fixture.on_behalf_of_request("same-subject"),
            ),
        ] {
            let token = format!("{audience}-{grant}");
            fixture.token_json(StatusCode::OK, &exchange_response(&token));
            fixture.send_request(client, request).await.unwrap();
        }
    }
    let audiences: Vec<_> = fixture
        .token_requests()
        .iter()
        .map(|request| form_value(&form_pairs(&request.body), "audience").to_owned())
        .collect();
    assert_eq!(audiences, ["catalog", "catalog", "billing", "billing"]);
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
async fn optional_scope_and_audience_are_omitted_from_the_client_credentials_form() {
    let fixture = Fixture::new().await;
    fixture
        .service_client()
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
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
    for _ in 0..2 {
        fixture
            .service_client()
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await
            .unwrap();
    }
    let requests = fixture.token_requests();
    assert_eq!(requests.len(), 2);
    let decoded = requests
        .iter()
        .map(|request| decode_assertion(&fixture, request))
        .collect::<Vec<_>>();
    for (header, claims) in &decoded {
        assert_eq!(header.alg, jsonwebtoken::Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("key-1"));
        assert_eq!(header.typ.as_deref(), Some("client-authentication+jwt"));
        assert_eq!(claims.iss, "client:id");
        assert_eq!(claims.sub, "client:id");
        assert_eq!(claims.aud, ASSERTION_AUDIENCE);
        assert_eq!(claims.exp - claims.iat, 60);
        assert_eq!(claims.iat, claims.nbf);
        let now = jsonwebtoken::get_current_timestamp();
        assert!((10..=15).contains(&(now - claims.iat)));
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
                fixture.on_behalf_of_request("subject"),
                deadline(Duration::from_secs(10)),
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
            .execute(fixture.request(), deadline(Duration::from_secs(10)))
            .await;
        assert_matches!(
            result,
            Err(Error::Acquisition(AcquisitionError::InvalidResponse))
        );
    }
    assert_eq!(fixture.token_requests().len(), malformed.len());
    assert!(fixture.resource_requests().is_empty());
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
        let before = fixture.token_requests().len();
        for _ in 0..2 {
            fixture.send(&client).await.unwrap();
        }
        assert_eq!(
            fixture.token_requests().len(),
            before + fetches_for_two_calls
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn invalid_service_and_exchanged_lifetimes_refuse_before_resource_dispatch() {
    let fixture = Fixture::new().await;
    for (client, request, lifetime) in [
        (fixture.service_client(), fixture.request(), u64::MAX.into()),
        (
            fixture.exchange_client(),
            fixture.on_behalf_of_request("subject"),
            u64::MAX.into(),
        ),
        (
            fixture.service_client(),
            fixture.request(),
            serde_json::Value::Null,
        ),
    ] {
        let mut response = exchange_response("invalid-lifetime");
        response["expires_in"] = lifetime;
        fixture.token_json(StatusCode::OK, &response);
        let error = client
            .execute(request, deadline(Duration::from_secs(10)))
            .await
            .unwrap_err();
        assert_matches!(error, Error::Acquisition(AcquisitionError::InvalidResponse));
    }
    assert_eq!(fixture.token_requests().len(), 3);
    assert!(fixture.resource_requests().is_empty());
    fixture.finish().await;
}

#[tokio::test]
async fn exchange_rejects_a_token_when_processing_crosses_the_attempt_deadline() {
    let fixture = Fixture::new().await;
    fixture.token_json(StatusCode::OK, &exchange_response("exchange-token"));
    let resource = Client::try_new_for_test_http(
        &fixture.origin,
        infra_outbound_http::Limits {
            operation_timeout: Duration::from_secs(20),
            ..TOKEN_LIMITS
        },
    )
    .unwrap();
    let client = fixture
        .exchange_tokens(&[], None, EXCHANGE_CAPACITY)
        .http(resource);
    let recorder = ObserveMetricRegistration(|key: &metrics::Key| {
        if key.name() == infra_outbound_http::REQUEST_DURATION_METRIC
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
            fixture.on_behalf_of_request("subject"),
            deadline(Duration::from_secs(20)),
        )
        .await;
    drop(observation);

    assert_matches!(
        result,
        Err(Error::Acquisition(AcquisitionError::Unavailable))
    );
    assert_eq!(fixture.token_requests().len(), 1);
    assert!(fixture.resource_requests().is_empty());
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
    let result = fixture
        .send_request(&client, fixture.on_behalf_of_request("subject"))
        .await;
    drop(observation);

    assert_matches!(
        result,
        Err(Error::Acquisition(AcquisitionError::InvalidResponse))
    );
    assert_eq!(fixture.token_requests().len(), 1);
    assert!(fixture.resource_requests().is_empty());
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
async fn cancellation_during_token_http_completion_is_recorded_for_both_parent_scopes() {
    for delegated in [false, true] {
        for extension in [false, true] {
            let fixture = Fixture::new().await;
            let (client, mut request) = if delegated {
                fixture.token_json(StatusCode::OK, &exchange_response("replacement"));
                (
                    fixture.exchange_client(),
                    fixture.on_behalf_of_request("subject"),
                )
            } else {
                (fixture.service_client(), fixture.request())
            };
            let caller = OperationContext::with_timeout(Duration::from_secs(10));
            let operation = OperationContext::with_timeout(Duration::from_secs(10));
            let context = if extension {
                request.extensions_mut().insert(caller.clone());
                &operation
            } else {
                &caller
            };
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
            assert_matches!(
                client.execute_with_context(request, context).await,
                Err(Error::Acquisition(AcquisitionError::Cancelled))
            );
            assert_eq!(*outcomes.lock().unwrap(), ["cancelled"]);
            assert!(fixture.resource_requests().is_empty());
            fixture.finish().await;
        }
    }
}

#[tokio::test]
async fn cancellation_at_provider_completion_leaves_a_live_caller_its_own_attempt() {
    for delegated in [false, true] {
        for extension in [false, true] {
            let fixture = Fixture::new().await;
            let (client, request) = if delegated {
                (
                    fixture.exchange_client(),
                    fixture.on_behalf_of_request("subject"),
                )
            } else {
                (fixture.service_client(), fixture.request())
            };
            let caller = OperationContext::with_timeout(Duration::from_secs(10));
            let mut first = request.clone();
            let operation = OperationContext::with_timeout(Duration::from_secs(10));
            let context = if extension {
                first.extensions_mut().insert(caller.clone());
                &operation
            } else {
                &caller
            };
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
            let mut first = Box::pin(client.execute_with_context(first, context));
            tokio::select! {
                () = fixture.token_received() => {}
                result = &mut first => panic!("provider response must be gated: {result:?}"),
            }
            let mut survivor = Box::pin(fixture.send_request(&client, request));
            poll_pending(survivor.as_mut()).await;
            gate.add_permits(1);
            assert_matches!(
                first.await,
                Err(Error::Acquisition(AcquisitionError::Cancelled))
            );
            assert!(fixture.resource_requests().is_empty());

            fixture.token_json(StatusCode::OK, &exchange_response("replacement"));
            gate.add_permits(1);
            assert_eq!(survivor.await.unwrap().status(), StatusCode::OK);
            assert_eq!(fixture.token_requests().len(), 2);
            assert_eq!(fixture.resource_requests().len(), 1);
            fixture.finish().await;
        }
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
    let far = || deadline(Duration::from_secs(10));
    let reused = fixture.service_client();
    for _ in 0..2 {
        reused.execute(fixture.request(), far()).await.unwrap();
    }
    fixture.token_raw(response(
        StatusCode::BAD_REQUEST,
        br#"{"error":"invalid_client"}"#,
    ));
    fixture
        .service_client()
        .execute(fixture.request(), far())
        .await
        .unwrap_err();
    fixture.token_raw(response(
        StatusCode::BAD_REQUEST,
        br#"{"error":"invalid_target"}"#,
    ));
    fixture
        .exchange_client()
        .execute(fixture.on_behalf_of_request("subject"), far())
        .await
        .unwrap_err();
    let gate = fixture.block_tokens();
    let timed = fixture.service_client();
    let mut request = fixture.request();
    request
        .extensions_mut()
        .insert(OperationContext::with_timeout(Duration::from_secs(1)));
    let mut timed = Box::pin(timed.execute(request, far()));
    tokio::select! { () = fixture.token_received() => {}, result = &mut timed => panic!("response must be gated: {result:?}"), }
    advance(Duration::from_secs(1)).await;
    assert_matches!(
        timed.await,
        Err(Error::Acquisition(AcquisitionError::Timeout))
    );
    let registration = fixture.build(Options {
        provider_concurrency: 1,
        ..fixture.options()
    });
    let (service_tokens, driver) = registration.service_tokens(TokenTarget::default());
    fixture.drive(driver);
    let dropped_client = service_tokens.http(fixture.resource_client());
    let exchange_client = registration
        .exchange_tokens(TokenTarget::default(), EXCHANGE_CAPACITY)
        .unwrap()
        .http(fixture.resource_client());
    let mut dropped = Box::pin(dropped_client.execute(fixture.request(), far()));
    tokio::select! { () = fixture.token_received() => {}, result = &mut dropped => panic!("response must be gated: {result:?}"), }
    captured.drain(&snapshotter);
    let before_capacity = captured.outbound_samples;
    assert!(before_capacity > 0);
    assert_matches!(
        exchange_client
            .execute(fixture.on_behalf_of_request("at-capacity"), far())
            .await,
        Err(Error::Acquisition(AcquisitionError::AtCapacity))
    );
    captured.drain(&snapshotter);
    assert_eq!(captured.outbound_samples, before_capacity);
    let mut waiter = Box::pin(dropped_client.execute(fixture.request(), far()));
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
