use std::{
    error::Error as _,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http::{Request, header};

use crate::{Error, Limits, UrlTemplate, test_support::*, tls::TlsMaterial};

const REQUEST_SENTINELS: [&str; 6] = [
    "sentinel-path",
    "sentinel-query",
    "sentinel-header",
    "sentinel-body",
    "sentinel-value",
    "SENTINEL-METHOD",
];

async fn exercise_completed_attempts() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let client = fixture_client_with_limits(
        "127.0.0.1:0".parse().expect("unused address"),
        &material,
        Limits {
            response_body_bytes: 2,
            ..limits()
        },
    );
    let unpolled = client.execute(request(), deadline());
    drop(unpolled);

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let success = fixture_client(address, &material)
        .execute(request(), deadline())
        .await;
    assert!(success.is_ok());
    server.await.expect("success fixture joins");

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let not_found = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("HTTP error statuses remain responses");
    assert_eq!(not_found.status(), http::StatusCode::NOT_FOUND);
    server.await.expect("not-found fixture joins");

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 600 Fixture Error\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let custom_error = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("600 status remains a response");
    assert_eq!(custom_error.status().as_u16(), 600);
    server.await.expect("600-status fixture joins");

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 999 Fixture Error\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let custom_error = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("999 status remains a response");
    assert_eq!(custom_error.status().as_u16(), 999);
    server.await.expect("999-status fixture joins");

    // Refused before I/O, so it is not an attempt.
    assert!(matches!(
        client
            .execute(request_to("https://other.fixture.test/items"), deadline())
            .await,
        Err(Error::InvalidTarget)
    ));

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 3\r\n\r\nno!",
    )
    .await;
    let mut request = Request::builder()
        .method(http::Method::from_bytes(b"SENTINEL-METHOD").expect("method"))
        .uri("https://provider.fixture.test/sentinel-path?token=sentinel-query")
        .header(header::AUTHORIZATION, "Bearer sentinel-header")
        .body(Bytes::from_static(b"sentinel-body"))
        .expect("sensitive fixture request");
    request
        .headers_mut()
        .insert("x-sentinel", "sentinel-value".parse().expect("header"));
    assert!(matches!(
        fixture_client_with_limits(
            address,
            &material,
            Limits {
                response_body_bytes: 2,
                ..limits()
            },
        )
        .execute(request, deadline())
        .await,
        Err(Error::ResponseBodyTooLarge)
    ));
    server.await.expect("body-limit fixture joins");
}

#[test]
fn observation_records_polled_attempts_once_without_request_data() {
    let recorder = observation_recorder();
    let diagnostics = SpanDiagnostics::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(diagnostics.clone(), || {
            // Keep callsite interest independent of a sibling test's thread-local
            // subscriber; tracing-core 0.1.36 otherwise has a single-dispatcher fast path.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(exercise_completed_attempts());
        });
    });

    let scrape = recorder.handle().render();
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "outbound_outcome=\"response\"",
                "http_request_method=\"GET\""
            ],
        ),
        4
    );
    for status in ["404", "600", "999"] {
        assert_eq!(
            recorded_count(
                &scrape,
                &[
                    "outbound_outcome=\"response\"",
                    &format!("error_type=\"{status}\""),
                    &format!("http_response_status_code=\"{status}\""),
                ],
            ),
            1,
            "{status} keeps matching status and error labels",
        );
    }
    assert_eq!(
        recorded_count(&scrape, &["error_type=\"invalid_target\""]),
        0
    );
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "outbound_outcome=\"error\"",
                "error_type=\"response_body_too_large\"",
                "http_request_method=\"_OTHER\"",
                "http_response_status_code=\"502\"",
            ],
        ),
        1
    );
    for secret in REQUEST_SENTINELS {
        assert!(!scrape.contains(secret), "metric disclosed request data");
    }
    assert!(
        !scrape.contains("url_template"),
        "a request without a template has no template label"
    );

    let spans = diagnostics.0.lock().expect("span diagnostic lock");
    assert_eq!(
        spans.len(),
        5,
        "unpolled futures and refused targets are not attempts"
    );
    let body_failure = spans
        .iter()
        .find(|fields| {
            fields
                .get("error.type")
                .is_some_and(|value| value.contains("response_body_too_large"))
        })
        .expect("body-limit attempt span");
    assert!(
        body_failure
            .get("http.response.status_code")
            .is_some_and(|value| value.contains("502"))
    );
    assert!(
        body_failure
            .get("http.request.method")
            .is_some_and(|value| value.contains("_OTHER"))
    );
    assert!(
        body_failure
            .get("otel.status_code")
            .is_some_and(|value| value.contains("ERROR"))
    );
    for fields in spans.iter() {
        let fields = format!("{fields:?}");
        for secret in REQUEST_SENTINELS {
            assert!(!fields.contains(secret), "span disclosed request data");
        }
    }
}

#[test]
fn url_template_names_the_operation_in_the_span_and_the_metric() {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt as _;

    let recorder = observation_recorder();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(subscriber, || {
            // See `observation_records_polled_attempts_once_without_request_data`.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(async {
                let material = TlsMaterial::new(FIXTURE_HOST);
                let (address, captured, server) = tls_server_capture(
                    &material,
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
                )
                .await;
                let mut request = request_to("https://provider.fixture.test/items/sentinel-path");
                request.extensions_mut().insert(UrlTemplate("/items/{id}"));
                fixture_client(address, &material)
                    .execute(request, deadline())
                    .await
                    .expect("HTTP error statuses remain responses");
                let wire = captured.await.expect("captured request");
                assert!(wire.starts_with(b"GET /items/sentinel-path HTTP/1.1\r\n"));
                server.await.expect("capture fixture server succeeds");
            });
        });
    });

    let scrape = recorder.handle().render();
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "url_template=\"/items/{id}\"",
                "http_request_method=\"GET\"",
                "http_response_status_code=\"404\"",
            ],
        ),
        1,
        "{scrape}"
    );
    assert!(
        !scrape.contains("sentinel-path"),
        "metric disclosed the path"
    );

    let spans = exporter.get_finished_spans().expect("exported spans");
    let [span] = spans.as_slice() else {
        panic!("one client span, got {spans:?}");
    };
    assert_eq!(span.name, "GET /items/{id}");
    assert!(span.attributes.iter().any(|attribute| {
        attribute.key.as_str() == "url.template" && attribute.value.as_str() == "/items/{id}"
    }));
    assert!(
        !format!("{span:?}").contains("sentinel-path"),
        "span disclosed the path"
    );
}

#[test]
fn observation_records_timeout_cancellation_and_polled_drop_once() {
    let recorder = observation_recorder();
    let diagnostics = SpanDiagnostics::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(diagnostics.clone(), || {
            // Keep callsite interest independent of a sibling test's thread-local
            // subscriber; tracing-core 0.1.36 otherwise has a single-dispatcher fast path.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(async {
                let material = TlsMaterial::new(FIXTURE_HOST);
                // The recorded 200 status proves the timeout struck after the
                // response headers, while the body was pending.
                let (address, server) = tls_server_stall_after_headers(&material).await;
                let timed = fixture_client(address, &material)
                    .execute(request(), deadline())
                    .await;
                assert!(matches!(timed, Err(Error::Timeout)));
                server.abort();
                assert!(
                    server
                        .await
                        .expect_err("timeout fixture aborts")
                        .is_cancelled()
                );

                let (address, server) = tls_server_stall_after_headers(&material).await;
                let client = fixture_client(address, &material);
                let mut exchange = Box::pin(client.execute(request(), deadline()));
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), &mut exchange)
                        .await
                        .is_err(),
                    "stalled exchange completed unexpectedly"
                );
                drop(exchange);
                server.abort();
                assert!(
                    server
                        .await
                        .expect_err("drop fixture aborts")
                        .is_cancelled()
                );

                let (address, server) = tls_server_stall_after_headers(&material).await;
                let context =
                    operation_context::OperationContext::with_timeout(Duration::from_secs(1));
                let client = fixture_client(address, &material);
                let mut exchange = Box::pin(client.execute_with_context(request(), &context));
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), &mut exchange)
                        .await
                        .is_err(),
                    "stalled exchange completed unexpectedly"
                );
                context.cancel();
                assert!(matches!(exchange.await, Err(Error::Cancelled)));
                server.abort();
                assert!(
                    server
                        .await
                        .expect_err("cancel fixture aborts")
                        .is_cancelled()
                );
            });
        });
    });

    let scrape = recorder.handle().render();
    assert_eq!(
        recorded_count(
            &scrape,
            &[
                "outbound_outcome=\"error\"",
                "error_type=\"timeout\"",
                "http_response_status_code=\"200\"",
            ],
        ),
        1
    );
    // A cancelled caller is not a provider failure, whether it cancels or
    // drops; both attempts share one series.
    assert!(scrape.lines().any(|line| {
        line.starts_with("http_client_request_duration_seconds_count")
            && line.contains("outbound_outcome=\"cancelled\"")
            && line.ends_with(" 2")
    }));
    assert!(!scrape.contains("error_type=\"cancelled\""));
    let spans = diagnostics.0.lock().expect("span diagnostic lock");
    assert_eq!(spans.len(), 3);
    assert_eq!(
        spans
            .iter()
            .filter(|fields| {
                fields
                    .get("outbound.outcome")
                    .is_some_and(|value| value.contains("cancelled"))
                    && !fields.contains_key("error.type")
            })
            .count(),
        2
    );
}

/// The name of the span an event was logged in, and the event's fields.
type EventFields = (Option<&'static str>, SpanFields);

/// Events of this crate.
#[derive(Clone, Default)]
struct EventDiagnostics(Arc<Mutex<Vec<EventFields>>>);

impl<S> tracing_subscriber::Layer<S> for EventDiagnostics
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if !event.metadata().target().starts_with("infra_outbound_http") {
            return;
        }
        let mut fields = FieldVisitor(SpanFields::new());
        event.record(&mut fields);
        let span = context.event_span(event).map(|span| span.name());
        self.0
            .lock()
            .expect("event diagnostic lock")
            .push((span, fields.0));
    }
}

/// One failure of each transport class, in the order `tls`, `connect`,
/// `protocol`, `transport`. Returns the deepest source of each error.
async fn exercise_transport_failures() -> Vec<String> {
    let sensitive_request = || {
        Request::get("https://provider.fixture.test/sentinel-path?token=sentinel-query")
            .header(header::AUTHORIZATION, "Bearer sentinel-header")
            .body(Bytes::from_static(b"sentinel-body"))
            .expect("sensitive fixture request")
    };
    let root_cause = |result: Result<http::Response<Bytes>, Error>| {
        let error = result.expect_err("transport failure");
        assert!(matches!(error, Error::Transport { .. }), "{error:?}");
        std::iter::successors(error.source(), |cause| (*cause).source())
            .last()
            .expect("transport source")
            .to_string()
    };
    let mut roots = Vec::new();
    let material = TlsMaterial::new(FIXTURE_HOST);

    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut untrusted = TlsMaterial::new(FIXTURE_HOST);
    untrusted.root = material.untrusted_root.clone();
    let untrusted = fixture_client(address, &untrusted)
        .execute(sensitive_request(), deadline())
        .await;
    roots.push(root_cause(untrusted));
    server.await.expect("untrusted fixture joins");

    let unresolved = fixture_client_resolving(FIXTURE_HOST, vec![], &material, limits())
        .execute(sensitive_request(), deadline())
        .await;
    roots.push(root_cause(unresolved));

    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Overflow: one\r\n\r\n",
    )
    .await;
    let overflow = fixture_client_with_limits(
        address,
        &material,
        Limits {
            response_header_count: 1,
            ..limits()
        },
    )
    .execute(sensitive_request(), deadline())
    .await;
    roots.push(root_cause(overflow));
    server.await.expect("parser fixture joins");

    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no").await;
    let truncated = fixture_client(address, &material)
        .execute(sensitive_request(), deadline())
        .await;
    roots.push(root_cause(truncated));
    server.await.expect("truncated fixture joins");
    roots
}

#[test]
fn transport_failures_are_classed_and_their_cause_is_logged_in_the_client_span() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let recorder = observation_recorder();
    let diagnostics = EventDiagnostics::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let roots = metrics::with_local_recorder(&recorder, || {
        let subscriber = tracing_subscriber::registry().with(diagnostics.clone());
        tracing::subscriber::with_default(subscriber, || {
            // See `observation_records_polled_attempts_once_without_request_data`.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            runtime.block_on(exercise_transport_failures())
        })
    });

    let scrape = recorder.handle().render();
    for (class, after_headers) in [
        ("tls", false),
        ("connect", false),
        ("protocol", false),
        // The body broke after a complete response head.
        ("transport", true),
    ] {
        let class_label = format!("error_type=\"{class}\"");
        assert_eq!(
            recorded_count(&scrape, &["outbound_outcome=\"error\"", &class_label]),
            1,
            "one {class} attempt in {scrape}",
        );
        assert_eq!(
            scrape
                .lines()
                .any(|line| line.contains(&class_label)
                    && line.contains("http_response_status_code")),
            after_headers,
            "{class} status label",
        );
    }

    let events = diagnostics.0.lock().expect("event diagnostic lock");
    let causes: Vec<&str> = events
        .iter()
        .map(|(span, fields)| {
            assert_eq!(*span, Some("outbound_http"));
            assert_eq!(fields["message"], "outbound_http_transport_failed");
            fields["error"].as_str()
        })
        .collect();
    let [tls, connect, _protocol, _transport] = causes.as_slice() else {
        panic!("one event for each transport failure, got {causes:?}");
    };
    assert!(tls.contains("peer certificate"), "{tls}");
    assert!(connect.contains("no fixture listener"), "{connect}");
    for (cause, root) in causes.iter().zip(&roots) {
        assert!(cause.ends_with(root.as_str()), "{cause} lacks {root}");
    }
    for cause in &causes {
        for secret in REQUEST_SENTINELS {
            assert!(!cause.contains(secret), "{cause} disclosed request data");
        }
    }
}
