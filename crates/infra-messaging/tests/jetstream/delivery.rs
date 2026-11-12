//! Typed handler runs, their settlement, tracing, drain and pull capacity.

use super::*;

#[tokio::test]
async fn typed_handler_success_is_followed_by_confirmed_source_ack() {
    let fixture = Fixture::create(false).await;
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
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (observed_tx, observed_rx) = oneshot::channel();
    let observed_tx = Arc::new(Mutex::new(Some(observed_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |event, _| {
            if let Some(sender) = observed_tx
                .lock()
                .expect("test completion sender lock must not be poisoned")
                .take()
            {
                let _ = sender.send(event.id.clone());
            }
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-consumer"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
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
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (observed_tx, observed_rx) = oneshot::channel();
    let observed_tx = Arc::new(Mutex::new(Some(observed_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            if let Some(sender) = observed_tx
                .lock()
                .expect("test completion sender lock must not be poisoned")
                .take()
            {
                let _ = sender.send(trace_id(&tracing::Span::current()));
            }
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-traced"),
        1024,
    )
    .expect("fixture event is prepared");
    let request = tracing::info_span!("request");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
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
    // The exported names are low-cardinality: the subject of a publication
    // and the durable's filter for a delivery.
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
async fn retryable_handler_is_redelivered_after_broker_nak_then_confirmed_acked() {
    let exporter = install_tracing();
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = Arc::clone(&attempts);
    let (completed_tx, completed_rx) = oneshot::channel();
    let completed_tx = Arc::new(Mutex::new(Some(completed_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
            let completed_tx = Arc::clone(&completed_tx);
            async move {
                if attempt == 0 {
                    Err(HandlerError::Retryable)
                } else {
                    if let Some(sender) = completed_tx
                        .lock()
                        .expect("test completion sender lock must not be poisoned")
                        .take()
                    {
                        let _ = sender.send(());
                    }
                    Ok(())
                }
            }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-retry"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
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
    // The failed attempt's span is an error named by its closed outcome.
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
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn handler_panic_is_retried_like_a_retryable_failure() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_handler = Arc::clone(&attempts);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            let attempt = attempts_for_handler.fetch_add(1, Ordering::SeqCst);
            async move {
                assert!(attempt > 0, "intentional first-delivery handler panic");
                Ok(())
            }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-panic"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
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
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let started = Arc::new(Notify::new());
    let started_for_handler = Arc::clone(&started);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            let started = Arc::clone(&started_for_handler);
            async move {
                started.notify_one();
                std::future::pending::<Result<(), HandlerError>>().await
            }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-drain"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
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
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let handled = || {
        let mut registry = registry(&fixture);
        registry
            .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
            .expect("fixture handler is registered");
        registry
    };
    let mut handle = messaging
        .consumer(handled())
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    handle.abort();
    let outcome = handle.finish(deadline()).await;
    std::assert_matches!(outcome, Err(ConsumerError::DrainTimedOut), "{outcome:?}");

    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-after-abort"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("publication continues after the owner aborted one consumer");
    let mut replacement = messaging
        .consumer(handled())
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
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let consumer_options = ConsumerOptions {
        concurrency: std::num::NonZeroUsize::new(2).expect("two is nonzero"),
        ..consumer_options(&fixture)
    };
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options), 1024),
        Instant::now() + Duration::from_secs(30),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let release_first = CancellationToken::new();
    let release_other = CancellationToken::new();
    let mut blocking = registry(&fixture);
    blocking
        .register::<ExampleEvent, _, _>({
            let release_first = release_first.clone();
            let release_other = release_other.clone();
            move |event, _| {
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
        let prepared =
            infra_messaging::PreparedEvent::prepare(fixture.subject.clone(), &event(id), 1024)
                .expect("fixture event is prepared");
        messaging
            .producer()
            .publish(&prepared, &context(deadline(), &cancel))
            .await
            .expect("fixture event publication is acknowledged");
    }
    let mut first = messaging
        .consumer(blocking)
        .await
        .expect("adapter creates its named durable consumer")
        .start(&cancel);
    timeout(Duration::from_secs(3), async {
        let first_id = started_rx.recv().await.expect("first handler starts");
        let second_id = started_rx.recv().await.expect("second handler starts");
        assert_ne!(first_id, second_id);
        assert!([first_id.as_str(), second_id.as_str()].contains(&"event-in-flight-1"));
        assert!([first_id.as_str(), second_id.as_str()].contains(&"event-in-flight-2"));
        // Observe broker ownership while the slots remain occupied, including
        // the interval in which the old client stream issued its next batch.
        let observed_until = Instant::now() + Duration::from_secs(1);
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        while Instant::now() < observed_until {
            cadence.tick().await;
            let durable: consumer::PullConsumer = fixture
                .jetstream
                .get_consumer_from_stream(&fixture.durable, &fixture.stream)
                .await
                .expect("the durable consumer exists");
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
    let durable: consumer::PullConsumer = fixture
        .jetstream
        .get_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .expect("the durable remains observable");
    assert_eq!(durable.cached_info().num_ack_pending, 2);
    assert_eq!(durable.cached_info().num_pending, 1);

    first.drain();
    release_other.cancel();
    first
        .finish(deadline())
        .await
        .expect("the in-flight deliveries settle within the shared deadline");

    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut recording = registry(&fixture);
    recording
        .register::<ExampleEvent, _, _>(move |event, _| {
            let _ = observed_tx.send(event.id);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut second = messaging
        .consumer(recording)
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
