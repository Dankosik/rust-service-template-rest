#![expect(
    clippy::expect_used,
    reason = "integration tests make failures and broker setup explicit"
)]

use std::time::Duration;

use async_nats::jetstream::{self, stream};
use domain_events::EventPayload;
use futures_util::StreamExt as _;
use infra_messaging::{ConsumerOptions, Messaging, MessagingOptions, Registry, Route};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct IdleEvent {
    value: String,
}

impl EventPayload for IdleEvent {
    const EVENT_TYPE: &'static str = "test.idle.created";
    const SCHEMA_VERSION: u16 = 1;
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

async fn provision(
    broker: async_nats::Client,
    stream_name: &str,
    subject: &str,
    dlq_stream: &str,
    dlq_subject: &str,
) -> jetstream::Context {
    let jetstream = jetstream::new(broker);
    jetstream
        .create_stream(stream::Config {
            name: stream_name.to_owned(),
            subjects: vec![subject.to_owned()],
            max_message_size: 1024 + 8 * 1024,
            ..Default::default()
        })
        .await
        .expect("idle fixture stream is created");
    jetstream
        .create_stream(stream::Config {
            name: dlq_stream.to_owned(),
            subjects: vec![dlq_subject.to_owned()],
            max_message_size: 16 * 1024,
            ..Default::default()
        })
        .await
        .expect("idle fixture DLQ stream is created");

    jetstream
}

async fn assert_idle_expiry(
    broker: &async_nats::Client,
    pulls: &mut async_nats::Subscriber,
    scrape: &metrics_exporter_prometheus::PrometheusHandle,
) {
    let first = tokio::time::timeout_at(deadline(), pulls.next())
        .await
        .expect("the consumer sends the first idle pull")
        .expect("pull subscription remains open");
    let first_reply = first.reply.expect("idle pull has a reply subject");
    let mut replies = broker
        .subscribe(first_reply.clone())
        .await
        .expect("observe the first pull reply");
    broker.flush().await.expect("reply subscription is active");
    let expiry = tokio::time::timeout_at(deadline(), replies.next())
        .await
        .expect("the broker replies when the idle pull expires")
        .expect("reply subscription remains open");
    assert_eq!(expiry.status, Some(async_nats::StatusCode::TIMEOUT));
    let renewed = tokio::time::timeout_at(deadline(), pulls.next())
        .await
        .expect("the consumer renews the expired idle pull")
        .expect("pull subscription remains open");
    assert_ne!(
        renewed.reply.expect("renewed pull has a reply subject"),
        first_reply
    );

    let errors = scrape
        .render()
        .lines()
        .filter(|line| line.starts_with("messaging_consumer_stream_errors_total"))
        .map(|line| {
            line.rsplit_once(' ')
                .expect("stream error metric has a sample value")
                .1
                .parse::<u64>()
                .expect("stream error metric is a counter")
        })
        .sum::<u64>();
    assert_eq!(errors, 0, "an idle pull expiry counted as a failed batch");
}

#[tokio::test]
async fn idle_pull_expiry_is_not_a_failed_batch() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let scrape = recorder.handle();
    metrics::set_global_recorder(recorder).expect("this binary installs one recorder");

    let url = std::env::var("NATS_URL")
        .expect("NATS_URL is required; run just test-integration-messaging");
    let broker = async_nats::connect(&url)
        .await
        .expect("NATS_URL must point to the JetStream broker selected for this suite");
    let suffix = std::process::id();
    let (stream_name, subject, durable) = (
        format!("TEST_IDLE_{suffix}"),
        format!("test.idle.{suffix}"),
        format!("test_idle_{suffix}"),
    );
    let (dlq_stream, dlq_subject) = (
        format!("TEST_IDLE_DLQ_{suffix}"),
        format!("test.idle_dlq.{suffix}"),
    );
    let jetstream = provision(
        broker.clone(),
        &stream_name,
        &subject,
        &dlq_stream,
        &dlq_subject,
    )
    .await;

    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        MessagingOptions {
            connection_name: "infra-messaging-idle-test".to_owned(),
            servers: vec![url],
            credentials: None,
            root_ca_path: None,
            allow_plaintext: true,
            tls_first: false,
            source_stream: stream_name.clone(),
            max_payload_bytes: 1024,
            consumer: Some(ConsumerOptions {
                durable_name: durable.clone(),
                filter_subject: subject.clone(),
                dlq_subject,
                dlq_stream: Some(dlq_stream.clone()),
                concurrency: std::num::NonZeroUsize::MIN,
            }),
        },
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("idle fixture stream is admitted");
    let route = Route::<IdleEvent>::new(subject);
    let mut registry = Registry::default();
    registry.declare(&route).expect("route is valid");
    registry
        .register(&route, |_, _| async { Ok(()) })
        .expect("handler is registered");
    let consumer = messaging
        .consumer(registry, deadline(), &cancel)
        .await
        .expect("durable consumer is admitted");

    let stream = jetstream
        .get_stream(&stream_name)
        .await
        .expect("fixture stream");
    let bound = stream
        .get_consumer::<jetstream::consumer::pull::Config>(&durable)
        .await
        .expect("fixture durable");
    let mut config = bound.cached_info().config.clone();
    config.max_expires = Duration::from_secs(3);
    stream
        .update_consumer(config)
        .await
        .expect("short idle pull expiry");
    let mut pulls = broker
        .subscribe(format!("$JS.API.CONSUMER.MSG.NEXT.{stream_name}.{durable}"))
        .await
        .expect("observe pulls for the test durable");
    broker.flush().await.expect("pull subscription is active");

    let mut handle = consumer.start(&cancel);
    assert_idle_expiry(&broker, &mut pulls, &scrape).await;

    handle
        .finish(deadline())
        .await
        .expect("bounded consumer drain joins its pull task");
    assert_eq!(
        messaging.close(deadline(), &CancellationToken::new()).await,
        infra_messaging::CloseOutcome::Complete
    );
    for name in [stream_name, dlq_stream] {
        jetstream
            .delete_stream(&name)
            .await
            .expect("idle fixture stream is removable");
    }
}
