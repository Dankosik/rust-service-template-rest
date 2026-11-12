use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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
            Err(ProbeError::new("connection refused"))
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

fn policy() -> RefreshPolicy {
    RefreshPolicy {
        interval: Duration::from_millis(50),
        probe_budget: Duration::from_millis(20),
        failure_threshold: 3,
    }
}

fn flaky_probe(healthy: bool) -> (Box<dyn Probe>, Arc<AtomicBool>, Arc<AtomicU32>) {
    let flag = Arc::new(AtomicBool::new(healthy));
    let calls = Arc::new(AtomicU32::new(0));
    let probe = Box::new(Flaky {
        healthy: flag.clone(),
        calls: calls.clone(),
    });
    (probe, flag, calls)
}

fn flaky(healthy: bool) -> (Readiness, Arc<AtomicBool>, Arc<AtomicU32>) {
    let (probe, flag, calls) = flaky_probe(healthy);
    (Readiness::new(vec![probe], policy()), flag, calls)
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
            // No read is needed to mark the prior publication stale.
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
    readiness.refresh().await;
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
async fn arming_uses_the_admitted_completion_and_refuses_failed_admission() {
    let (readiness, flag, _) = flaky(false);
    assert_eq!(readiness.arm_progress(), Err(NotReady::NotEvaluated));
    readiness.refresh().await;
    std::assert_matches!(readiness.arm_progress(), Err(NotReady::ProbeFailed { .. }));
    assert!(readiness.progress_loss().is_none());
    flag.store(true, Ordering::Relaxed);
    readiness.refresh().await;
    tokio::time::advance(policy().stale_after()).await;
    assert_eq!(readiness.arm_progress(), Ok(()), "equality is fresh");
    tokio::time::advance(Duration::from_nanos(1)).await;
    assert!(
        readiness.progress_loss().is_some(),
        "arm must not reset age"
    );

    let (late, _, _) = flaky(true);
    late.refresh().await;
    tokio::time::advance(policy().stale_after() + Duration::from_nanos(1)).await;
    std::assert_matches!(late.arm_progress(), Err(NotReady::Stale { .. }));
    assert!(late.progress_loss().is_some());
}

#[tokio::test(start_paused = true)]
async fn armed_late_success_retains_the_gap_before_an_observer_can_run() {
    let (readiness, _, _) = flaky(true);
    readiness.refresh().await;
    readiness.arm_progress().unwrap();
    tokio::time::advance(policy().stale_after() + Duration::from_nanos(1)).await;
    // No observer or reader sees the gap before this success publishes.
    readiness.refresh().await;
    let loss = readiness.progress_loss().expect("terminal completion gap");
    std::assert_matches!(readiness.reader().verdict(), Err(NotReady::Stale { .. }));
    readiness.refresh().await;
    assert_eq!(readiness.progress_loss(), Some(loss));
    assert_eq!(readiness.stop_progress(), Some(loss));
    readiness.start_drain();
    readiness.refresh().await;
    assert_eq!(readiness.progress_loss(), Some(loss));
    assert_eq!(readiness.reader().verdict(), Err(NotReady::Draining));
}

#[tokio::test(start_paused = true)]
async fn progress_observer_needs_no_readers_or_new_completions() {
    let readiness = Readiness::new(Vec::new(), policy());
    readiness.refresh().await;
    readiness.arm_progress().unwrap();
    let cancel = CancellationToken::new();
    let mut observer = Box::pin(readiness.wait_for_progress_loss(cancel));
    assert!(futures_util::FutureExt::now_or_never(observer.as_mut()).is_none());
    tokio::time::advance(policy().stale_after()).await;
    assert!(
        readiness.progress_loss().is_none(),
        "strictly above the bound"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    let loss = tokio::time::timeout(Duration::from_millis(1), observer)
        .await
        .expect("observer wakes without a reader or publication")
        .expect("progress loss");
    assert_eq!(readiness.progress_loss(), Some(loss));
}

#[tokio::test(start_paused = true)]
async fn stop_and_drain_close_obligations_but_preserve_an_already_expired_gap() {
    for drain in [false, true] {
        for expired in [false, true] {
            let (readiness, _, _) = flaky(true);
            readiness.refresh().await;
            readiness.arm_progress().unwrap();
            tokio::time::advance(
                policy().stale_after()
                    + if expired {
                        Duration::from_nanos(1)
                    } else {
                        Duration::ZERO
                    },
            )
            .await;
            if drain {
                readiness.start_drain();
            } else {
                assert_eq!(readiness.stop_progress().is_some(), expired);
            }
            tokio::time::advance(policy().stale_after() * 2).await;
            assert_eq!(readiness.progress_loss().is_some(), expired);
            readiness.refresh().await;
            assert_eq!(readiness.progress_loss().is_some(), expired);
            readiness.start_drain();
            assert_eq!(readiness.reader().verdict(), Err(NotReady::Draining));
            if !expired {
                let cancel = CancellationToken::new();
                let mut observer = Box::pin(readiness.wait_for_progress_loss(cancel.clone()));
                assert!(futures_util::FutureExt::now_or_never(observer.as_mut()).is_none());
                cancel.cancel();
                assert_eq!(observer.await, None, "stop alone cannot end tracked work");
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn failed_and_timed_out_completed_rounds_keep_armed_progress_recoverable() {
    struct Controlled(Arc<AtomicU32>);
    #[async_trait::async_trait]
    impl Probe for Controlled {
        fn name(&self) -> &'static str {
            "controlled"
        }
        async fn check(&self) -> Result<(), ProbeError> {
            match self.0.load(Ordering::Relaxed) {
                0 => Ok(()),
                1 => Err(ProbeError::new("unavailable")),
                _ => std::future::pending().await,
            }
        }
    }
    for outcome in [1, 2] {
        let control = Arc::new(AtomicU32::new(0));
        let readiness = Readiness::new(vec![Box::new(Controlled(control.clone()))], policy());
        readiness.refresh().await;
        readiness.arm_progress().unwrap();
        control.store(outcome, Ordering::Relaxed);
        for _ in 0..10 {
            tokio::time::advance(policy().interval).await;
            readiness.refresh().await;
            assert!(readiness.progress_loss().is_none());
        }
        assert!(readiness.reader().verdict().is_err());
        control.store(0, Ordering::Relaxed);
        readiness.refresh().await;
        assert_eq!(readiness.reader().verdict(), Ok(()));
        assert!(readiness.progress_loss().is_none());
    }
}

#[test]
fn terminal_progress_withdraws_the_gauge_and_logs_once_without_false_recovery() {
    let events = Events::default();
    let _guard = tracing::subscriber::set_default(events.clone());
    let scrape = scrape(async {
        let (readiness, flag, _) = flaky(true);
        readiness.refresh().await;
        readiness.arm_progress().unwrap();
        flag.store(false, Ordering::Relaxed);
        for _ in 0..3 {
            readiness.refresh().await;
        }
        tokio::time::advance(policy().stale_after() + Duration::from_nanos(1)).await;
        flag.store(true, Ordering::Relaxed);
        readiness.refresh().await;
        assert!(readiness.progress_loss().is_some());
        let _ = readiness.stop_progress();
        readiness.start_drain();
        readiness.refresh().await;
    });
    assert_sample(&scrape, "readiness_ready 0");
    let recorded = events.0.lock().unwrap();
    assert_eq!(
        recorded
            .iter()
            .filter(|event| *event == "readiness_progress_lost")
            .count(),
        1
    );
    assert!(!recorded.iter().any(|event| event == "readiness_recovered"));
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
        let readiness = readiness.clone();
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

#[tokio::test(start_paused = true)]
async fn a_zero_interval_refreshes_at_the_timer_resolution() {
    let (probe, _, calls) = flaky_probe(true);
    let readiness = Readiness::new(
        vec![probe],
        RefreshPolicy {
            interval: Duration::ZERO,
            probe_budget: Duration::from_micros(100),
            ..policy()
        },
    );
    let cancel = CancellationToken::new();
    let refresher = tokio::spawn({
        let readiness = readiness.clone();
        let cancel = cancel.clone();
        async move { readiness.refresh_until(cancel).await }
    });
    readiness.tx.subscribe().changed().await.unwrap();
    readiness.arm_progress().unwrap();
    for _ in 0..3 {
        tokio::time::advance(MIN_INTERVAL).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(readiness.reader().verdict(), Ok(()));
    assert!(
        readiness.progress_loss().is_none(),
        "a refresher on schedule is not a completion gap"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        4,
        "the first check, then one per millisecond"
    );

    cancel.cancel();
    refresher
        .await
        .expect("a zero interval must not panic the refresher");
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

#[tokio::test]
async fn every_probe_is_checked_and_the_first_failure_in_registration_order_is_named() {
    let (healthy, _, healthy_calls) = flaky_probe(true);
    let (failing, _, failing_calls) = flaky_probe(false);
    let readiness = Readiness::new(
        vec![Box::new(Hanging), healthy, failing],
        RefreshPolicy {
            probe_budget: Duration::from_millis(1),
            ..policy()
        },
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
async fn a_slow_probe_does_not_spend_another_probes_budget() {
    // Each probe needs most of the budget; one after the other they
    // would overrun it.
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

fn paused_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .expect("test runtime")
}

fn scrape(test: impl Future<Output = ()>) -> String {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    metrics::with_local_recorder(&recorder, || paused_runtime().block_on(test));
    recorder.handle().render()
}

#[track_caller]
fn assert_sample(scrape: &str, sample: &str) {
    assert!(scrape.lines().any(|line| line == sample), "{scrape}");
}

#[test]
fn each_completed_check_is_counted_by_outcome() {
    let scrape = scrape(async {
        let (readiness, flag, _) = flaky(true);
        readiness.refresh().await;
        flag.store(false, Ordering::Relaxed);
        readiness.refresh().await;
        readiness.refresh().await;
        Readiness::new(vec![Box::new(Hanging)], policy())
            .refresh()
            .await;
    });

    for (outcome, count) in [("ok", 1), ("failed", 2), ("timed_out", 1)] {
        assert_sample(
            &scrape,
            &format!("readiness_checks_total{{outcome=\"{outcome}\"}} {count}"),
        );
    }
}

#[test]
fn each_probe_is_counted_by_its_own_outcome_in_every_check() {
    let scrape = scrape(async {
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
    });

    for (probe, outcome) in [("flaky", "ok"), ("hanging", "timed_out"), ("slow", "ok")] {
        assert_sample(
            &scrape,
            &format!("readiness_probe_checks_total{{probe=\"{probe}\",outcome=\"{outcome}\"}} 2"),
        );
    }
    // The check itself is counted once per round, under the first failure.
    assert_sample(&scrape, "readiness_checks_total{outcome=\"timed_out\"} 2");
}

#[test]
fn the_ready_gauge_is_the_published_verdict() {
    let ready = |scrape: String| {
        scrape
            .lines()
            .find(|line| line.starts_with("readiness_ready "))
            .map(str::to_owned)
    };
    let before_the_first_check = scrape(async {
        let _ = flaky(true);
    });
    assert_eq!(
        ready(before_the_first_check).as_deref(),
        Some("readiness_ready 0")
    );

    let absorbed_failure = scrape(async {
        let (readiness, flag, _) = flaky(true);
        readiness.refresh().await;
        flag.store(false, Ordering::Relaxed);
        readiness.refresh().await;
    });
    assert_eq!(
        ready(absorbed_failure).as_deref(),
        Some("readiness_ready 1")
    );

    let withdrawn = scrape(async {
        let (readiness, flag, _) = flaky(true);
        readiness.refresh().await;
        flag.store(false, Ordering::Relaxed);
        for _ in 0..3 {
            readiness.refresh().await;
        }
    });
    assert_eq!(ready(withdrawn).as_deref(), Some("readiness_ready 0"));

    let draining = scrape(async {
        let (readiness, _, _) = flaky(true);
        readiness.refresh().await;
        readiness.start_drain();
        // A check that passes during the drain does not bring it back.
        readiness.refresh().await;
    });
    assert_eq!(ready(draining).as_deref(), Some("readiness_ready 0"));
}

#[test]
fn completion_metrics_date_only_finished_checks_and_survive_stopped_refresh() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let timestamp = || {
        handle
            .render()
            .lines()
            .find_map(|line| line.strip_prefix("readiness_last_completed_timestamp_seconds "))
            .expect("completion timestamp is exposed")
            .parse::<f64>()
            .unwrap()
    };
    metrics::with_local_recorder(&recorder, || {
        paused_runtime().block_on(async {
            let (readiness, flag, _) = flaky(true);
            assert_eq!(timestamp(), 0.0);
            assert_sample(&handle.render(), "readiness_stale_after_seconds 0.17");
            let before = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            readiness.refresh().await;
            let completed = timestamp();
            let after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            assert!((before..=after).contains(&completed));
            tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
            assert_eq!(
                timestamp().to_bits(),
                completed.to_bits(),
                "stopped refresh leaves a dated completion"
            );
            assert_sample(&handle.render(), "readiness_ready 1");
            std::assert_matches!(readiness.reader().verdict(), Err(NotReady::Stale { .. }));
            readiness.start_drain();
            assert_eq!(
                timestamp().to_bits(),
                completed.to_bits(),
                "drain does not forge a completion"
            );
            flag.store(false, Ordering::Relaxed);
            let before_failure = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            readiness.refresh().await;
            assert!(
                timestamp() >= before_failure,
                "failure during drain is still a completion"
            );
            assert_sample(&handle.render(), "readiness_ready 0");

            let hanging = Readiness::new(vec![Box::new(Hanging)], policy());
            let cancel = CancellationToken::new();
            let refresh = hanging.refresh_until(cancel.clone());
            tokio::pin!(refresh);
            std::future::poll_fn(|cx| {
                assert!(refresh.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            assert_eq!(timestamp(), 0.0, "in-flight is not completed");
            cancel.cancel();
            refresh.await;
            assert_eq!(timestamp(), 0.0, "cancellation is not completed");
            hanging.refresh().await;
            assert!(timestamp() > 0.0, "a timed-out round is completed");
        });
    });
}

#[test]
fn absorbed_failures_flips_and_a_late_refresh_are_logged() {
    let events = Events::default();
    tracing::subscriber::with_default(events.clone(), || {
        // Keep callsite interest independent of a sibling test's thread-local
        // subscriber; tracing-core otherwise has a single-dispatcher fast path.
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        paused_runtime().block_on(async {
            let (readiness, flag, _) = flaky(true);
            readiness.refresh().await;
            flag.store(false, Ordering::Relaxed);
            // Two absorbed failures, the flip, then a failure that is
            // already published.
            for _ in 0..4 {
                readiness.refresh().await;
            }
            flag.store(true, Ordering::Relaxed);
            readiness.refresh().await;
            tokio::time::advance(policy().stale_after() + Duration::from_millis(1)).await;
            readiness.refresh().await;
        });
    });

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

#[test]
fn a_draining_owner_logs_no_check_outcome() {
    let events = Events::default();
    tracing::subscriber::with_default(events.clone(), || {
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        paused_runtime().block_on(async {
            let (readiness, flag, _) = flaky(true);
            readiness.refresh().await;
            readiness.start_drain();
            flag.store(false, Ordering::Relaxed);
            for _ in 0..3 {
                readiness.refresh().await;
            }
        });
    });

    assert!(events.0.lock().unwrap().is_empty(), "{:?}", events.0);
}

// template:begin grpc:health-changed-verdict-test
#[tokio::test(start_paused = true)]
async fn wait_for_verdict_event_delivers_drain_once_then_waits_without_losing_drain() {
    let (readiness, _, calls) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();

    readiness.start_drain();
    assert_eq!(reader.wait_for_verdict_event().await, Ok(()));

    let waiter = tokio::spawn(async move { reader.wait_for_verdict_event().await });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "observed draining must wait for another publication"
    );

    readiness.refresh().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("drain publication waiter must finish")
            .expect("drain publication waiter must not panic"),
        Ok(())
    );
    assert_eq!(readiness.reader().verdict(), Err(NotReady::Draining));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[tokio::test(start_paused = true)]
async fn wait_for_verdict_event_delivers_each_stale_edge_once_without_a_probe() {
    let (readiness, _, calls) = flaky(true);
    readiness.refresh().await;
    let mut reader = readiness.reader();
    let waiter = tokio::spawn(async move {
        let result = reader.wait_for_verdict_event().await;
        (reader, result)
    });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "a fresh check must wait for its stale boundary"
    );

    tokio::time::advance(policy().stale_after() + Duration::from_nanos(1)).await;
    let (mut reader, result) = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("stale boundary waiter must finish")
        .expect("stale boundary waiter must not panic");
    assert_eq!(result, Ok(()));

    let waiter = tokio::spawn(async move { reader.wait_for_verdict_event().await });
    tokio::task::yield_now().await;
    assert!(
        !waiter.is_finished(),
        "the same stale check must not wake repeatedly"
    );

    readiness.refresh().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("publication waiter must finish")
            .expect("publication waiter must not panic"),
        Ok(())
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn wait_for_verdict_event_reports_when_the_owner_is_dropped() {
    let readiness = Readiness::new(Vec::new(), policy());
    let mut reader = readiness.reader();
    drop(readiness);
    assert_eq!(reader.wait_for_verdict_event().await, Err(OwnerDropped));
}
// template:end grpc:health-changed-verdict-test
