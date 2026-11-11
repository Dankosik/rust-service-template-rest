//! The readiness probe.

use super::*;

#[tokio::test]
async fn a_fresh_probe_rejects_silent_stream_metadata_then_recovers() {
    let fixture = Fixture::create(false).await;
    let withhold_stream = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let withhold = Arc::clone(&withhold_stream);
    let source_stream = fixture.stream.clone();
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        withhold.load(Ordering::SeqCst)
            && serde_json::from_slice::<serde_json::Value>(payload).is_ok_and(|stream| {
                stream["config"]["name"] == source_stream && stream.get("created").is_some()
            })
    })
    .await;
    let messaging = Messaging::connect(
        options_with_servers(&fixture, vec![relay.url.clone()], None, 1024),
        deadline(),
        CancellationToken::new(),
    )
    .await
    .expect("fixture source stream is admitted");

    withhold_stream.store(true, Ordering::SeqCst);
    assert!(
        timeout(Duration::from_secs(7), messaging.probe().check())
            .await
            .expect("the existing native request bound ends the silent probe")
            .is_err(),
        "a connected socket is insufficient without fresh source metadata"
    );
    relay.wait_for_drop().await;

    withhold_stream.store(false, Ordering::SeqCst);
    timeout(Duration::from_secs(7), messaging.probe().check())
        .await
        .expect("a restored metadata reply is observed in a new health round")
        .expect("fresh source metadata restores readiness");
    close(messaging).await;
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
async fn a_completed_native_runner_refuses_the_retained_probe() {
    let fixture = Fixture::create(false).await;
    let messaging = Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        CancellationToken::new(),
    )
    .await
    .expect("fixture source stream is admitted");
    let probe = messaging.probe();
    close(messaging).await;
    assert!(
        probe.check().await.is_err(),
        "native runner completion stays a local readiness refusal after close"
    );
    fixture.cleanup().await;
}
