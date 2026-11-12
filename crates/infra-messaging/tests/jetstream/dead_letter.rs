//! Dead-letter transfer and redrive.

use super::*;

#[tokio::test]
async fn permanent_failure_transfers_original_record_then_redrive_keeps_logical_identity() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (called_tx, called_rx) = oneshot::channel();
    let called_tx = Arc::new(Mutex::new(Some(called_tx)));
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            if let Some(sender) = called_tx
                .lock()
                .expect("test handler signal lock must not be poisoned")
                .take()
            {
                let _ = sender.send(());
            }
            async { Err(HandlerError::Permanent) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-permanent"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("fixture event publication is acknowledged");
    timeout(Duration::from_secs(3), called_rx)
        .await
        .expect("permanent handler must receive one delivery")
        .expect("handler signal must remain connected");

    let dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "permanent",
        prepared.payload().as_ref(),
    );
    assert_eq!(
        dead_letter
            .headers
            .get("Message-Id")
            .map(ToString::to_string),
        Some("event-permanent".to_owned())
    );
    wait_for_source_ack(&fixture).await;
    handle
        .finish(deadline())
        .await
        .expect("terminal transfer worker can drain after source settlement");

    fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .get_raw_message(1)
        .await
        .expect("Limits retention keeps the source after confirmed consumer ACK");
    let record = infra_messaging::wire::DeadLetterRecord {
        headers: dead_letter.headers.clone(),
        payload: dead_letter.payload.clone(),
        stream: fixture.dlq_stream.clone(),
        stream_sequence: dead_letter.sequence,
        stored_at: dead_letter.time,
    };
    let redrive = infra_messaging::wire::restore_dead_letter(record.clone())
        .expect("real DLQ record is restorable");
    let repeated = infra_messaging::wire::restore_dead_letter(record)
        .expect("the same DLQ record is repeatedly restorable");
    assert_eq!(redrive.message_id(), "event-permanent");
    assert!(redrive.publication_id().starts_with("redrive-"));
    assert_eq!(redrive.publication_id(), repeated.publication_id());
    let redrive_ack = messaging
        .producer()
        .publish(&redrive, &context(deadline(), &cancel))
        .await
        .expect("redrive publication receives a JetStream acknowledgment");
    assert_eq!(redrive_ack.stream, fixture.stream);

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn lost_dlq_or_source_ack_reply_preserves_the_transfer_and_settles_without_another_dlq_record()
 {
    for lose_dlq_ack in [true, false] {
        let fixture = Fixture::create(false).await;
        let relay = if lose_dlq_ack {
            AckDroppingRelay::start(&fixture.dlq_stream).await
        } else {
            AckDroppingRelay::start_source_ack(&fixture.stream, &fixture.durable, false).await
        };
        let cancel = CancellationToken::new();
        let messaging = Messaging::connect(
            options_with_servers(
                &fixture,
                vec![relay.url.clone()],
                Some(consumer_options(&fixture)),
                1024,
            ),
            deadline(),
            cancel.clone(),
        )
        .await
        .unwrap();
        let mut registered = registry(&fixture);
        registered
            .register::<ExampleEvent, _, _>(|_, _| async { Err(HandlerError::Permanent) })
            .unwrap();
        let prepared = registered
            .prepare(&event("lost-transfer-ack"), 1024)
            .unwrap();
        let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);
        messaging
            .producer()
            .publish(&prepared, &context(deadline(), &cancel))
            .await
            .unwrap();
        let first = wait_for_dead_letter(&fixture).await;
        relay.wait_for_drop().await;
        assert_dead_letter(&first, &fixture, "permanent", prepared.payload().as_ref());
        if lose_dlq_ack {
            wait_for_source_unacked(&fixture).await;
        }
        // A lost DLQ PubAck spends the five-second request budget, then the
        // adapter requests the broker's actual 30-second delayed redelivery.
        wait_for_source_ack_within(&fixture, 1, Duration::from_secs(45)).await;
        handle
            .finish(Instant::now() + Duration::from_secs(10))
            .await
            .unwrap();
        let dlq = fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .unwrap();
        assert_eq!(
            dlq.cached_info().state.messages,
            1,
            "same source identity deduplicates the DLQ retry"
        );
        let retained = dlq.get_raw_message(first.sequence).await.unwrap();
        assert_eq!(retained.payload, first.payload);
        assert_eq!(
            retained.headers.get("Nats-Msg-Id"),
            first.headers.get("Nats-Msg-Id")
        );
        close(messaging).await;
        relay.join().await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn malformed_and_unknown_envelopes_bypass_the_typed_handler_and_transfer_to_dlq() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut handlers = registry(&fixture);
    handlers
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(handlers)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let malformed = Bytes::from_static(b"not-a-go-compatible-envelope");
    fixture
        .jetstream
        .publish(fixture.subject.clone(), malformed.clone())
        .await
        .expect("malformed fixture message is accepted by the source stream")
        .await
        .expect("malformed fixture message receives a source publication acknowledgment");

    let malformed_dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &malformed_dead_letter,
        &fixture,
        "malformed",
        malformed.as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack(&fixture).await;

    let unknown = registry(&fixture)
        .prepare(&event("event-unknown"), 1024)
        .expect("unknown-type fixture starts from a valid production envelope");
    let mut unknown_headers = infra_messaging::wire::encode_prepared(&unknown)
        .expect("unknown-type fixture has valid identity headers");
    unknown_headers.insert("Event-Type", "test.example.unknown");
    fixture
        .jetstream
        .publish_with_headers(
            fixture.subject.clone(),
            unknown_headers,
            unknown.payload().clone(),
        )
        .await
        .expect("unknown-type fixture message is accepted by the source stream")
        .await
        .expect("unknown-type fixture message receives a source publication acknowledgment");

    let dead_letter = wait_for_dead_letter_after(&fixture, malformed_dead_letter.sequence).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "permanent",
        unknown.payload().as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack_at_least(&fixture, 2).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn unhandled_and_undecodable_deliveries_share_a_dead_letter_reason_and_differ_in_the_span() {
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
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut handlers = registry(&fixture);
    handlers
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(handlers)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    // A schema version published before its consumer was deployed, then the
    // handled version with a payload that is not the handler's type.
    let newer = registry(&fixture)
        .prepare(&event("event-newer-schema"), 1024)
        .expect("newer-schema fixture starts from a valid production envelope");
    let mut newer_headers = infra_messaging::wire::encode_prepared(&newer)
        .expect("newer-schema fixture has valid identity headers");
    newer_headers.insert("Event-Schema", "v2");
    let undecodable = registry(&fixture)
        .prepare(&event("event-undecodable"), 1024)
        .expect("undecodable fixture starts from a valid production envelope");
    let undecodable_headers = infra_messaging::wire::encode_prepared(&undecodable)
        .expect("undecodable fixture has valid identity headers");
    let undecodable_payload = Bytes::from_static(br#"{"value":7}"#);

    let mut transferred = 0;
    for (headers, payload) in [
        (newer_headers, newer.payload().clone()),
        (undecodable_headers, undecodable_payload),
    ] {
        fixture
            .jetstream
            .publish_with_headers(fixture.subject.clone(), headers, payload.clone())
            .await
            .expect("fixture message is accepted by the source stream")
            .await
            .expect("fixture message receives a source publication acknowledgment");
        let dead_letter = wait_for_dead_letter_after(&fixture, transferred).await;
        assert_dead_letter(&dead_letter, &fixture, "permanent", payload.as_ref());
        transferred = dead_letter.sequence;
    }
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack_at_least(&fixture, 2).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    let causes: Vec<_> = exported_spans(exporter, &format!("process {}", fixture.subject))
        .iter()
        .map(|span| (attribute(span, "outcome"), attribute(span, "error.type")))
        .collect();
    assert_eq!(
        causes,
        [
            (Some("unhandled".to_owned()), Some("unhandled".to_owned())),
            (
                Some("undecodable".to_owned()),
                Some("undecodable".to_owned())
            ),
        ]
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn definite_dlq_refusal_keeps_the_source_for_redelivery_and_the_consumer_running() {
    let fixture = Fixture::create_with_limits(true, 10, 1).await;
    fixture
        .jetstream
        .publish(
            fixture.dlq_subject.clone(),
            Bytes::from_static(b"occupied-dlq"),
        )
        .await
        .expect("fixture fills its refusal-control DLQ stream")
        .await
        .expect("fixture DLQ fill receives an acknowledgment");
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(|_, _| async { Err(HandlerError::Permanent) })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-dlq-refused"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("fixture event publication is acknowledged");

    wait_for_source_unacked(&fixture).await;
    assert!(
        timeout(Duration::from_secs(2), handle.failed())
            .await
            .is_err(),
        "a refused dead-letter transfer must not stop the consumer"
    );
    let dlq = fixture
        .jetstream
        .get_stream(&fixture.dlq_stream)
        .await
        .expect("fixture DLQ stream remains available")
        .get_info()
        .await
        .expect("fixture DLQ state is observable");
    assert_eq!(
        dlq.state.messages, 1,
        "only the fixture fill reached the DLQ"
    );
    handle
        .finish(deadline())
        .await
        .expect("the consumer drains after a refused transfer");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn sixth_delivery_bypasses_the_handler_and_transfers_as_exhausted() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let prepared = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event("event-exhausted"),
        1024,
    )
    .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("fixture event publication is acknowledged");
    redeliver_until_attempt(&fixture, 6).await;

    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Err(HandlerError::Retryable) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    let dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "exhausted",
        prepared.payload().as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack(&fixture).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain must join its pull task");
    close(messaging).await;
    fixture.cleanup().await;
}

async fn retain_record_before_shrinking_source(
    fixture: &Fixture,
    cancel: &CancellationToken,
) -> infra_messaging::PreparedEvent {
    let source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source is available for the operator update");
    let mut source_config = source.cached_info().config.clone();
    source_config.max_message_size = 24 * 1024;
    fixture
        .jetstream
        .update_stream(source_config.clone())
        .await
        .expect("the original stream limit fits the retained event");
    let producer = Box::pin(Messaging::connect(
        options(fixture, None, 16 * 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture producer is admitted against the original source bound");
    let oversized = registry(fixture)
        .prepare(
            &event_with_value("event-oversized", &"x".repeat(12 * 1024)),
            16 * 1024,
        )
        .expect("fixture event is valid for the original producer bound");
    producer
        .producer()
        .publish(&oversized, &context(deadline(), cancel))
        .await
        .expect("source retains the event before the operator shrinks its bound");
    close(producer).await;

    source_config.max_message_size = 32 + 8 * 1024;
    let updated = fixture
        .jetstream
        .update_stream(source_config)
        .await
        .expect("operator can shrink the stream limit without deleting retained records");
    assert_eq!(updated.state.messages, 1);
    oversized
}

#[tokio::test]
async fn oversized_source_message_is_dead_lettered_without_invoking_the_handler() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let oversized = retain_record_before_shrinking_source(&fixture, &cancel).await;
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 32),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted for the smaller delivery bound");
    let handler_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = Arc::clone(&handler_calls);
    let mut registry = registry(&fixture);
    registry
        .register::<ExampleEvent, _, _>(move |_, _| {
            calls_for_handler.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .expect("fixture handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("operator-provisioned durable consumer is admitted")
        .start(&cancel);

    let dead_letter = wait_for_dead_letter(&fixture).await;
    assert_dead_letter(
        &dead_letter,
        &fixture,
        "malformed",
        oversized.payload().as_ref(),
    );
    assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
    wait_for_source_ack(&fixture).await;
    let next = infra_messaging::PreparedEvent::prepare(
        fixture.subject.clone(),
        &event_with_value("event-after-oversized", "ok"),
        32,
    )
    .expect("ordinary event fits the reduced limit");
    let ack = messaging
        .producer()
        .publish(&next, &context(deadline(), &cancel))
        .await
        .expect("ordinary event publishes after the oversized record is settled");
    wait_for_source_ack_at_least(&fixture, ack.sequence).await;
    assert_eq!(handler_calls.load(Ordering::SeqCst), 1);
    handle
        .finish(deadline())
        .await
        .expect("an oversized delivery must not stop the consumer");

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn oversized_dead_letter_stops_the_consumer_and_keeps_the_source_unacknowledged() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let _oversized = retain_record_before_shrinking_source(&fixture, &cancel).await;
    let dlq = fixture
        .jetstream
        .get_stream(&fixture.dlq_stream)
        .await
        .expect("fixture DLQ is available for the operator update");
    let mut dlq_config = dlq.cached_info().config.clone();
    dlq_config.max_message_size = 10 * 1024;
    fixture
        .jetstream
        .update_stream(dlq_config)
        .await
        .expect("DLQ admits newly published source records after the limit reduction");
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 8 * 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("current stream limits satisfy admission");
    let mut handle = messaging
        .consumer(registry(&fixture))
        .await
        .expect("current stream limits satisfy consumer admission")
        .start(&cancel);

    let failure = timeout(Duration::from_secs(5), handle.failed())
        .await
        .expect("a conclusively oversized DLQ transfer must surface a terminal failure");
    assert!(matches!(
        failure,
        ConsumerError::DeadLetterTooLarge { sequence: 1 }
    ));
    assert!(matches!(
        handle.finish(deadline()).await,
        Err(ConsumerError::DeadLetterTooLarge { sequence: 1 })
    ));
    wait_for_source_unacked(&fixture).await;
    assert_eq!(
        fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .expect("fixture DLQ remains available")
            .cached_info()
            .state
            .messages,
        0,
    );
    close(messaging).await;
    fixture.cleanup().await;
}
