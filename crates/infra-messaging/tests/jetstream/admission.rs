use super::*;

fn admission_refusals(logged: &Logged) -> Vec<(String, String, Option<String>)> {
    logged
        .snapshot()
        .into_iter()
        .filter(|event| event.message() == Some("messaging_admission_failed"))
        .map(|event| {
            (
                event
                    .unquoted("error.type")
                    .expect("refusal names its error type")
                    .to_owned(),
                event
                    .unquoted("required_bytes")
                    .expect("refusal names the delivery size")
                    .to_owned(),
                event.unquoted("limit_bytes").map(str::to_owned),
            )
        })
        .collect()
}

#[tokio::test]
async fn admission_requires_streams_that_accept_and_retain_acknowledged_publications() {
    type ConfigureStream = fn(&mut stream::Config);
    let cases: [(&str, ConfigureStream); 5] = [
        ("stream_no_ack", |config| config.no_ack = true),
        ("stream_memory_storage", |config| {
            config.storage = stream::StorageType::Memory;
        }),
        ("stream_async_persistence", |config| {
            config.persist_mode = Some(stream::PersistenceMode::Async);
        }),
        ("stream_interest_retention", |config| {
            config.retention = stream::RetentionPolicy::Interest;
        }),
        ("stream_sealed", |config| config.sealed = true),
    ];
    for (error_type, configure) in cases {
        for (consumer_mode, dead_letter) in [(false, false), (true, false), (true, true)] {
            let fixture = Fixture::create(Topology::StreamsOnly).await;
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
            configure(&mut config);
            if dead_letter && config.retention == stream::RetentionPolicy::Interest {
                let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
                let mut source_config = source.cached_info().config.clone();
                source_config.retention = stream::RetentionPolicy::WorkQueue;
                fixture.replace_stream(source_config).await;
            }
            if config.sealed {
                fixture.jetstream.update_stream(config).await.unwrap();
            } else {
                fixture.replace_stream(config).await;
            }
            let (_guard, logged) = capture_logs();
            let refused = Messaging::connect(
                options(
                    &fixture,
                    consumer_mode.then(|| consumer_options(&fixture)),
                    1024,
                ),
                &OperationContext::with_timeout(Duration::from_secs(5)),
            )
            .await
            .expect_err("the configured publishing role cannot accept or retain messages");
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
            assert!(
                logged
                    .snapshot()
                    .iter()
                    .any(|event| event.unquoted("operation") == Some(role))
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
            fixture.cleanup().await;
        }
    }
}

// template:begin outbox:messaging-outbox-topology-tests
#[tokio::test]
async fn outbox_refuses_route_changing_subject_transforms() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let swapped = fixture
        .subject
        .replacen("test.jetstream", "jetstream.test", 1);
    let subject = fixture.subject.as_str();
    let literal_braces = format!("{subject}.{}", "{{}}");
    let function_token = format!("{subject}.{{{{random(1)}}}}");
    let evaluated = format!("{subject}.0");
    for (id, published_subject, transform, stored_subject) in [
        ("none", subject, None, subject),
        ("literal", subject, Some((subject, subject)), subject),
        (
            "literal-braces",
            literal_braces.as_str(),
            Some((literal_braces.as_str(), literal_braces.as_str())),
            literal_braces.as_str(),
        ),
        (
            "captures",
            subject,
            Some(("*.*.>", "$+01.{{ Wildcard ( 2 ) }}.>")),
            subject,
        ),
        (
            "swap",
            subject,
            Some(("*.*.>", "$2.$1.>")),
            swapped.as_str(),
        ),
        (
            "function",
            function_token.as_str(),
            Some((function_token.as_str(), function_token.as_str())),
            evaluated.as_str(),
        ),
    ] {
        let mut config = source.cached_info().config.clone();
        config.subjects = vec![published_subject.to_owned()];
        config.subject_transform = transform.map(|(src, dest)| stream::SubjectTransform {
            source: src.to_owned(),
            destination: dest.to_owned(),
        });
        fixture.jetstream.update_stream(config).await.unwrap();
        let cancel = CancellationToken::new();
        let messaging =
            Messaging::connect(options(&fixture, None, 1024), &context(deadline(), &cancel))
                .await
                .expect("ordinary publication accepts a stream transform");
        let event = Registry::default()
            .declare_publication(&Route::<ExampleEvent>::new(published_subject), 1024)
            .unwrap()
            .prepare(&event_with_value(id, "route"))
            .unwrap();
        let ack = messaging
            .producer()
            .publish(&context(deadline(), &cancel), &event)
            .await
            .unwrap();
        let stored = source.get_raw_message(ack.sequence).await.unwrap();
        assert_eq!(stored.subject.as_str(), stored_subject);
        if stored_subject == published_subject {
            messaging
                .outbox_producer()
                .expect("transform preserves the route");
        } else {
            std::assert_matches!(
                messaging.outbox_producer(),
                Err(infra_messaging::MessagingError::OutboxSource(
                    "subject transform must preserve the publication subject"
                ))
            );
        }
        close(messaging).await;
    }
    fixture.cleanup().await;
}

#[tokio::test]
async fn outbox_refuses_interest_retention_when_puback_does_not_imply_storage() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let mut source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    let mut config = source.cached_info().config.clone();
    config.retention = stream::RetentionPolicy::Interest;
    fixture.replace_stream(config).await;
    fixture
        .jetstream
        .publish(fixture.subject.clone(), "no-interest".into())
        .await
        .unwrap()
        .await
        .expect("JetStream acknowledges publication without a consumer");
    let state = &source.info().await.unwrap().state;
    assert_eq!(state.consumer_count, 0);
    assert_eq!(state.messages, 0);
    std::assert_matches!(
        Messaging::connect(
            options(&fixture, None, 1024),
            &OperationContext::with_timeout(Duration::from_secs(5)),
        )
        .await,
        Err(infra_messaging::MessagingError::Topology)
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn outbox_capacity_refuses_new_publication_and_preserves_unread_messages() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    for (max_messages, max_bytes, max_messages_per_subject, expected) in [
        (
            1,
            -1,
            -1,
            "bounded message or byte capacity requires DiscardNew",
        ),
        (
            -1,
            1400,
            -1,
            "bounded message or byte capacity requires DiscardNew",
        ),
        (
            -1,
            -1,
            1,
            "a per-subject message limit requires DiscardNewPerSubject",
        ),
    ] {
        let mut config = source.cached_info().config.clone();
        config.retention = stream::RetentionPolicy::WorkQueue;
        config.max_age = Duration::from_secs(3600);
        config.max_messages = max_messages;
        config.max_bytes = max_bytes;
        config.max_messages_per_subject = max_messages_per_subject;
        config.discard = if max_messages_per_subject > 0 {
            stream::DiscardPolicy::New
        } else {
            stream::DiscardPolicy::Old
        };
        fixture.replace_stream(config.clone()).await;
        let cancel = CancellationToken::new();
        let messaging =
            Messaging::connect(options(&fixture, None, 1024), &context(deadline(), &cancel))
                .await
                .expect("ordinary publisher admission is unchanged");
        std::assert_matches!(
            messaging.outbox_producer(),
            Err(infra_messaging::MessagingError::OutboxSource(reason)) if reason == expected
        );
        close(messaging).await;

        config.discard = stream::DiscardPolicy::New;
        config.discard_new_per_subject = max_messages_per_subject > 0;
        fixture.replace_stream(config).await;
        verify_full_stream(&fixture).await;
    }
    fixture.cleanup().await;
}

async fn verify_full_stream(fixture: &Fixture) {
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(options(fixture, None, 1024), &context(deadline(), &cancel))
        .await
        .expect("admit WorkQueue source");
    messaging
        .outbox_producer()
        .expect("bounded source safely refuses new messages");
    let producer = messaging.producer();
    let first = fixture
        .publication
        .prepare(&event_with_value("first", &"a".repeat(750)))
        .unwrap();
    let second = fixture
        .publication
        .prepare(&event_with_value("second", &"b".repeat(750)))
        .unwrap();
    let ack = producer
        .publish(&context(deadline(), &cancel), &first)
        .await
        .expect("the first event fits into an empty stream");
    assert_eq!(
        producer
            .publish(&context(deadline(), &cancel), &second)
            .await,
        Err(PublishError::Rejected(
            infra_messaging::PublishRejection::BrokerRefused
        ))
    );

    let source = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
    assert_eq!(source.cached_info().state.messages, 1);
    let durable = source
        .create_consumer(consumer::pull::Config {
            durable_name: Some(fixture.durable.clone()),
            ..Default::default()
        })
        .await
        .expect("first consumer can be added after the publish");
    let mut deliveries = durable
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(3))
        .messages()
        .await
        .unwrap();
    let retained = deliveries.next().await.unwrap().unwrap();
    assert_eq!(&retained.payload, first.payload());
    assert_eq!(retained.info().unwrap().stream_sequence, ack.sequence);
    retained.double_ack().await.unwrap();
    eventually(
        Duration::from_secs(3),
        "acknowledged event frees WorkQueue capacity",
        async || {
            let info = fixture.jetstream.get_stream(&fixture.stream).await.unwrap();
            (info.cached_info().state.messages == 0).then_some(())
        },
    )
    .await;
    let stored = producer
        .publish(&context(deadline(), &cancel), &second)
        .await
        .expect("the same rejected event fits after old bytes are acknowledged");
    assert!(!stored.duplicate);
    assert_eq!(
        &source
            .get_raw_message(stored.sequence)
            .await
            .unwrap()
            .payload,
        second.payload()
    );
    close(messaging).await;
}
// template:end outbox:messaging-outbox-topology-tests

#[tokio::test]
async fn an_explicit_dead_letter_stream_must_receive_the_dead_letter_subject() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let mut consumer = consumer_options(&fixture);
    consumer.dlq_subject = fixture.subject.clone();
    let (_guard, logged) = capture_logs();
    std::assert_matches!(
        Messaging::connect(
            options(&fixture, Some(consumer), 1024),
            &OperationContext::with_timeout(Duration::from_secs(5)),
        )
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
    let fixture = Fixture::create(Topology::StreamsOnly).await;
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
    let messaging = Messaging::connect(
        options(&fixture, None, 1024),
        &OperationContext::with_timeout(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    messaging
        .producer()
        .publish(
            &context(deadline(), &CancellationToken::new()),
            &fixture
                .publication
                .prepare(&event("publisher-no-dlq"))
                .unwrap(),
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

fn encoded_headers(headers: &async_nats::HeaderMap) -> usize {
    let mut encoded = String::from("NATS/1.0\r\n");
    #[expect(
        clippy::iter_over_hash_type,
        reason = "order-independent total encoded header byte length"
    )]
    for (name, values) in headers.iter() {
        for value in values {
            use std::fmt::Write as _;
            write!(encoded, "{name}: {value}\r\n").unwrap();
        }
    }
    encoded.push_str("\r\n");
    encoded.len()
}

fn transfer_additions(subject: &str, stream: &str) -> usize {
    let id = "x".repeat(68);
    format!("Message-Id: {id}\r\nNats-Msg-Id: {id}\r\nOriginal-Subject: {subject}\r\nDead-Letter-Reason: permanent\r\nNats-Expected-Stream: {stream}\r\n").len()
}

const TRANSFER_FRAMING_RESERVE_BYTES: usize = 16;

#[tokio::test]
async fn consumer_admits_the_transfer_bound_and_refuses_one_byte_less_before_durable_creation() {
    for short in [true, false] {
        let fixture = Fixture::create(Topology::StreamsOnly).await;
        let required = 1024
            + 8192
            + TRANSFER_FRAMING_RESERVE_BYTES
            + transfer_additions(&fixture.subject, &fixture.dlq_stream);
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
        let messaging = Messaging::connect(
            options(&fixture, Some(consumer_options(&fixture)), 1024),
            &context(deadline(), &cancel),
        )
        .await
        .unwrap();
        let mut registered = fixture.registry.clone();
        registered
            .register(
                &fixture.route,
                |_: Event<ExampleEvent>, _: OperationContext| async {
                    Err(HandlerError::Permanent)
                },
            )
            .unwrap();
        let (_guard, logged) = capture_logs();
        let consumer = messaging
            .consumer(registered, &context(deadline(), &cancel))
            .await;
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
            let prepared = fixture
                .publication
                .prepare(&event_with_value("full-transfer", &"x".repeat(1012)))
                .unwrap();
            assert_eq!(prepared.payload().len(), 1024);
            let mut headers = infra_messaging::wire::encode_prepared(&prepared);
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
    let exporter = install_tracing();
    let fixture = Fixture::create(Topology::StreamsOnly).await;
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

    let route = Route::<ExampleEvent>::new(registered_subject);
    let mut registered = Registry::default();
    registered
        .declare(&route)
        .expect("the handled subject belongs to the wildcard filter")
        .handle(accept);
    let mut consumer = consumer_options(&fixture);
    consumer.filter_subject.clone_from(&filter);
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(
        options(&fixture, Some(consumer), 1024),
        &context(deadline(), &cancel),
    )
    .await
    .unwrap();
    let mut handle = messaging
        .consumer(registered, &context(deadline(), &cancel))
        .await
        .unwrap()
        .start(&cancel);

    let prepared = Registry::default()
        .declare_publication(&Route::new(subject.clone()), 1024)
        .unwrap()
        .prepare(&event_with_value("wildcard-transfer", &"x".repeat(1012)))
        .unwrap();
    assert_eq!(prepared.payload().len(), 1024);
    let mut headers = infra_messaging::wire::encode_prepared(&prepared);
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

    let spans = exported_spans(exporter, &format!("process {filter}"));
    let [process] = spans.as_slice() else {
        panic!("expected one unhandled delivery: {spans:?}");
    };
    assert_eq!(attribute(process, "outcome").as_deref(), Some("unhandled"));
    assert!(
        process
            .events
            .iter()
            .any(|event| event.name == "messaging_delivery_failed")
    );
    let ack = exported_spans(exporter, "ack")
        .into_iter()
        .find(|span| span.parent_span_id == process.span_context.span_id())
        .expect("dead-letter transfer acknowledges the source");
    for span in [process, &ack] {
        assert_eq!(
            attribute(span, "messaging.destination.template").as_deref(),
            Some(filter.as_str())
        );
        assert!(
            !format!("{span:?}").contains(subject.as_str()),
            "unregistered subject reached telemetry"
        );
    }
    close(messaging).await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn unlimited_dlq_cannot_exceed_the_advertised_server_transfer_limit() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
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
    let messaging = Messaging::connect(
        options(
            &fixture,
            Some(consumer_options(&fixture)),
            server_limit - 8192,
        ),
        &OperationContext::with_timeout(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let mut registered = fixture.registry.clone();
    registered.register(&fixture.route, accept).unwrap();
    let (_guard, logged) = capture_logs();
    std::assert_matches!(
        messaging
            .consumer(
                registered,
                &OperationContext::with_timeout(Duration::from_secs(5)),
            )
            .await,
        Err(infra_messaging::MessagingError::Bounds)
    );
    assert_eq!(
        admission_refusals(&logged),
        [(
            "server_max_payload".to_owned(),
            (server_limit
                + TRANSFER_FRAMING_RESERVE_BYTES
                + transfer_additions(&fixture.subject, &fixture.dlq_stream))
            .to_string(),
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
        let fixture = Fixture::create(Topology::StreamsOnly).await;
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
        let route = Route::<ExampleEvent>::new(handled_subject);
        let mut registered = Registry::default();
        registered.declare(&route).unwrap().handle(accept);
        registered
            .declare(&Route::<UnhandledEvent>::new(subject))
            .unwrap();
        let messaging = Messaging::connect(
            options,
            &OperationContext::with_timeout(Duration::from_secs(5)),
        )
        .await
        .unwrap();
        let (_guard, logged) = capture_logs();
        let admitted = messaging
            .consumer(
                registered,
                &OperationContext::with_timeout(Duration::from_secs(5)),
            )
            .await;
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
            let _ = admitted.unwrap();
        }
        close(messaging).await;
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn admission_names_the_broker_limit_that_cannot_carry_one_delivery() {
    let fixture = Fixture::create(Topology::StreamsOnly).await;
    let cancel = CancellationToken::new();
    let server_max_payload = async_nats::connect(nats_url())
        .await
        .expect("fixture broker is reachable")
        .server_info()
        .max_payload;
    let (_guard, logged) = capture_logs();

    let refused = Messaging::connect(
        options(&fixture, None, server_max_payload),
        &context(deadline(), &cancel),
    )
    .await
    .expect_err("a delivery the server cannot carry is refused");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Bounds,
        "{refused:?}"
    );

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
    let refused = Messaging::connect(without_limit, &context(deadline(), &cancel))
        .await
        .expect_err("a source stream without a message limit is refused");
    std::assert_matches!(
        refused,
        infra_messaging::MessagingError::Bounds,
        "{refused:?}"
    );

    let refused = Messaging::connect(
        options(&fixture, Some(consumer_options(&fixture)), 512),
        &context(deadline(), &cancel),
    )
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
