use super::*;

#[tokio::test]
async fn typed_handler_success_is_followed_by_confirmed_source_ack() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .cached_info()
        .config
        .clone();
    source.retention = stream::RetentionPolicy::WorkQueue;
    fixture.replace_stream(source).await;
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        &context(deadline(), &cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let (observed_tx, observed_rx) = oneshot::channel();
    let observed_tx = Arc::new(Mutex::new(Some(observed_tx)));
    let mut registry = fixture.registry.clone();
    registry
        .register(
            &fixture.route,
            move |event: Event<ExampleEvent>, _: OperationContext| {
                let sender = observed_tx
                    .lock()
                    .expect("test completion sender lock must not be poisoned")
                    .take();
                if let Some(sender) = sender {
                    let _ = sender.send(event.id);
                }
                async { Ok(()) }
            },
        )
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry, &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = fixture
        .publication
        .prepare(&event("event-consumer"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .await
        .expect("fixture event publication is acknowledged");

    let observed = timeout(Duration::from_secs(3), observed_rx)
        .await
        .expect("typed handler must observe the broker delivery")
        .expect("handler completion signal must remain connected");
    assert_eq!(observed, "event-consumer");
    wait_for_source_ack(&fixture).await;
    std::assert_matches!(
            fixture.jetstream.get_stream(&fixture.stream).await.unwrap().get_raw_message(1).await,
            Err(error) if matches!(error.kind(), stream::RawMessageErrorKind::NoMessageFound)
        ,
        "WorkQueue retention removes the acknowledged source record"
    );

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn handler_runs_in_the_trace_of_the_publication() {
    use tracing::Instrument as _;

    let exporter = install_tracing();
    let fixture = Fixture::create(Topology::WithDurable).await;
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        &context(deadline(), &cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let (observed_tx, observed_rx) = oneshot::channel();
    let observed_tx = Arc::new(Mutex::new(Some(observed_tx)));
    let mut registry = fixture.registry.clone();
    registry
        .register(
            &fixture.route,
            move |_: Event<ExampleEvent>, _: OperationContext| {
                let sender = observed_tx
                    .lock()
                    .expect("test completion sender lock must not be poisoned")
                    .take();
                if let Some(sender) = sender {
                    let _ = sender.send(trace_id(&tracing::Span::current()));
                }
                async { Ok(()) }
            },
        )
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry, &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = fixture
        .publication
        .prepare(&event("event-traced"))
        .expect("fixture event is prepared");
    let request = tracing::info_span!("request");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .instrument(request.clone())
        .await
        .expect("fixture event publication is acknowledged");

    let observed = timeout(Duration::from_secs(3), observed_rx)
        .await
        .expect("typed handler must observe the broker delivery")
        .expect("handler completion signal must remain connected");
    assert_ne!(observed, opentelemetry::trace::TraceId::INVALID);
    assert_eq!(observed, trace_id(&request));
    wait_for_source_ack(&fixture).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    for (name, kind, operation) in [
        (
            format!("publish {}", fixture.subject),
            opentelemetry::trace::SpanKind::Producer,
            "publish",
        ),
        (
            format!("process {}", fixture.subject),
            opentelemetry::trace::SpanKind::Consumer,
            "process",
        ),
    ] {
        let spans = exported_spans(exporter, &name);
        assert_eq!(spans.len(), 1, "{name}");
        let span = &spans[0];
        assert_eq!(span.span_context.trace_id(), observed, "{name}");
        assert_eq!(span.span_kind, kind, "{name}");
        assert_eq!(
            attribute(span, "messaging.system.name").as_deref(),
            Some("nats")
        );
        assert_eq!(
            attribute(span, "messaging.operation.name").as_deref(),
            Some(operation)
        );
        assert_eq!(attribute(span, "error.type"), None, "{name}");
        assert_eq!(span.status, opentelemetry::trace::Status::Unset, "{name}");
    }
    let process = &exported_spans(exporter, &format!("process {}", fixture.subject))[0];
    assert_eq!(
        attribute(process, "messaging.destination.template").as_deref(),
        Some(fixture.subject.as_str())
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn lost_source_ack_reply_leaves_handler_success_and_settlement_unconfirmed() {
    let exporter = install_tracing();
    let fixture = Fixture::create(Topology::WithDurable).await;
    let relay =
        AckDroppingRelay::start_source_ack(&fixture.stream, &fixture.durable, DroppedAck::Reply)
            .await;
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options_with_servers(
            &fixture,
            vec![relay.url.clone()],
            Some(consumer_options(&fixture)),
            1024,
        ),
        &context(deadline(), &cancel),
    )
    .await
    .expect("consumer connects through the settlement relay");
    let mut registry = fixture.registry.clone();
    registry
        .register(&fixture.route, accept)
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry, &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = fixture
        .publication
        .prepare(&event("event-lost-source-ack-reply"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .await
        .expect("fixture event publication is acknowledged");

    relay.wait_for_drop().await;
    wait_for_source_ack(&fixture).await;
    let process_name = format!("process {}", fixture.subject);
    let process = eventually(
        Duration::from_secs(8),
        "processing span must close after the ACK reply times out",
        async || exported_spans(exporter, &process_name).into_iter().next(),
    )
    .await;
    assert_eq!(attribute(&process, "outcome").as_deref(), Some("success"));
    assert_eq!(attribute(&process, "error.type"), None);
    assert_eq!(process.status, opentelemetry::trace::Status::Unset);

    let settlement = exported_settlement(exporter, "ack", &process);
    assert_eq!(
        attribute(&settlement, "outcome").as_deref(),
        Some("unconfirmed")
    );
    assert_eq!(
        attribute(&settlement, "error.type").as_deref(),
        Some("timeout")
    );
    assert!(matches!(
        settlement.status,
        opentelemetry::trace::Status::Error { .. }
    ));

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn retryable_handler_is_redelivered_after_broker_nak_then_confirmed_acked() {
    let exporter = install_tracing();
    let fixture = Fixture::create(Topology::WithDurable).await;
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        &context(deadline(), &cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = Arc::clone(&attempts);
    let (completed_tx, completed_rx) = oneshot::channel();
    let completed_tx = Arc::new(Mutex::new(Some(completed_tx)));
    let mut registry = fixture.registry.clone();
    registry
        .register(
            &fixture.route,
            move |_: Event<ExampleEvent>, _: OperationContext| {
                let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
                let completed_tx = Arc::clone(&completed_tx);
                async move {
                    if attempt == 0 {
                        Err(HandlerError::Retryable)
                    } else {
                        let sender = completed_tx
                            .lock()
                            .expect("test completion sender lock must not be poisoned")
                            .take();
                        if let Some(sender) = sender {
                            let _ = sender.send(());
                        }
                        Ok(())
                    }
                }
            },
        )
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry, &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = fixture
        .publication
        .prepare(&event("event-retry"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .await
        .expect("fixture event publication is acknowledged");

    timeout(Duration::from_secs(4), completed_rx)
        .await
        .expect("retryable delivery must be redelivered after the first NAK")
        .expect("handler completion signal must remain connected");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    wait_for_source_ack(&fixture).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    let deliveries = exported_spans(exporter, &format!("process {}", fixture.subject));
    let outcomes: Vec<_> = deliveries
        .iter()
        .map(|span| {
            (
                attribute(span, "outcome"),
                attribute(span, "error.type"),
                matches!(span.status, opentelemetry::trace::Status::Error { .. }),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (
                Some("retryable".to_owned()),
                Some("retryable".to_owned()),
                true
            ),
            (Some("success".to_owned()), None, false),
        ]
    );
    for (operation, process, outcome) in [
        ("nak", &deliveries[0], "submitted"),
        ("ack", &deliveries[1], "confirmed"),
    ] {
        let settlement = exported_settlement(exporter, operation, process);
        assert_eq!(attribute(&settlement, "outcome").as_deref(), Some(outcome));
        assert_eq!(attribute(&settlement, "error.type"), None);
        assert_eq!(settlement.status, opentelemetry::trace::Status::Unset);
    }
    close(messaging).await;
    fixture.cleanup().await;
}

fn exported_settlement(
    exporter: &opentelemetry_sdk::trace::InMemorySpanExporter,
    operation: &str,
    process: &opentelemetry_sdk::trace::SpanData,
) -> opentelemetry_sdk::trace::SpanData {
    let mut spans = exported_spans(exporter, operation)
        .into_iter()
        .filter(|span| span.parent_span_id == process.span_context.span_id());
    let span = spans.next().expect("process span has a settlement child");
    assert!(spans.next().is_none(), "process span settles once");
    assert_eq!(
        span.span_context.trace_id(),
        process.span_context.trace_id()
    );
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Client);
    assert_eq!(
        attribute(&span, "messaging.system.name").as_deref(),
        Some("nats")
    );
    assert_eq!(
        attribute(&span, "messaging.operation.type").as_deref(),
        Some("settle")
    );
    assert_eq!(
        attribute(&span, "messaging.operation.name").as_deref(),
        Some(operation)
    );
    span
}

#[tokio::test]
async fn handler_panic_is_retried_like_a_retryable_failure() {
    let fixture = Fixture::create(Topology::WithDurable).await;
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        &context(deadline(), &cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = Arc::clone(&attempts);
    let mut registry = fixture.registry.clone();
    registry
        .register(
            &fixture.route,
            move |_: Event<ExampleEvent>, _: OperationContext| {
                let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
                async move {
                    assert!(attempt > 0, "intentional first-delivery handler panic");
                    Ok(())
                }
            },
        )
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry, &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = fixture
        .publication
        .prepare(&event("event-panic"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .await
        .expect("fixture event publication is acknowledged");

    wait_for_source_ack(&fixture).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    handle
        .finish(deadline())
        .await
        .expect("a handler panic must not stop the consumer");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn drain_forces_unfinished_handler_shutdown_at_the_shared_deadline() {
    let fixture = Fixture::create(Topology::WithDurable).await;
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        &context(deadline(), &cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let started = Arc::new(Notify::new());
    let started_for_handler = Arc::clone(&started);
    let mut registry = fixture.registry.clone();
    registry
        .register(
            &fixture.route,
            move |_: Event<ExampleEvent>, _: OperationContext| {
                let started = Arc::clone(&started_for_handler);
                async move {
                    started.notify_one();
                    std::future::pending::<Result<(), HandlerError>>().await
                }
            },
        )
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry, &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = fixture
        .publication
        .prepare(&event("event-drain"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .await
        .expect("fixture event publication is acknowledged");
    timeout(Duration::from_secs(3), started.notified())
        .await
        .expect("handler must begin before drain is requested");

    let outcome = handle.finish(Instant::now() + Duration::from_secs(1)).await;
    std::assert_matches!(outcome, Err(ConsumerError::DrainTimedOut));
    wait_for_source_unacked(&fixture).await;

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn aborting_one_consumer_leaves_the_resource_serving() {
    let fixture = Fixture::create(Topology::WithDurable).await;
    let cancel = CancellationToken::new();
    let startup_deadline = deadline();
    let startup_cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        &context(startup_deadline, &startup_cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let handled = || {
        let mut registry = fixture.registry.clone();
        registry
            .register(&fixture.route, accept)
            .expect("fixture handler is registered");
        registry
    };
    let mut handle = messaging
        .consumer(handled(), &context(deadline(), &cancel))
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    handle.abort();
    let outcome = handle.finish(deadline()).await;
    std::assert_matches!(outcome, Err(ConsumerError::DrainTimedOut), "{outcome:?}");

    let prepared = fixture
        .publication
        .prepare(&event("event-after-abort"))
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&context(deadline(), &cancel), &prepared)
        .await
        .expect("publication continues after the owner aborted one consumer");
    startup_cancel.cancel();
    tokio::time::sleep_until(startup_deadline).await;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    std::assert_matches!(
        messaging
            .consumer(handled(), &context(deadline(), &cancelled))
            .await,
        Err(infra_messaging::MessagingError::Cancelled)
    );
    std::assert_matches!(
        messaging
            .consumer(handled(), &context(Instant::now(), &cancel))
            .await,
        Err(infra_messaging::MessagingError::TimedOut { .. })
    );
    let mut replacement = messaging
        .consumer(handled(), &context(deadline(), &cancel))
        .await
        .expect("a new consumer is admitted after the owner aborted one")
        .start(&cancel);
    replacement
        .finish(deadline())
        .await
        .expect("the replacement consumer drains");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one broker scenario proves admission, one-slot refill and drain together"
)]
async fn pulls_reserve_handler_slots_and_drain_leaves_unadmitted_messages_at_the_broker() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let cancel = CancellationToken::new();
    let consumer_options = ConsumerOptions {
        concurrency: const { std::num::NonZeroUsize::new(2).expect("2 is nonzero") },
        ..consumer_options(&fixture)
    };
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer_options), 1024),
        &context(Instant::now() + Duration::from_secs(30), &cancel),
    )
    .await
    .expect("fixture source stream is admitted");
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release_first = CancellationToken::new();
    let release_other = CancellationToken::new();
    let mut blocking = fixture.registry.clone();
    blocking
        .register(&fixture.route, {
            let release_first = release_first.clone();
            let release_other = release_other.clone();
            move |event: Event<ExampleEvent>, _: OperationContext| {
                let _ = started_tx.send(event.id.clone());
                let release = if event.id == "event-in-flight-1" {
                    release_first.clone()
                } else {
                    release_other.clone()
                };
                async move {
                    release.cancelled().await;
                    Ok(())
                }
            }
        })
        .expect("fixture handler is registered");
    for id in [
        "event-in-flight-1",
        "event-in-flight-2",
        "event-next",
        "event-unadmitted",
    ] {
        let prepared = fixture
            .publication
            .prepare(&event(id))
            .expect("fixture event is prepared");
        messaging
            .producer()
            .publish(&context(deadline(), &cancel), &prepared)
            .await
            .expect("fixture event publication is acknowledged");
    }
    let mut first = messaging
        .consumer(blocking, &context(deadline(), &cancel))
        .await
        .expect("adapter creates its named durable consumer")
        .start(&cancel);
    timeout(Duration::from_secs(3), async {
        let first_id = started_rx.recv().await.expect("first handler starts");
        let second_id = started_rx.recv().await.expect("second handler starts");
        assert_ne!(first_id, second_id);
        assert!([first_id.as_str(), second_id.as_str()].contains(&"event-in-flight-1"));
        assert!([first_id.as_str(), second_id.as_str()].contains(&"event-in-flight-2"));
        let observed_until = Instant::now() + Duration::from_secs(1);
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        while Instant::now() < observed_until {
            cadence.tick().await;
            let durable = durable(&fixture).await;
            assert_eq!(durable.cached_info().num_ack_pending, 2);
            assert_eq!(durable.cached_info().num_pending, 2);
            assert_eq!(durable.cached_info().num_redelivered, 0);
        }
    })
    .await
    .expect("two occupied slots must leave both other events at the broker");

    release_first.cancel();
    let next = timeout(Duration::from_secs(3), started_rx.recv())
        .await
        .expect("one freed slot admits the next event")
        .expect("handler channel remains connected");
    assert_eq!(next, "event-next");
    let durable = durable(&fixture).await;
    assert_eq!(durable.cached_info().num_ack_pending, 2);
    assert_eq!(durable.cached_info().num_pending, 1);

    first.drain();
    release_other.cancel();
    first
        .finish(deadline())
        .await
        .expect("the in-flight deliveries settle within the shared deadline");

    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut recording = fixture.registry.clone();
    recording
        .register(
            &fixture.route,
            move |event: Event<ExampleEvent>, _: OperationContext| {
                let _ = observed_tx.send(event.id);
                async { Ok(()) }
            },
        )
        .expect("fixture handler is registered");
    let mut second = messaging
        .consumer(recording, &context(deadline(), &cancel))
        .await
        .expect("the durable consumer is admitted again")
        .start(&cancel);
    let observed = timeout(Duration::from_secs(10), observed_rx.recv())
        .await
        .expect("an unadmitted event remains available without waiting for ack wait")
        .expect("handler completion signal must remain connected");
    assert_eq!(observed, "event-unadmitted");

    second
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}
