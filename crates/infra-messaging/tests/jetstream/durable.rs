use super::*;

#[tokio::test]
async fn concurrent_replicas_admit_the_same_new_durable() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let mut registry = fixture.registry.clone();
    registry
        .register(&fixture.route, |_, _| async { Ok(()) })
        .expect("fixture handler is registered");

    let (first, second) = tokio::join!(
        messaging.consumer(registry.clone(), deadline(), &cancel),
        messaging.consumer(registry, deadline(), &cancel),
    );
    let mut handle = first
        .expect("the first replica admits the durable")
        .start(&cancel);
    second.expect("the second replica admits the same durable");
    let actual = durable(&fixture).await.cached_info().clone();
    assert_eq!(actual.config.filter_subject, fixture.subject);
    assert_eq!(actual.config.max_deliver, -1);
    assert_eq!(actual.config.ack_policy, consumer::AckPolicy::Explicit);
    assert_eq!(actual.config.ack_wait, Duration::from_secs(46));
    assert!(
        actual.config.max_ack_pending > 1,
        "the durable keeps the broker default instead of one replica's concurrency"
    );
    handle
        .finish(deadline())
        .await
        .expect("empty durable consumer drains within the shared deadline");
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn replicas_with_different_filters_cannot_both_claim_a_new_durable() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let other_subject = format!("{}.other", fixture.subject);
    let stream = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let mut config = stream.cached_info().config.clone();
    config.subjects.push(other_subject.clone());
    fixture.jetstream.update_stream(config).await.unwrap();
    let mut other_options = consumer_options(&fixture);
    other_options.filter_subject.clone_from(&other_subject);
    let (first, second) = tokio::try_join!(
        Box::pin(Messaging::connect(
            options(&fixture, Some(consumer_options(&fixture)), 1024),
            deadline(),
            cancel.clone(),
        )),
        Box::pin(Messaging::connect(
            options(&fixture, Some(other_options), 1024),
            deadline(),
            cancel.clone(),
        )),
    )
    .unwrap();
    let mut registered = fixture.registry.clone();
    registered
        .register(&fixture.route, |_, _| async { Ok(()) })
        .unwrap();
    let mut other_registry = Registry::default();
    let route = Route::<ExampleEvent>::new(other_subject.clone());
    other_registry.declare(&route).unwrap();
    other_registry
        .register(&route, |_, _| async { Ok(()) })
        .unwrap();
    let (left, right) = tokio::join!(
        first.consumer(registered, deadline(), &cancel),
        second.consumer(other_registry, deadline(), &cancel),
    );
    let filter = match (left, right) {
        (Ok(_), Err(infra_messaging::MessagingError::Topology)) => fixture.subject.as_str(),
        (Err(infra_messaging::MessagingError::Topology), Ok(_)) => other_subject.as_str(),
        result => panic!("exactly one filter must own the durable: {result:?}"),
    };
    assert_eq!(
        durable(&fixture).await.cached_info().config.filter_subject,
        filter
    );
    close(first).await;
    close(second).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn incompatible_durable_stops_consumption_and_stays_unchanged_on_readmission() {
    for small_pull in [false, true] {
        let fixture = Fixture::create(true).await;
        let cancel = CancellationToken::new();
        let messaging = Box::pin(Messaging::connect(
            options(&fixture, Some(consumer_options(&fixture)), 1024),
            deadline(),
            cancel.clone(),
        ))
        .await
        .unwrap();
        let mut registered = fixture.registry.clone();
        registered
            .register(&fixture.route, |_, _| async { Ok(()) })
            .unwrap();
        let before = durable(&fixture).await.cached_info().clone();
        let admitted = messaging
            .consumer(registered.clone(), deadline(), &cancel)
            .await
            .unwrap();
        let after = durable(&fixture).await.cached_info().clone();
        assert_eq!(after.config, before.config);
        assert_eq!(after.created, before.created);

        let stream = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
        let mut config = after.config;
        if small_pull {
            config.max_bytes = 1024;
        } else {
            config.max_deliver = 3;
        }
        let changed = stream.update_consumer(config).await.unwrap();
        std::assert_matches!(
            messaging.consumer(registered, deadline(), &cancel).await,
            Err(infra_messaging::MessagingError::Topology)
        );
        let actual = durable(&fixture).await.cached_info().clone();
        assert_eq!(actual.config, changed.cached_info().config);
        assert_eq!(actual.created, before.created);
        let mut handle = admitted.start(&cancel);
        std::assert_matches!(
            timeout(Duration::from_secs(5), handle.failed())
                .await
                .unwrap(),
            ConsumerError::ConsumerLost
        );
        std::assert_matches!(
            handle.finish(deadline()).await,
            Err(ConsumerError::ConsumerLost)
        );
        close(messaging).await;
        fixture.cleanup().await;
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
struct PublishedElsewhere;

impl EventPayload for PublishedElsewhere {
    const EVENT_TYPE: &'static str = "test.example.published_elsewhere";
    const SCHEMA_VERSION: u16 = 1;
}

#[tokio::test]
async fn consumer_admission_checks_the_filter_for_handled_subjects_only() {
    let fixture = Fixture::create(true).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let elsewhere = Route::<PublishedElsewhere>::new("elsewhere.published");
    let mut routes = fixture.registry.clone();
    routes
        .declare(&elsewhere)
        .expect("fixture routes are valid");

    let mut publishes_elsewhere = routes.clone();
    publishes_elsewhere
        .register(&fixture.route, |_, _| async { Ok(()) })
        .expect("fixture handler is registered");
    messaging
        .consumer(publishes_elsewhere, deadline(), &cancel)
        .await
        .expect("a publish-only route outside the filter does not refuse the consumer");

    let mut handles_elsewhere = routes;
    handles_elsewhere
        .register(&elsewhere, |_, _| async { Ok(()) })
        .expect("fixture handler is registered");
    let refused = messaging
        .consumer(handles_elsewhere, deadline(), &cancel)
        .await
        .expect_err("a handler the filter can never reach refuses the consumer");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Configuration(_),
        "{refused:?}"
    );

    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn durable_deleted_between_pulls_stops_the_consumer() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let mut registry = fixture.registry.clone();
    registry
        .register(&fixture.route, |_, _| async { Ok(()) })
        .expect("fixture handler is registered");
    let consumer = messaging
        .consumer(registry, deadline(), &cancel)
        .await
        .expect("adapter creates its named durable consumer");
    fixture
        .jetstream
        .delete_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .expect("test administrator deletes the durable consumer");

    let handle = consumer.start(&cancel);
    let failure = timeout(Duration::from_secs(10), handle.failed())
        .await
        .expect("a durable the broker reports missing must stop the consumer");
    std::assert_matches!(failure, ConsumerError::ConsumerLost);

    drop(handle);
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn deleting_a_durable_with_a_waiting_batch_stops_the_consumer() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("source stream is admitted");
    let mut registered = fixture.registry.clone();
    registered
        .register(&fixture.route, |_, _| async { Ok(()) })
        .unwrap();
    let mut handle = messaging
        .consumer(registered, deadline(), &cancel)
        .await
        .unwrap()
        .start(&cancel);
    eventually(
        Duration::from_secs(3),
        "the broker observes a waiting batch",
        async || (durable(&fixture).await.cached_info().num_waiting > 0).then_some(()),
    )
    .await;
    fixture
        .jetstream
        .delete_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .unwrap();
    std::assert_matches!(
        timeout(Duration::from_secs(10), handle.failed())
            .await
            .unwrap(),
        ConsumerError::ConsumerLost
    );
    std::assert_matches!(
        handle.finish(deadline()).await,
        Err(ConsumerError::ConsumerLost)
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn a_durable_replaced_before_the_next_pull_arrives_cannot_dispatch() {
    let fixture = Fixture::create(false).await;
    let pull_subject = format!(
        "$JS.API.CONSUMER.MSG.NEXT.{}.{}",
        fixture.stream, fixture.durable
    );
    let (relay, pull) = AckDroppingRelay::intercept_second_pull(pull_subject.clone()).await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(
            &fixture,
            vec![relay.url.clone()],
            Some(consumer_options(&fixture)),
            1024,
        ),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    for id in ["before-replacement", "pending-after-replacement"] {
        let prepared = fixture.prepare(&event(id), 1024).unwrap();
        messaging
            .producer()
            .publish(&prepared, &context(deadline(), &cancel))
            .await
            .unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registered = fixture.registry.clone();
    registered
        .register(&fixture.route, {
            let calls = Arc::clone(&calls);
            move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok(()) }
            }
        })
        .unwrap();
    let mut handle = messaging
        .consumer(registered, deadline(), &cancel)
        .await
        .unwrap()
        .start(&cancel);
    let (reply, payload) = timeout(Duration::from_secs(5), pull)
        .await
        .expect("the second pull passes the old durable's identity check")
        .unwrap();
    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    fixture
        .jetstream
        .delete_consumer_from_stream(&fixture.durable, &fixture.stream)
        .await
        .unwrap();
    source
        .create_consumer(consumer::pull::Config {
            durable_name: Some(fixture.durable.clone()),
            filter_subject: fixture.subject.clone(),
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(46),
            max_deliver: -1,
            ..Default::default()
        })
        .await
        .unwrap();
    fixture
        .jetstream
        .client()
        .publish_with_reply(pull_subject, reply, payload)
        .await
        .expect("the replacement receives the old consumer's pull");
    std::assert_matches!(
        timeout(Duration::from_secs(10), handle.failed())
            .await
            .expect("a successful previous batch cannot hide replacement"),
        ConsumerError::ConsumerLost
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    std::assert_matches!(
        handle.finish(deadline()).await,
        Err(ConsumerError::ConsumerLost)
    );
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn an_unanswered_identity_check_blocks_pulls_without_losing_the_consumer() {
    let fixture = Fixture::create(false).await;
    let refuse_info = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drop_info = Arc::clone(&refuse_info);
    let stream = fixture.stream.clone();
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        drop_info.load(Ordering::SeqCst)
            && serde_json::from_slice::<serde_json::Value>(payload)
                .is_ok_and(|info| info.get("ack_floor").is_some() && info["stream_name"] == stream)
    })
    .await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(
            &fixture,
            vec![relay.url.clone()],
            Some(consumer_options(&fixture)),
            1024,
        ),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut registered = fixture.registry.clone();
    registered
        .register(&fixture.route, move |event, _| {
            observed_tx.send(event.id).unwrap();
            async { Ok(()) }
        })
        .unwrap();
    let consumer = messaging
        .consumer(registered, deadline(), &cancel)
        .await
        .unwrap();
    let prepared = fixture
        .prepare(&event("waiting-for-identity"), 1024)
        .unwrap();
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .unwrap();
    refuse_info.store(true, Ordering::SeqCst);
    let mut handle = consumer.start(&cancel);
    timeout(Duration::from_secs(3), relay.dropped_ack.cancelled())
        .await
        .expect("the broker answered the identity query but its reply was withheld");
    assert!(
        messaging.probe().check().await.is_ok(),
        "the NATS connection itself is healthy"
    );
    assert!(
        timeout(Duration::from_secs(7), observed_rx.recv())
            .await
            .is_err()
    );
    let durable = durable(&fixture).await;
    assert_eq!(durable.cached_info().num_ack_pending, 0);
    assert_eq!(durable.cached_info().num_pending, 1);
    refuse_info.store(false, Ordering::SeqCst);
    assert_eq!(
        timeout(Duration::from_secs(15), observed_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        "waiting-for-identity"
    );
    wait_for_source_ack(&fixture).await;
    handle.finish(deadline()).await.unwrap();
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn drain_interrupts_identity_waits_and_joins_any_admitted_delivery() {
    for admit_delivery in [false, true] {
        let fixture = Fixture::create(false).await;
        let refuse_info = Arc::new(AtomicBool::new(false));
        let drop_info = Arc::clone(&refuse_info);
        let stream = fixture.stream.clone();
        let relay = AckDroppingRelay::start_filtering(move |payload| {
            drop_info.load(Ordering::SeqCst)
                && serde_json::from_slice::<serde_json::Value>(payload).is_ok_and(|info| {
                    info.get("ack_floor").is_some() && info["stream_name"] == stream
                })
        })
        .await;
        let cancel = CancellationToken::new();
        let messaging = Box::pin(Messaging::connect(
            options_with_servers(
                &fixture,
                vec![relay.url.clone()],
                Some(ConsumerOptions {
                    concurrency: const { std::num::NonZeroUsize::new(2).expect("2 is nonzero") },
                    ..consumer_options(&fixture)
                }),
                1024,
            ),
            deadline(),
            cancel.clone(),
        ))
        .await
        .unwrap();
        let started = [CancellationToken::new(), CancellationToken::new()];
        let settle = [CancellationToken::new(), CancellationToken::new()];
        let mut registered = fixture.registry.clone();
        registered
            .register(&fixture.route, {
                let started = started.clone();
                let settle = settle.clone();
                move |event, _| {
                    let slot = usize::from(event.id == "drain-release-slot");
                    started[slot].cancel();
                    let settle = settle[slot].clone();
                    async move {
                        settle.cancelled().await;
                        Ok(())
                    }
                }
            })
            .unwrap();
        let consumer = messaging
            .consumer(registered, deadline(), &cancel)
            .await
            .unwrap();
        let events = ["drain-with-info-withheld", "drain-release-slot"];
        for id in &events[..=usize::from(admit_delivery)] {
            let prepared = fixture.prepare(&event(id), 1024).unwrap();
            messaging
                .producer()
                .publish(&prepared, &context(deadline(), &cancel))
                .await
                .unwrap();
        }
        refuse_info.store(!admit_delivery, Ordering::SeqCst);
        let mut handle = consumer.start(&cancel);
        if admit_delivery {
            for started in &started {
                timeout(Duration::from_secs(3), started.cancelled())
                    .await
                    .expect("both deliveries fill the pull before its next identity check");
            }
            refuse_info.store(true, Ordering::SeqCst);
            settle[1].cancel();
        }
        relay.wait_for_drop().await;
        let mut finishing = Box::pin(handle.finish(Instant::now() + Duration::from_secs(1)));
        if admit_delivery {
            assert!(
                futures_util::poll!(&mut finishing).is_pending(),
                "drain must retain ownership of the admitted handler"
            );
            settle[0].cancel();
        }
        finishing
            .await
            .expect("an unanswered INFO must not spend the admitted delivery's drain window");
        for started in &started {
            assert_eq!(started.is_cancelled(), admit_delivery);
        }
        let durable = durable(&fixture).await;
        assert_eq!(durable.cached_info().num_ack_pending, 0);
        assert_eq!(
            durable.cached_info().num_pending,
            u64::from(!admit_delivery)
        );
        assert!(messaging.probe().check().await.is_ok());
        close(messaging).await;
        relay.join().await;
        fixture.cleanup().await;
    }
}
