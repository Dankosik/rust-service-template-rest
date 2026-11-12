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

/// Decodes and verifies a captured request's `client_assertion` field with
/// the fixture's matching public key.
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

#[tokio::test]
async fn client_credentials_request_has_no_authorization_header_and_expected_fields() {
    let fixture = Fixture::new().await;
    fixture.token_json(
        "200 OK",
        &serde_json::json!({
            "access_token": "token+/_~.=",
            "token_type": "bEaReR",
            "expires_in": 60,
            "refresh_token": "private-refresh",
        }),
    );
    let response = fixture
        .credentials(&["read", "write"], Some("https://api.example/resource"))
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let token = fixture.token_requests().pop().unwrap();
    assert_eq!(token.target, TOKEN_PATH);
    assert!(token.header("authorization").is_none());
    let pairs = form_pairs(&token.body);
    let keys = pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "grant_type",
            "scope",
            "audience",
            "client_id",
            "client_assertion_type",
            "client_assertion",
        ]
    );
    assert_eq!(
        form_value(&pairs, "grant_type"),
        GRANT_TYPE_CLIENT_CREDENTIALS
    );
    assert_eq!(form_value(&pairs, "client_id"), "client:id");
    assert_eq!(
        form_value(&pairs, "client_assertion_type"),
        CLIENT_ASSERTION_TYPE
    );
    assert_ne!(form_value(&pairs, "client_assertion"), "");
    assert_eq!(form_value(&pairs, "scope"), "read write");
    assert_eq!(
        form_value(&pairs, "audience"),
        "https://api.example/resource"
    );
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer token+/_~.=")
    );
    fixture.finish().await;
}

#[tokio::test]
async fn optional_scope_and_audience_are_omitted_from_the_client_credentials_form() {
    let fixture = Fixture::new().await;
    fixture
        .credentials(&[], None)
        .http(fixture.resource_client())
        .execute(fixture.request(), deadline(Duration::from_secs(10)))
        .await
        .unwrap();
    let pairs = form_pairs(&fixture.token_requests()[0].body);
    let keys = pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "grant_type",
            "client_id",
            "client_assertion_type",
            "client_assertion"
        ]
    );
    fixture.finish().await;
}

#[tokio::test]
async fn assertion_header_and_claims_are_verifiable_and_two_requests_differ_in_jti() {
    let fixture = Fixture::new().await;
    for _ in 0..2 {
        fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
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
        // Dated ten seconds back for a provider whose clock is behind.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!((10..=15).contains(&(now - claims.iat)));
        assert_ne!(claims.jti, "");
    }
    assert_ne!(decoded[0].1.jti, decoded[1].1.jti);
    fixture.finish().await;
}

#[tokio::test]
async fn token_exchange_request_has_expected_form_fields() {
    let fixture = Fixture::new().await;
    fixture.token_json("200 OK", &exchange_response("exchanged"));
    fixture
        .credentials(&["read"], Some("https://api.example/resource"))
        .http(fixture.resource_client())
        .execute(
            fixture.on_behalf_of_request("subject-token"),
            deadline(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let token = fixture.token_requests().pop().unwrap();
    assert!(token.header("authorization").is_none());
    let pairs = form_pairs(&token.body);
    let keys = pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "grant_type",
            "subject_token",
            "subject_token_type",
            "requested_token_type",
            "scope",
            "audience",
            "client_id",
            "client_assertion_type",
            "client_assertion",
        ]
    );
    assert_eq!(form_value(&pairs, "grant_type"), GRANT_TYPE_TOKEN_EXCHANGE);
    assert_eq!(form_value(&pairs, "subject_token"), "subject-token");
    assert_eq!(form_value(&pairs, "subject_token_type"), ACCESS_TOKEN_TYPE);
    assert_eq!(
        form_value(&pairs, "requested_token_type"),
        ACCESS_TOKEN_TYPE
    );
    assert_eq!(form_value(&pairs, "scope"), "read");
    assert_eq!(
        form_value(&pairs, "audience"),
        "https://api.example/resource"
    );
    assert_eq!(form_value(&pairs, "client_id"), "client:id");
    assert_eq!(
        form_value(&pairs, "client_assertion_type"),
        CLIENT_ASSERTION_TYPE
    );
    assert_ne!(form_value(&pairs, "client_assertion"), "");
    assert_eq!(
        fixture.resource_requests()[0].header("authorization"),
        Some("Bearer exchanged")
    );
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
        fixture.token_json("200 OK", &body);
        let result = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
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
            "200 OK",
            &serde_json::json!({"access_token": token, "token_type": "BEARER", "expires_in": 60}),
        );
        let result = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
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
        fixture.token_json("200 OK", &body);
        let client = fixture
            .credentials(&[], None)
            .http(fixture.resource_client());
        let before = fixture.token_requests().len();
        for _ in 0..2 {
            client
                .execute(fixture.request(), deadline(Duration::from_secs(10)))
                .await
                .unwrap();
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
    for (request, lifetime) in [
        (fixture.request(), u64::MAX.into()),
        (fixture.on_behalf_of_request("subject"), u64::MAX.into()),
        (fixture.request(), serde_json::Value::Null),
    ] {
        let mut response = exchange_response("invalid-lifetime");
        response["expires_in"] = lifetime;
        fixture.token_json("200 OK", &response);
        let error = fixture
            .credentials(&[], None)
            .http(fixture.resource_client())
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
    fixture.token_json("200 OK", &exchange_response("exchange-token"));
    let resource = Client::try_new_for_test_http(
        &fixture.origin,
        infra_outbound_http::Limits {
            operation_timeout: Duration::from_secs(20),
            ..TOKEN_LIMITS
        },
    )
    .unwrap();
    let client = fixture.credentials(&[], None).http(resource);
    let recorder = ObserveMetricRegistration(|key: &metrics::Key| {
        if key.name() == infra_outbound_http::REQUEST_DURATION_METRIC
            && key
                .labels()
                .any(|label| label.key() == "http.request.method" && label.value() == "POST")
            && key
                .labels()
                .any(|label| label.key() == "outbound.outcome" && label.value() == "response")
        {
            // The transport has accepted the buffered response. Cross the
            // attempt deadline before control returns to OAuth for decoding.
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
    fixture.token_json("200 OK", &response);
    let client = fixture
        .credentials(&[], None)
        .http(fixture.resource_client());
    let recorder = ObserveMetricRegistration(|key: &metrics::Key| {
        if key.name() == "oauth2_token_acquisitions_total"
            && key
                .labels()
                .any(|label| label.key() == "outcome" && label.value() == "success")
        {
            // Advance after successful admission, before returning to the binding.
            advance_during_metric(Duration::from_secs(1));
        }
    });
    let observation = metrics::set_default_local_recorder(&recorder);
    let result = client
        .execute(
            fixture.on_behalf_of_request("subject"),
            deadline(Duration::from_secs(10)),
        )
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
async fn cancellation_at_provider_completion_leaves_a_live_caller_its_own_attempt() {
    for on_behalf_of in [false, true] {
        for extension in [false, true] {
            let fixture = Fixture::new().await;
            let client = fixture
                .credentials(&[], None)
                .http(fixture.resource_client());
            let request = || {
                if on_behalf_of {
                    fixture.on_behalf_of_request("subject")
                } else {
                    fixture.request()
                }
            };
            let caller = OperationContext::with_timeout(Duration::from_secs(10));
            let mut first = request();
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
            // The real provider response is already decoded when its terminal
            // counter registers, so cancellation arrives during acquisition polling.
            let _recorder = metrics::set_default_local_recorder(&recorder);
            fixture.token_json(
                "400 Bad Request",
                &serde_json::json!({"error": "invalid_client"}),
            );
            let gate = fixture.block_tokens();
            let mut first = Box::pin(client.execute_with_context(first, context));
            tokio::select! {
                () = fixture.token_received() => {}
                result = &mut first => panic!("provider response must be gated: {result:?}"),
            }
            let mut survivor =
                Box::pin(client.execute(request(), deadline(Duration::from_secs(10))));
            poll_pending(survivor.as_mut()).await;
            gate.add_permits(1);
            assert_matches!(
                first.await,
                Err(Error::Acquisition(AcquisitionError::Cancelled))
            );
            assert!(fixture.resource_requests().is_empty());

            fixture.token_json("200 OK", &exchange_response("replacement"));
            gate.add_permits(1);
            assert_eq!(survivor.await.unwrap().status(), StatusCode::OK);
            assert_eq!(fixture.token_requests().len(), 2);
            assert_eq!(fixture.resource_requests().len(), 1);
            fixture.finish().await;
        }
    }
}

/// The `oauth2_token_acquisitions_total` series a scrape holds, as
/// `(grant, outcome, count)`.
fn acquisition_counts(scrape: &str) -> Vec<(String, String, u64)> {
    let label = |line: &str, name: &str| {
        let (_, rest) = line.split_once(&format!("{name}=\"")).unwrap();
        rest.split_once('"').unwrap().0.to_owned()
    };
    let mut counts: Vec<_> = scrape
        .lines()
        .filter(|line| line.starts_with("oauth2_token_acquisitions_total{"))
        .map(|line| {
            let count = line.rsplit_once(' ').unwrap().1.parse().unwrap();
            (label(line, "grant"), label(line, "outcome"), count)
        })
        .collect();
    counts.sort();
    counts
}

#[tokio::test]
async fn every_token_request_is_counted_once_under_its_grant_and_closed_outcome() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let fixture = Fixture::new().await;
    let client = || {
        fixture
            .prepare(Options {
                provider_concurrency: 1,
                ..fixture.options(&[], None)
            })
            .http(fixture.resource_client())
    };
    let far = || deadline(Duration::from_secs(10));
    // One request, then a reuse that makes none.
    let reused = client();
    for _ in 0..2 {
        reused.execute(fixture.request(), far()).await.unwrap();
    }
    fixture.token_raw(response(
        "400 Bad Request",
        br#"{"error":"invalid_client"}"#,
    ));
    client()
        .execute(fixture.request(), far())
        .await
        .unwrap_err();
    fixture.token_raw(response(
        "400 Bad Request",
        br#"{"error":"invalid_target"}"#,
    ));
    client()
        .execute(fixture.on_behalf_of_request("subject"), far())
        .await
        .unwrap_err();
    let gate = fixture.block_tokens();
    let timed = client();
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
    let dropped = client();
    let dropped_client = dropped;
    let mut dropped = Box::pin(dropped_client.execute(fixture.request(), far()));
    tokio::select! { () = fixture.token_received() => {}, result = &mut dropped => panic!("response must be gated: {result:?}"), }
    let outbound_counts = || {
        let mut counts = recorder
            .handle()
            .render()
            .lines()
            .filter(|line| line.starts_with("http_client_request_duration_seconds_count"))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        counts.sort();
        counts
    };
    let before_capacity = outbound_counts();
    assert_ne!(before_capacity, [] as [String; 0]);
    assert_matches!(
        dropped_client
            .execute(fixture.on_behalf_of_request("at-capacity"), far())
            .await,
        Err(Error::Acquisition(AcquisitionError::AtCapacity))
    );
    assert_eq!(outbound_counts(), before_capacity);
    let mut waiter = Box::pin(dropped_client.execute(fixture.request(), far()));
    poll_pending(waiter.as_mut()).await;
    drop(waiter);
    drop(dropped);
    drop(gate);
    fixture.finish().await;
    let count = |grant: &str, outcome: &str| (grant.to_owned(), outcome.to_owned(), 1);
    assert_eq!(
        acquisition_counts(&recorder.handle().render()),
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
