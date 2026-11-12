//! Broker and stream limits refused at admission.

use super::*;

/// The `messaging_admission_failed` events as (`error.type`, `required_bytes`, `limit_bytes`).
fn admission_refusals(logged: &Logged) -> Vec<(String, String, Option<String>)> {
    let field = |event: &[(&'static str, String)], name: &str| {
        event
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.clone())
    };
    logged
        .lock()
        .expect("log lock")
        .iter()
        .filter(|event| field(event, "message").as_deref() == Some("messaging_admission_failed"))
        .map(|event| {
            (
                field(event, "error.type").expect("refusal names its error type"),
                field(event, "required_bytes").expect("refusal names the delivery size"),
                field(event, "limit_bytes"),
            )
        })
        .collect()
}

#[tokio::test]
async fn admission_refuses_unacknowledged_or_volatile_streams_for_the_publishing_role() {
    for (no_ack, storage, persist_mode, error_type) in [
        (true, stream::StorageType::File, None, "stream_no_ack"),
        (
            false,
            stream::StorageType::Memory,
            None,
            "stream_memory_storage",
        ),
        (
            false,
            stream::StorageType::File,
            Some(stream::PersistenceMode::Async),
            "stream_async_persistence",
        ),
    ] {
        for (consumer_mode, dead_letter) in [(false, false), (true, false), (true, true)] {
            let fixture = Fixture::create(false).await;
            let name = if dead_letter {
                &fixture.dlq_stream
            } else {
                &fixture.stream
            };
            let mut config = fixture
                .jetstream
                .get_stream(name)
                .await
                .unwrap()
                .cached_info()
                .config
                .clone();
            config.storage = storage;
            config.persist_mode = persist_mode;
            config.no_ack = no_ack;
            fixture.replace_stream(config).await;
            let (_guard, logged) = capture_logs();
            let refused = Box::pin(Messaging::connect(
                options(
                    &fixture,
                    consumer_mode.then(|| consumer_options(&fixture)),
                    1024,
                ),
                deadline(),
                CancellationToken::new(),
            ))
            .await
            .expect_err("each publishing role requires ACKs and durable storage");
            std::assert_matches!(refused, infra_messaging::MessagingError::Topology);
            assert_eq!(
                admission_refusals(&logged),
                [(error_type.to_owned(), (1024 + 8192).to_string(), None)]
            );
            let role = if dead_letter {
                "dead_letter_stream"
            } else {
                "source_stream"
            };
            assert!(logged.lock().unwrap().iter().any(|event| {
                event
                    .iter()
                    .any(|(name, value)| *name == "operation" && value == role)
            }));
            assert_eq!(
                fixture
                    .jetstream
                    .get_stream(&fixture.stream)
                    .await
                    .unwrap()
                    .cached_info()
                    .state
                    .consumer_count,
                0
            );
            fixture.cleanup().await;
        }
    }
}

#[tokio::test]
async fn an_explicit_dead_letter_stream_must_receive_the_dead_letter_subject() {
    let fixture = Fixture::create(false).await;
    let mut consumer = consumer_options(&fixture);
    consumer.dlq_subject = fixture.subject.clone();
    let (_guard, logged) = capture_logs();
    std::assert_matches!(
        Box::pin(Messaging::connect(
            options(&fixture, Some(consumer), 1024),
            deadline(),
            CancellationToken::new(),
        ))
        .await,
        Err(infra_messaging::MessagingError::Topology)
    );
    assert_eq!(
        admission_refusals(&logged),
        [(
            "dead_letter_subject_mismatch".to_owned(),
            "9216".to_owned(),
            None
        )]
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn publisher_admits_explicit_default_persistence_without_a_dead_letter_stream() {
    let fixture = Fixture::create(false).await;
    fixture
        .jetstream
        .delete_stream(&fixture.dlq_stream)
        .await
        .unwrap();
    let mut config = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .cached_info()
        .config
        .clone();
    config.persist_mode = Some(stream::PersistenceMode::Default);
    fixture.replace_stream(config).await;
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    messaging
        .producer()
        .publish(
            &registry(&fixture)
                .prepare(&event("publisher-no-dlq"), 1024)
                .unwrap(),
            &context(deadline(), &CancellationToken::new()),
        )
        .await
        .unwrap();
    close(messaging).await;
    fixture
        .jetstream
        .delete_stream(&fixture.stream)
        .await
        .unwrap();
}

/// Fixture byte count follows the NATS header framing, independently of adapter sizing.
fn encoded_headers(headers: &async_nats::HeaderMap) -> usize {
    let mut encoded = String::from("NATS/1.0\r\n");
    for (name, values) in headers.iter() {
        for value in values {
            use std::fmt::Write as _;
            write!(encoded, "{name}: {value}\r\n").unwrap();
        }
    }
    encoded.push_str("\r\n");
    encoded.len()
}

/// Independent native line encoding, including the replacement publication ID.
fn transfer_additions(subject: &str, stream: &str) -> usize {
    let id = "x".repeat(68);
    format!("Message-Id: {id}\r\nNats-Msg-Id: {id}\r\nOriginal-Subject: {subject}\r\nDead-Letter-Reason: permanent\r\nNats-Expected-Stream: {stream}\r\n").len()
}

#[tokio::test]
async fn consumer_admits_the_transfer_bound_and_refuses_one_byte_less_before_durable_creation() {
    for short in [true, false] {
        let fixture = Fixture::create(false).await;
        let required = 1024 + 8192 + transfer_additions(&fixture.subject, &fixture.dlq_stream);
        let mut config = fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        config.max_message_size = i32::try_from(required - usize::from(short)).unwrap();
        fixture.jetstream.update_stream(config).await.unwrap();
        let cancel = CancellationToken::new();
        let messaging = Box::pin(Messaging::connect(
            options(&fixture, Some(consumer_options(&fixture)), 1024),
            deadline(),
            cancel.clone(),
        ))
        .await
        .unwrap();
        let mut registered = registry(&fixture);
        registered
            .register::<ExampleEvent, _, _>(|_, _| async { Err(HandlerError::Permanent) })
            .unwrap();
        let (_guard, logged) = capture_logs();
        let consumer = messaging.consumer(registered).await;
        if short {
            std::assert_matches!(consumer, Err(infra_messaging::MessagingError::Bounds));
            assert_eq!(
                admission_refusals(&logged),
                [(
                    "stream_max_message_size".to_owned(),
                    required.to_string(),
                    Some((required - 1).to_string())
                )]
            );
            assert_eq!(
                fixture
                    .jetstream
                    .get_stream(&fixture.stream)
                    .await
                    .unwrap()
                    .cached_info()
                    .state
                    .consumer_count,
                0
            );
        } else {
            let mut handle = consumer.unwrap().start(&cancel);
            let prepared = registry(&fixture)
                .prepare(&event_with_value("full-transfer", &"x".repeat(1012)), 1024)
                .unwrap();
            assert_eq!(prepared.payload().len(), 1024);
            let mut headers = infra_messaging::wire::encode_prepared(&prepared).unwrap();
            headers.insert("tracestate", "");
            let trace = "x".repeat(8192 - encoded_headers(&headers));
            headers.insert("tracestate", trace.as_str());
            assert_eq!(encoded_headers(&headers), 8192);
            fixture
                .jetstream
                .publish_with_headers(fixture.subject.clone(), headers, prepared.payload().clone())
                .await
                .unwrap()
                .await
                .unwrap();
            let transferred = wait_for_dead_letter(&fixture).await;
            assert_dead_letter(&transferred, &fixture, "permanent", prepared.payload());
            assert_eq!(
                transferred.headers.get("tracestate").unwrap().as_str(),
                trace
            );
            wait_for_source_ack(&fixture).await;
            handle.finish(deadline()).await.unwrap();
        }
        close(messaging).await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn wildcard_consumer_budgets_unregistered_subjects_for_pull_and_transfer() {
    let fixture = Fixture::create(false).await;
    let filter = format!("{}.events.>", fixture.subject);
    let registered_subject = format!("{}.events.known", fixture.subject);
    let subject = format!("{}.events.{}", fixture.subject, "x".repeat(256));
    let mut source = fixture
        .jetstream
        .get_stream(&fixture.stream)
        .await
        .unwrap()
        .cached_info()
        .config
        .clone();
    source.subjects = vec![filter.clone()];
    fixture.jetstream.update_stream(source).await.unwrap();

    let mut registered = Registry::try_new([Route::new::<ExampleEvent>(registered_subject)])
        .expect("the handled subject belongs to the wildcard filter");
    registered
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .unwrap();
    let mut consumer = consumer_options(&fixture);
    consumer.filter_subject = filter;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer), 1024),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);

    let prepared = infra_messaging::PreparedEvent::prepare(
        subject.clone(),
        &event_with_value("wildcard-transfer", &"x".repeat(1012)),
        1024,
    )
    .unwrap();
    assert_eq!(prepared.payload().len(), 1024);
    let mut headers = infra_messaging::wire::encode_prepared(&prepared).unwrap();
    headers.insert("tracestate", "");
    let trace = "x".repeat(8192 - encoded_headers(&headers));
    headers.insert("tracestate", trace.as_str());
    fixture
        .jetstream
        .publish_with_headers(subject.clone(), headers, prepared.payload().clone())
        .await
        .unwrap()
        .await
        .unwrap();

    let transferred = wait_for_dead_letter(&fixture).await;
    assert_eq!(
        transferred
            .headers
            .get("Original-Subject")
            .unwrap()
            .as_str(),
        subject
    );
    assert_eq!(transferred.payload, *prepared.payload());
    wait_for_source_ack(&fixture).await;
    handle.finish(deadline()).await.unwrap();
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn unlimited_dlq_cannot_exceed_the_advertised_server_transfer_limit() {
    let fixture = Fixture::create(false).await;
    let server_limit = async_nats::connect(nats_url())
        .await
        .unwrap()
        .server_info()
        .max_payload;
    for name in [&fixture.stream, &fixture.dlq_stream] {
        let mut config = fixture
            .jetstream
            .get_stream(name)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        config.max_message_size = if name == &fixture.stream {
            i32::try_from(server_limit).unwrap()
        } else {
            -1
        };
        fixture.jetstream.update_stream(config).await.unwrap();
    }
    let messaging = Box::pin(Messaging::connect(
        options(
            &fixture,
            Some(consumer_options(&fixture)),
            server_limit - 8192,
        ),
        deadline(),
        CancellationToken::new(),
    ))
    .await
    .unwrap();
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
        .unwrap();
    let (_guard, logged) = capture_logs();
    std::assert_matches!(
        messaging.consumer(registered).await,
        Err(infra_messaging::MessagingError::Bounds)
    );
    assert_eq!(
        admission_refusals(&logged),
        [(
            "server_max_payload".to_owned(),
            (server_limit + transfer_additions(&fixture.subject, &fixture.dlq_stream)).to_string(),
            Some(server_limit.to_string())
        )]
    );
    assert_eq!(
        fixture
            .jetstream
            .get_stream(&fixture.stream)
            .await
            .unwrap()
            .cached_info()
            .state
            .consumer_count,
        0
    );
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn selected_routes_without_handlers_obey_the_native_header_ceiling() {
    for (selected, excess) in [(true, 0), (true, 1), (false, 1)] {
        let fixture = Fixture::create(false).await;
        let mut config = fixture
            .jetstream
            .get_stream(&fixture.dlq_stream)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        config.max_message_size = -1;
        fixture.jetstream.update_stream(config).await.unwrap();
        let mut options = options(&fixture, Some(consumer_options(&fixture)), 1024);
        options.consumer.as_mut().unwrap().filter_subject = format!("{}.events.>", fixture.subject);
        // Both the handled route and selected publish-only route lie below this
        // filter; no long subject enters a broker control line during admission.
        let mut source = fixture
            .jetstream
            .get_stream(&fixture.stream)
            .await
            .unwrap()
            .cached_info()
            .config
            .clone();
        source.subjects = vec![format!("{}.events.>", fixture.subject)];
        fixture.jetstream.update_stream(source).await.unwrap();
        let handled_subject = format!("{}.events.handled", fixture.subject);
        let prefix = if selected {
            format!("{}.events.", fixture.subject)
        } else {
            "elsewhere.".to_owned()
        };
        let subject_bytes = 65_535 + excess - 8192 - transfer_additions("", &fixture.dlq_stream);
        let subject = format!("{prefix}{}", "x".repeat(subject_bytes - prefix.len()));
        let mut registered = Registry::try_new([
            Route::new::<ExampleEvent>(handled_subject),
            Route::new::<UnhandledEvent>(subject),
        ])
        .unwrap();
        registered
            .register::<ExampleEvent, _, _>(|_, _| async { Ok(()) })
            .unwrap();
        let messaging = Box::pin(Messaging::connect(
            options,
            deadline(),
            CancellationToken::new(),
        ))
        .await
        .unwrap();
        let (_guard, logged) = capture_logs();
        let admitted = messaging.consumer(registered).await;
        if selected && excess > 0 {
            std::assert_matches!(admitted, Err(infra_messaging::MessagingError::Bounds));
            assert_eq!(
                admission_refusals(&logged),
                [(
                    "dead_letter_header_bytes".to_owned(),
                    "65536".to_owned(),
                    Some("65535".to_owned())
                )]
            );
            assert_eq!(
                fixture
                    .jetstream
                    .get_stream(&fixture.stream)
                    .await
                    .unwrap()
                    .cached_info()
                    .state
                    .consumer_count,
                0
            );
        } else {
            admitted.unwrap();
        }
        close(messaging).await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn admission_names_the_broker_limit_that_cannot_carry_one_delivery() {
    let fixture = Fixture::create(false).await;
    let cancel = CancellationToken::new();
    let server_max_payload = async_nats::connect(nats_url())
        .await
        .expect("fixture broker is reachable")
        .server_info()
        .max_payload;
    let (_guard, logged) = capture_logs();

    // One delivery is the payload limit plus 8 KiB of headers, and the
    // server bounds the two together.
    let refused = Box::pin(Messaging::connect(
        options(&fixture, None, server_max_payload),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect_err("a delivery the server cannot carry is refused");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Bounds,
        "{refused:?}"
    );

    // A consumer's source stream must bound its own messages.
    let unbounded = format!("{}_UNBOUNDED", fixture.stream);
    fixture
        .jetstream
        .create_stream(stream::Config {
            name: unbounded.clone(),
            subjects: vec![format!("{}.unbounded", fixture.subject)],
            ..Default::default()
        })
        .await
        .expect("test stream without a message limit is created");
    let mut without_limit = options(&fixture, Some(consumer_options(&fixture)), 1024);
    without_limit.source_stream.clone_from(&unbounded);
    let refused = Box::pin(Messaging::connect(
        without_limit,
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect_err("a source stream without a message limit is refused");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Bounds,
        "{refused:?}"
    );

    // A stream that admits more than one delivery is refused too.
    let refused = Box::pin(Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 512),
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect_err("a source stream that admits a larger message is refused");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Bounds,
        "{refused:?}"
    );

    assert_eq!(
        admission_refusals(&logged),
        [
            (
                "server_max_payload".to_owned(),
                (server_max_payload + 8 * 1024).to_string(),
                Some(server_max_payload.to_string()),
            ),
            (
                "stream_max_message_size_unset".to_owned(),
                (1024 + 8 * 1024).to_string(),
                Some("-1".to_owned()),
            ),
            (
                "stream_max_message_size".to_owned(),
                (512 + 8 * 1024).to_string(),
                Some((1024 + 8 * 1024).to_string()),
            ),
        ]
    );

    fixture
        .jetstream
        .delete_stream(&unbounded)
        .await
        .expect("test stream is removable");
    fixture.cleanup().await;
}
