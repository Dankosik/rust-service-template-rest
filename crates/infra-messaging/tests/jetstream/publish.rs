//! Publication acknowledgement, refusal, and ambiguity.

use super::*;

#[tokio::test]
async fn publication_requires_expected_stream_ack_deduplicates_and_rejects_definite_refusal() {
    let fixture = Fixture::create_with_source_limit(false, 1).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let original = event("event-publication");
    let prepared = registry(&fixture)
        .prepare(&original, 1024)
        .expect("registered event is prepared once");

    let first = messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("first publication receives a JetStream acknowledgment");
    let duplicate = messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("retrying the same immutable publication identity is acknowledged");

    assert_eq!(first.stream, fixture.stream);
    assert!(!first.duplicate);
    assert_eq!(duplicate.stream, fixture.stream);
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.sequence, first.sequence);

    let rejected = registry(&fixture)
        .prepare(&event("event-refused"), 1024)
        .expect("second registered event is prepared");
    let result = messaging
        .producer()
        .publish(&rejected, &context(deadline(), &cancel))
        .await;
    assert!(matches!(result, Err(PublishError::Rejected)));

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn a_prepared_event_obeys_the_receiving_resources_payload_limit() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let prepared = registry(&fixture)
        .prepare(&event("event-receiving-limit"), 1024)
        .expect("event fits the preparing resource");
    let receiving_limit = prepared.payload().len() - 1;
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, receiving_limit),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("smaller receiving resource is admitted");
    assert!(matches!(
        messaging
            .producer()
            .publish(&prepared, &context(deadline(), &cancel))
            .await,
        Err(PublishError::Rejected)
    ));
    let mut source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    assert_eq!(source.info().await.unwrap().state.messages, 0);
    close(messaging).await;

    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, receiving_limit + 1),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("the exact payload boundary is admitted");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_publications_hold_the_shared_window_until_native_cleanup() {
    let fixture = Fixture::create(false).await;
    let client = async_nats::connect(nats_url()).await.unwrap();
    let max_payload_bytes = client.server_info().max_payload.min(64 * 1024 * 1024) - 8192;
    client.drain().await.unwrap();
    let capacity = (64 * 1024 * 1024) / (max_payload_bytes + 8192);
    let observed = Arc::new(AtomicUsize::new(0));
    let dispatched = Arc::new(Notify::new());
    let (relay_observed, relay_dispatched) = (observed.clone(), dispatched.clone());
    let stream = fixture.stream.clone();
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        if is_stream_publish_ack(payload, &stream)
            && relay_observed.load(Ordering::SeqCst) < capacity
        {
            if relay_observed.fetch_add(1, Ordering::SeqCst) + 1 == capacity {
                relay_dispatched.notify_one();
            }
            true
        } else {
            false
        }
    })
    .await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(&fixture, vec![relay.url.clone()], None, max_payload_bytes),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let prepared = registry(&fixture)
        .prepare(&event("event-window"), 1024)
        .unwrap();
    let mut publications = tokio::task::JoinSet::new();
    for _ in 0..capacity {
        let (producer, prepared, cancel) = (messaging.producer(), prepared.clone(), cancel.clone());
        publications.spawn(async move {
            producer
                .publish(&prepared, &context(deadline(), &cancel))
                .await
        });
    }
    timeout(Duration::from_secs(3), dispatched.notified())
        .await
        .expect("every admitted publication reaches the broker");
    cancel.cancel();
    while let Some(result) = publications.join_next().await {
        assert!(matches!(result.unwrap(), Err(PublishError::Ambiguous)));
    }
    let producer = messaging.producer();
    let healthy = CancellationToken::new();
    let refused = timeout(
        Duration::from_millis(250),
        producer.publish(&prepared, &context(deadline(), &healthy)),
    )
    .await
    .expect("a full shared window refuses without waiting");
    assert!(matches!(refused, Err(PublishError::Rejected)));
    assert_eq!(observed.load(Ordering::SeqCst), capacity);

    timeout(Duration::from_secs(7), async {
        let mut cadence = tokio::time::interval(Duration::from_millis(20));
        loop {
            cadence.tick().await;
            match producer
                .publish(&prepared, &context(deadline(), &healthy))
                .await
            {
                Ok(ack) => {
                    assert!(ack.duplicate);
                    break;
                }
                Err(PublishError::Rejected) => {}
                Err(PublishError::Ambiguous) => panic!("restored ACK path must confirm"),
            }
        }
    })
    .await
    .expect("native cleanup restores publication without replacing the resource");
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn post_dispatch_lost_ack_is_ambiguous_and_same_identity_retries_as_duplicate() {
    let fixture = Fixture::create(false).await;
    let relay = AckDroppingRelay::start(&fixture.stream).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(&fixture, vec![relay.url.clone()], None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted through the transparent relay");
    let prepared = registry(&fixture)
        .prepare(&event("event-ambiguous"), 1024)
        .expect("fixture event is prepared once before its ambiguous publication");

    let result = messaging
        .producer()
        .publish(
            &prepared,
            &context(Instant::now() + Duration::from_millis(250), &cancel),
        )
        .await;
    assert!(matches!(result, Err(PublishError::Ambiguous)));

    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source stream remains observable after the dropped response");
    let source_info = source
        .info()
        .await
        .expect("fixture source message count is observable after dispatch");
    assert_eq!(source_info.state.messages, 1);
    let stored_sequence = source_info.state.last_sequence;
    let stored = source
        .get_raw_message(stored_sequence)
        .await
        .expect("broker retains the dispatched message despite its lost response");
    assert_eq!(
        stored.headers.get("Nats-Msg-Id").map(ToString::to_string),
        Some(prepared.publication_id().to_owned())
    );

    close(messaging).await;
    relay.join().await;

    let retry = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("direct retry producer is admitted after relay shutdown");
    let acknowledgment = retry
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("retrying the same prepared identity receives a broker acknowledgment");
    assert!(acknowledgment.duplicate);
    assert_eq!(acknowledgment.sequence, stored_sequence);
    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .expect("fixture source stream remains observable after duplicate retry");
    assert_eq!(
        source
            .info()
            .await
            .expect("fixture source count is observable after duplicate retry")
            .state
            .messages,
        1
    );

    close(retry).await;
    fixture.cleanup().await;
}
