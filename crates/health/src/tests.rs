use std::sync::atomic::AtomicU32;

use proptest::prelude::*;

use super::*;

struct Flaky {
    healthy: Arc<AtomicBool>,
    calls: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl Probe for Flaky {
    fn name(&self) -> &'static str {
        "flaky"
    }
    async fn check(&self) -> Result<(), ProbeError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.healthy.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(ProbeError::from_static("connection refused"))
        }
    }
}

struct Hanging;

#[async_trait::async_trait]
impl Probe for Hanging {
    fn name(&self) -> &'static str {
        "hanging"
    }
    async fn check(&self) -> Result<(), ProbeError> {
        std::future::pending().await
    }
}

struct Slow {
    name: &'static str,
    delay: Duration,
}

#[async_trait::async_trait]
impl Probe for Slow {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn check(&self) -> Result<(), ProbeError> {
        tokio::time::sleep(self.delay).await;
        Ok(())
    }
}

struct Gated {
    hold: Arc<AtomicBool>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl Probe for Gated {
    fn name(&self) -> &'static str {
        "gated"
    }
    async fn check(&self) -> Result<(), ProbeError> {
        if self.hold.load(Ordering::Relaxed) {
            self.release.notified().await;
        }
        Ok(())
    }
}

fn policy() -> RefreshPolicy {
    RefreshPolicy::try_new(
        Duration::from_millis(50),
        Duration::from_millis(20),
        NonZeroU32::new(3).unwrap(),
    )
    .unwrap()
}

fn flaky_probe(healthy: bool) -> (Box<dyn Probe>, Arc<AtomicBool>, Arc<AtomicU32>) {
    let flag = Arc::new(AtomicBool::new(healthy));
    let calls = Arc::new(AtomicU32::new(0));
    let probe = Box::new(Flaky {
        healthy: Arc::clone(&flag),
        calls: Arc::clone(&calls),
    });
    (probe, flag, calls)
}

fn flaky(healthy: bool) -> (Readiness, Arc<AtomicBool>, Arc<AtomicU32>) {
    let (probe, flag, calls) = flaky_probe(healthy);
    (Readiness::new(vec![probe], policy()), flag, calls)
}

#[test]
fn probe_error_formats_borrowed_and_owned_messages() {
    let borrowed = ProbeError::from_static("connection refused");
    let owned = ProbeError::format(format_args!("upstream status {}", 503));
    assert_eq!(borrowed.to_string(), "connection refused");
    assert_eq!(owned.to_string(), "upstream status 503");
}

#[test]
fn refresh_policy_enforces_duration_bounds() {
    let threshold = NonZeroU32::new(3).unwrap();
    let valid = Duration::from_millis(50);

    for invalid in [
        Duration::ZERO,
        Duration::from_micros(999),
        Duration::from_hours(1) + Duration::from_nanos(1),
    ] {
        assert_eq!(
            RefreshPolicy::try_new(invalid, valid, threshold),
            Err(PolicyError::Interval(invalid))
        );
        assert_eq!(
            RefreshPolicy::try_new(valid, invalid, threshold),
            Err(PolicyError::ProbeBudget(invalid))
        );
    }

    for boundary in [Duration::from_millis(1), Duration::from_hours(1)] {
        let by_interval = RefreshPolicy::try_new(boundary, valid, threshold).unwrap();
        assert_eq!(by_interval.interval, boundary);
        assert_eq!(by_interval.probe_budget, valid);
        assert_eq!(by_interval.failure_threshold, threshold);

        let by_budget = RefreshPolicy::try_new(valid, boundary, threshold).unwrap();
        assert_eq!(by_budget.interval, valid);
        assert_eq!(by_budget.probe_budget, boundary);
        assert_eq!(by_budget.failure_threshold, threshold);
    }

    assert_eq!(policy().stale_after(), Duration::from_millis(170));
    let budget_dominated = RefreshPolicy::try_new(
        Duration::from_millis(10),
        Duration::from_millis(40),
        threshold,
    )
    .unwrap();
    assert_eq!(budget_dominated.stale_after(), Duration::from_millis(160));
}

#[tokio::test]
async fn fails_closed_before_first_evaluation() {
    let (readiness, _, _) = flaky(true);
    assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
}

#[tokio::test]
async fn refresh_seeds_the_verdict_and_reads_do_not_probe() {
    let (readiness, _, calls) = flaky(true);
    readiness.refresh().await;
    let reader = readiness.reader();
    for _ in 0..10 {
        assert_eq!(reader.verdict(), Ok(()));
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "verdict() must not run probes"
    );
}

#[tokio::test]
async fn cloned_owner_preserves_updates_without_readers() {
    let (readiness, _, _) = flaky(true);
    let other = readiness.clone();
    readiness.refresh().await;
    assert_eq!(other.reader().verdict(), Ok(()));

    drop(readiness);
    other.start_drain();
    assert_eq!(other.reader().verdict(), Err(NotReady::Draining));
}

#[tokio::test]
async fn healthy_instance_survives_blips_below_the_threshold() {
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    flag.store(false, Ordering::Relaxed);
    readiness.refresh().await;
    assert_eq!(readiness.reader().verdict(), Ok(()), "1 of 3 failures");
    readiness.refresh().await;
    assert_eq!(readiness.reader().verdict(), Ok(()), "2 of 3 failures");
    readiness.refresh().await;
    std::assert_matches!(
        readiness.reader().verdict(),
        Err(NotReady::ProbeFailed { probe: "flaky", .. }),
        "3 of 3 failures flips"
    );
    flag.store(true, Ordering::Relaxed);
    readiness.refresh().await;
    assert_eq!(readiness.reader().verdict(), Ok(()), "one success recovers");
}

#[tokio::test(start_paused = true)]
async fn is_draining_stays_false_through_every_verdict_until_drain_starts() {
    let (readiness, flag, _) = flaky(true);
    let reader = readiness.reader();

    assert_eq!(reader.verdict(), Err(NotReady::NotEvaluated));
    assert!(!reader.is_draining(), "not evaluated");

    readiness.refresh().await;
    assert_eq!(reader.verdict(), Ok(()));
    assert!(!reader.is_draining(), "ready");

    tokio::time::advance(policy().stale_after() + Duration::from_nanos(1)).await;
    std::assert_matches!(reader.verdict(), Err(NotReady::Stale { .. }));
    assert!(!reader.is_draining(), "stale");

    readiness.refresh().await;
    assert_eq!(
        reader.verdict(),
        Ok(()),
        "a refresh after staleness recovers"
    );
    assert!(!reader.is_draining(), "recovered from stale");

    flag.store(false, Ordering::Relaxed);
    readiness.refresh().await;
    assert_eq!(reader.verdict(), Ok(()));
    assert!(!reader.is_draining(), "absorbed failure");

    readiness.refresh().await;
    readiness.refresh().await;
    std::assert_matches!(reader.verdict(), Err(NotReady::ProbeFailed { .. }));
    assert!(!reader.is_draining(), "probe failed");

    flag.store(true, Ordering::Relaxed);
    readiness.refresh().await;
    assert_eq!(reader.verdict(), Ok(()));
    assert!(!reader.is_draining(), "recovered from failure");

    readiness.start_drain();
    assert_eq!(reader.verdict(), Err(NotReady::Draining));
    assert!(reader.is_draining(), "draining");
}

#[tokio::test(start_paused = true)]
async fn failed_refresh_only_absorbs_a_previous_ready_that_is_fresh_at_completion() {
    for previously_absorbed in [false, true] {
        for expired in [false, true] {
            let (readiness, flag, _) = flaky(true);
            readiness.refresh().await;
            flag.store(false, Ordering::Relaxed);
            if previously_absorbed {
                readiness.refresh().await;
            }
            let age = policy().stale_after()
                + if expired {
                    Duration::from_nanos(1)
                } else {
                    Duration::ZERO
                };
            tokio::time::advance(age).await;
            readiness.refresh().await;
            let verdict = readiness.reader().verdict();
            if expired {
                std::assert_matches!(verdict, Err(NotReady::ProbeFailed { .. }), "{verdict:?}");
                readiness.refresh().await;
                assert!(readiness.reader().verdict().is_err());
                flag.store(true, Ordering::Relaxed);
                readiness.refresh().await;
                assert_eq!(readiness.reader().verdict(), Ok(()));
                flag.store(false, Ordering::Relaxed);
                readiness.refresh().await;
                assert_eq!(
                    readiness.reader().verdict(),
                    Ok(()),
                    "success resets the streak"
                );
            } else {
                assert_eq!(verdict, Ok(()), "equality remains fresh");
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_failed_round_starting_fresh_cannot_revive_readiness_after_expiry() {
    let (probe, flag, _) = flaky_probe(true);
    let readiness = Readiness::new(
        vec![
            probe,
            Box::new(Slow {
                name: "slow",
                delay: policy().probe_budget / 2,
            }),
        ],
        policy(),
    );
    readiness.refresh().await;
    tokio::time::advance(policy().stale_after()).await;
    assert_eq!(readiness.reader().verdict(), Ok(()));
    flag.store(false, Ordering::Relaxed);
    readiness.refresh().await;
    std::assert_matches!(
        readiness.reader().verdict(),
        Err(NotReady::ProbeFailed { .. })
    );
}

#[tokio::test]
async fn never_healthy_instance_fails_immediately() {
    let (readiness, _, _) = flaky(false);
    std::assert_matches!(readiness.admit().await, Err(NotReady::ProbeFailed { .. }));
    std::assert_matches!(
        readiness.reader().verdict(),
        Err(NotReady::ProbeFailed { .. })
    );
}

#[tokio::test(start_paused = true)]
async fn a_hanging_probe_times_out_under_its_own_name() {
    let (healthy, _, _) = flaky_probe(true);
    let readiness = Readiness::new(vec![healthy, Box::new(Hanging)], policy());
    readiness.refresh().await;
    let verdict = readiness.reader().verdict();
    std::assert_matches!(
        verdict,
        Err(NotReady::TimedOut {
            probe: "hanging",
            ..
        }),
        "{verdict:?}"
    );
}

#[tokio::test]
async fn draining_wins_over_a_healthy_verdict_immediately() {
    let (readiness, _, _) = flaky(true);
    readiness.refresh().await;
    let reader = readiness.reader();
    readiness.start_drain();
    assert_eq!(reader.verdict(), Err(NotReady::Draining));
}

#[tokio::test(start_paused = true)]
async fn a_verdict_nobody_refreshes_goes_stale() {
    let (readiness, _, _) = flaky(true);
    readiness.refresh().await;
    let reader = readiness.reader();
    assert_eq!(reader.verdict(), Ok(()));

    tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
    std::assert_matches!(
        reader.verdict(),
        Err(NotReady::Stale { .. }),
        "{:?}",
        reader.verdict()
    );
}

#[tokio::test(start_paused = true)]
async fn refreshes_on_the_interval_and_stops_on_cancel() {
    let (readiness, flag, _) = flaky(true);
    let cancel = CancellationToken::new();
    let mut published = readiness.tx.subscribe();
    let refresher = tokio::spawn({
        let readiness = readiness.clone();
        let cancel = cancel.clone();
        async move { readiness.refresh_until(cancel).await }
    });
    published.changed().await.unwrap();
    let reader = readiness.reader();

    for _ in 0..10 {
        tokio::time::advance(policy().interval).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(
        reader.verdict(),
        Ok(()),
        "a running refresher keeps the verdict fresh"
    );

    flag.store(false, Ordering::Relaxed);
    for _ in 0..3 {
        tokio::time::advance(policy().interval).await;
        tokio::task::yield_now().await;
    }
    std::assert_matches!(reader.verdict(), Err(NotReady::ProbeFailed { .. }));

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), refresher)
        .await
        .expect("refresh_until must return on cancel")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn refresh_until_stops_while_an_evaluation_is_in_flight() {
    let readiness = Readiness::new(vec![Box::new(Hanging)], policy());
    let cancel = CancellationToken::new();
    let refresher = tokio::spawn({
        let cancel = cancel.clone();
        async move { readiness.refresh_until(cancel).await }
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    tokio::task::yield_now().await;
    assert!(
        refresher.is_finished(),
        "cancel must drop the in-flight refresh, not wait for probe_budget"
    );
    refresher.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_already_cancelled_refresher_never_checks_a_probe() {
    let (readiness, _, calls) = flaky(true);
    let cancel = CancellationToken::new();
    cancel.cancel();
    readiness.refresh_until(cancel).await;
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(readiness.reader().verdict(), Err(NotReady::NotEvaluated));
}

#[tokio::test]
async fn every_probe_is_checked_and_the_first_failure_in_registration_order_is_named() {
    let (healthy, _, healthy_calls) = flaky_probe(true);
    let (failing, _, failing_calls) = flaky_probe(false);
    let readiness = Readiness::new(
        vec![Box::new(Hanging), healthy, failing],
        RefreshPolicy::try_new(
            policy().interval,
            Duration::from_millis(1),
            policy().failure_threshold,
        )
        .unwrap(),
    );
    readiness.refresh().await;
    let verdict = readiness.reader().verdict();
    std::assert_matches!(
        verdict,
        Err(NotReady::TimedOut {
            probe: "hanging",
            ..
        }),
        "{verdict:?}"
    );
    assert_eq!(healthy_calls.load(Ordering::Relaxed), 1);
    assert_eq!(failing_calls.load(Ordering::Relaxed), 1);
}

#[tokio::test(start_paused = true)]
async fn a_drain_during_an_in_flight_check_wins_over_its_success() {
    let hold = Arc::new(AtomicBool::new(true));
    let release = Arc::new(tokio::sync::Notify::new());
    let readiness = Readiness::new(
        vec![Box::new(Gated {
            hold,
            release: Arc::clone(&release),
        })],
        policy(),
    );
    let refresh = readiness.refresh();
    tokio::pin!(refresh);
    assert!(futures_util::FutureExt::now_or_never(refresh.as_mut()).is_none());
    readiness.start_drain();
    release.notify_one();
    refresh.await;
    assert_eq!(readiness.reader().verdict(), Err(NotReady::Draining));
}

#[tokio::test(start_paused = true)]
async fn a_slow_probe_does_not_spend_another_probes_budget() {
    let delay = policy().probe_budget * 3 / 4;
    let readiness = Readiness::new(
        vec![
            Box::new(Slow {
                name: "first",
                delay,
            }),
            Box::new(Slow {
                name: "second",
                delay,
            }),
        ],
        policy(),
    );
    readiness.refresh().await;
    assert_eq!(readiness.reader().verdict(), Ok(()));
}

#[derive(Clone, Default)]
struct Events(Arc<std::sync::Mutex<Vec<String>>>);

impl tracing::Subscriber for Events {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push(message.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder()
}

#[track_caller]
fn assert_sample(scrape: &str, sample: &str) {
    assert!(scrape.lines().any(|line| line == sample), "{scrape}");
}

#[tokio::test(start_paused = true)]
async fn each_completed_check_is_counted_by_outcome() {
    let recorder = recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    flag.store(false, Ordering::Relaxed);
    readiness.refresh().await;
    readiness.refresh().await;
    Readiness::new(vec![Box::new(Hanging)], policy())
        .refresh()
        .await;
    let scrape = recorder.handle().render();

    for (outcome, count) in [("ok", 1), ("failed", 2), ("timed_out", 1)] {
        assert_sample(
            &scrape,
            &format!("readiness_checks_total{{outcome=\"{outcome}\"}} {count}"),
        );
    }
}

#[tokio::test(start_paused = true)]
async fn each_probe_is_counted_by_its_own_outcome_in_every_check() {
    let recorder = recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (healthy, _, _) = flaky_probe(true);
    let readiness = Readiness::new(
        vec![
            healthy,
            Box::new(Hanging),
            Box::new(Slow {
                name: "slow",
                delay: Duration::ZERO,
            }),
        ],
        policy(),
    );
    readiness.refresh().await;
    readiness.refresh().await;
    let scrape = recorder.handle().render();

    for (probe, outcome) in [("flaky", "ok"), ("hanging", "timed_out"), ("slow", "ok")] {
        assert_sample(
            &scrape,
            &format!("readiness_probe_checks_total{{probe=\"{probe}\",outcome=\"{outcome}\"}} 2"),
        );
    }
    assert_sample(&scrape, "readiness_checks_total{outcome=\"timed_out\"} 2");
}

#[tokio::test(start_paused = true)]
async fn absorbed_failures_flips_and_a_late_refresh_are_logged() {
    let events = Events::default();
    let _guard = tracing::subscriber::set_default(events.clone());
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    flag.store(false, Ordering::Relaxed);
    for _ in 0..4 {
        readiness.refresh().await;
    }
    flag.store(true, Ordering::Relaxed);
    readiness.refresh().await;
    tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
    readiness.refresh().await;

    assert_eq!(
        *events.0.lock().unwrap(),
        [
            "readiness_check_failed",
            "readiness_check_failed",
            "readiness_lost",
            "readiness_recovered",
            "readiness_refresh_late",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_draining_owner_logs_no_check_outcome() {
    let events = Events::default();
    let _guard = tracing::subscriber::set_default(events.clone());
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    readiness.start_drain();
    flag.store(false, Ordering::Relaxed);
    for _ in 0..3 {
        readiness.refresh().await;
    }

    assert!(events.0.lock().unwrap().is_empty(), "{:?}", events.0);
}

// template:begin grpc:health-changed-verdict-test
#[tokio::test(start_paused = true)]
async fn a_ready_watch_sleeps_through_steady_and_absorbed_refreshes_and_wakes_on_each_flip() {
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move {
        let changed = reader.wait_until_ready_is_not(true).await;
        (reader, changed)
    });
    tokio::task::yield_now().await;

    for _ in 0..6 {
        tokio::time::advance(policy().interval).await;
        readiness.refresh().await;
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "a ready -> ready refresh past the first stale boundary must not end the wait"
        );
    }

    flag.store(false, Ordering::Relaxed);
    for _ in 0..2 {
        tokio::time::advance(policy().interval).await;
        readiness.refresh().await;
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "a failure absorbed below the threshold must not end the wait"
        );
    }

    tokio::time::advance(policy().interval).await;
    readiness.refresh().await;
    let (mut reader, changed) = tokio::time::timeout(Duration::from_millis(1), waiter)
        .await
        .expect("the threshold failure must end the wait at once")
        .unwrap();
    assert_eq!(changed, Some(false));

    flag.store(true, Ordering::Relaxed);
    readiness.refresh().await;
    assert_eq!(
        futures_util::FutureExt::now_or_never(reader.wait_until_ready_is_not(false)),
        Some(Some(true)),
        "a recovered verdict must be reported without waiting"
    );
}

#[tokio::test(start_paused = true)]
async fn wait_for_drain_or_owner_drop_resolves_on_both_events() {
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move {
        reader.wait_for_drain_or_owner_drop().await;
        reader
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    flag.store(false, Ordering::Relaxed);
    for _ in 0..3 {
        readiness.refresh().await;
    }
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "probe failure must not resolve wait_for_drain_or_owner_drop"
    );

    readiness.start_drain();
    let mut reader = tokio::time::timeout(Duration::from_millis(1), waiter)
        .await
        .expect("start_drain must resolve wait_for_drain_or_owner_drop")
        .unwrap();
    assert!(reader.is_draining());
    assert!(futures_util::FutureExt::now_or_never(reader.wait_for_drain_or_owner_drop()).is_some());

    let readiness = Readiness::new(Vec::new(), policy());
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move {
        reader.wait_for_drain_or_owner_drop().await;
        reader
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    drop(readiness);
    let reader = tokio::time::timeout(Duration::from_millis(1), waiter)
        .await
        .expect("owner drop must resolve wait_for_drain_or_owner_drop")
        .unwrap();
    assert!(!reader.is_draining());
}

#[tokio::test(start_paused = true)]
async fn a_ready_watch_reports_the_stale_boundary_once_then_the_next_successful_refresh() {
    let (readiness, _, calls) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move {
        let changed = reader.wait_until_ready_is_not(true).await;
        (reader, changed)
    });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "a fresh ready check must wait for its stale boundary"
    );

    tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
    let (mut reader, changed) = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("the stale boundary must end the wait without a refresh")
        .unwrap();
    assert_eq!(changed, Some(false));

    let waiter = tokio::spawn(async move { reader.wait_until_ready_is_not(false).await });
    tokio::task::yield_now().await;
    tokio::time::advance(policy().stale_after()).await;
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "the same stale check must not end a not-ready wait"
    );

    readiness.refresh().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(1), waiter)
            .await
            .expect("a successful refresh must end the not-ready wait")
            .unwrap(),
        Some(true)
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "waiting must not run probes"
    );
}

#[tokio::test(start_paused = true)]
async fn a_ready_watch_reports_a_failed_check_published_after_a_stall_before_it_is_polled_again() {
    let (readiness, flag, _) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    let wait = reader.wait_until_ready_is_not(true);
    tokio::pin!(wait);
    assert!(futures_util::FutureExt::now_or_never(wait.as_mut()).is_none());

    tokio::time::advance(policy().stale_after() + Duration::from_millis(5)).await;
    flag.store(false, Ordering::Relaxed);
    readiness.refresh().await;
    std::assert_matches!(
        readiness.reader().verdict(),
        Err(NotReady::ProbeFailed { .. })
    );

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(60), wait.as_mut()).await,
        Ok(Some(false)),
        "a watcher that last saw ready must learn about the failed check"
    );
}

#[tokio::test(start_paused = true)]
async fn a_ready_watch_reports_a_stall_that_ends_in_a_timed_out_check_of_a_running_refresher() {
    let hold = Arc::new(AtomicBool::new(false));
    let readiness = Readiness::new(
        vec![Box::new(Gated {
            hold: Arc::clone(&hold),
            release: Arc::new(tokio::sync::Notify::new()),
        })],
        policy(),
    );
    let mut reader = readiness.reader();
    let cancel = CancellationToken::new();
    let refresher = tokio::spawn({
        let readiness = readiness.clone();
        let cancel = cancel.clone();
        async move { readiness.refresh_until(cancel).await }
    });
    assert_eq!(
        tokio::time::timeout(policy().probe_budget, reader.wait_until_ready_is_not(false)).await,
        Ok(Some(true)),
        "the refresher's first check must publish ready"
    );

    let waiter = tokio::spawn(async move { reader.wait_until_ready_is_not(true).await });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    hold.store(true, Ordering::Relaxed);
    tokio::time::advance(policy().interval).await;
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "a check in flight within the stale boundary must not end the wait"
    );

    tokio::time::advance(policy().stale_after() + Duration::from_secs(1)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(1), waiter)
            .await
            .expect("a stall past stale_after must end the ready wait")
            .unwrap(),
        Some(false)
    );
    assert!(readiness.reader().verdict().is_err());

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), refresher)
        .await
        .expect("refresh_until must return on cancel")
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_ready_watch_reports_drain_as_not_ready_then_ends() {
    let (readiness, _, _) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move {
        let changed = reader.wait_until_ready_is_not(true).await;
        (reader, changed)
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());

    readiness.start_drain();
    let (mut reader, changed) = tokio::time::timeout(Duration::from_millis(1), waiter)
        .await
        .expect("start_drain must end a ready wait at once")
        .unwrap();
    assert_eq!(changed, Some(false));

    readiness.refresh().await;
    assert_eq!(
        futures_util::FutureExt::now_or_never(reader.wait_until_ready_is_not(false)),
        Some(None),
        "a draining service can never become ready again"
    );
}

#[tokio::test(start_paused = true)]
async fn a_not_ready_watch_ends_when_drain_starts() {
    let (readiness, _, _) = flaky(false);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    std::assert_matches!(reader.verdict(), Err(NotReady::ProbeFailed { .. }));
    let waiter = tokio::spawn(async move { reader.wait_until_ready_is_not(false).await });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "a failed verdict must wait for a change"
    );

    readiness.start_drain();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(1), waiter)
            .await
            .expect("start_drain must end a not-ready wait at once")
            .unwrap(),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn a_ready_watch_dropped_mid_wait_still_reports_the_stale_boundary_later() {
    let (readiness, _, _) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    tokio::select! {
        changed = reader.wait_until_ready_is_not(true) => panic!("a fresh ready check ended the wait early: {changed:?}"),
        () = tokio::time::sleep(policy().stale_after() / 2) => {}
    }
    tokio::time::advance(policy().stale_after()).await;
    assert_eq!(
        tokio::time::timeout(
            Duration::from_millis(1),
            reader.wait_until_ready_is_not(true)
        )
        .await,
        Ok(Some(false)),
        "the dropped wait must not swallow the stale boundary"
    );
}

#[tokio::test(start_paused = true)]
async fn a_watch_ends_when_every_owner_is_dropped() {
    let readiness = Readiness::new(Vec::new(), policy());
    let survivor = readiness.clone();
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move { reader.wait_until_ready_is_not(false).await });
    drop(readiness);
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "a remaining owner can still publish readiness"
    );

    drop(survivor);
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(1), waiter)
            .await
            .expect("dropping the last owner must end the wait")
            .unwrap(),
        None
    );
}
// template:end grpc:health-changed-verdict-test

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn the_failure_threshold_never_outlives_its_streak_or_a_stale_gap(
        failure_threshold in 1_u32..6,
        stale_after_ms in 1_u64..100,
        steps in proptest::collection::vec((any::<bool>(), 0_u64..150), 1..40),
    ) {
        let stale_after = Duration::from_millis(stale_after_ms);
        let origin = Instant::now();
        let mut elapsed_ms = 0_u64;
        let mut previous: Option<Check> = None;
        let mut streak = 0_u32;
        let mut published: Option<(bool, u64)> = None;
        for (succeeds, gap_ms) in steps {
            elapsed_ms += gap_ms;
            let at = origin + Duration::from_millis(elapsed_ms);
            let observed = if succeeds {
                Ok(())
            } else {
                Err(ProbeFailure::TimedOut { probe: "model", budget: Duration::from_millis(1) })
            };
            let threshold = NonZeroU32::new(failure_threshold).unwrap();
            let (check, _) =
                apply_failure_threshold(previous.as_ref(), observed, threshold, at, stale_after);

            streak = if succeeds { 0 } else { streak + 1 };
            let fresh_ready = published.is_some_and(|(ready, at_ms)| {
                ready && elapsed_ms - at_ms <= stale_after_ms
            });
            let expect_ready = succeeds || (fresh_ready && streak < failure_threshold);

            prop_assert_eq!(check.verdict.is_ok(), expect_ready);
            prop_assert_eq!(check.consecutive_failures, streak);
            published = Some((expect_ready, elapsed_ms));
            previous = Some(check);
        }
    }
}
