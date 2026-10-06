#![allow(
    clippy::expect_used,
    reason = "integration tests make failures and broker setup explicit"
)]
//! Its own test binary: the stream-error counter is process-wide and
//! unlabelled, so other suites' deliberate failures would pollute it.

use std::time::Duration;

use async_nats::jetstream::{self, consumer, stream};
use domain_events::EventPayload;
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

/// The operator-owned source stream, DLQ stream and durable of the fixture.
async fn provision(
    url: &str,
    stream_name: &str,
    subject: &str,
    durable: &str,
    dlq_stream: &str,
    dlq_subject: &str,
) -> jetstream::Context {
    let jetstream = jetstream::new(
        async_nats::connect(url)
            .await
            .expect("NATS_URL must point to the JetStream broker selected for this suite"),
    );
    let created = jetstream
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
            max_message_size: 1024 + 8 * 1024,
            ..Default::default()
        })
        .await
        .expect("idle fixture DLQ stream is created");
    created
        .create_consumer(consumer::pull::Config {
            durable_name: Some(durable.to_owned()),
            filter_subject: subject.to_owned(),
            ..Default::default()
        })
        .await
        .expect("idle fixture durable is created");

    jetstream
}

/// An idle pull ends with the broker's own expiry answer, not a failed batch:
/// the local bound outlasts `expires`, so a quiet stream raises no stream
/// error. One pull lifetime and a margin pass before the check.
#[tokio::test]
async fn idle_pull_expiry_is_not_a_failed_batch() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let scrape = recorder.handle();
    metrics::set_global_recorder(recorder).expect("this binary installs one recorder");

    let url = std::env::var("NATS_URL")
        .expect("NATS_URL is required; run this suite through test-integration-messaging.sh");
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
        &url,
        &stream_name,
        &subject,
        &durable,
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
            credentials_file: None,
            root_ca_path: None,
            allow_plaintext: true,
            tls_first: false,
            source_stream: stream_name.clone(),
            dlq_stream: Some(dlq_stream.clone()),
            max_payload_bytes: 1024,
            consumer: Some(ConsumerOptions {
                durable_name: durable,
                filter_subject: subject.clone(),
                dlq_subject,
                concurrency: 1,
            }),
        },
        deadline(),
        cancel.clone(),
    ))
    .await
    .expect("idle fixture stream is admitted");
    let mut registry = Registry::new([Route::new::<IdleEvent>(subject)]).expect("route is valid");
    registry
        .register::<IdleEvent, _, _>(|_, _| async { Ok(()) })
        .expect("handler is registered");
    let mut handle = messaging
        .consumer(registry)
        .await
        .expect("durable consumer is admitted")
        .start(&cancel);

    tokio::time::sleep(Duration::from_secs(33)).await;

    let errors = scrape
        .render()
        .lines()
        .filter(|line| line.starts_with("messaging_consumer_stream_errors_total"))
        .filter_map(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .sum::<f64>();
    assert!(
        errors < 1.0,
        "an idle pull expiry counted as a failed batch"
    );

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
