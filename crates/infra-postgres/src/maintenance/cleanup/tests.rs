use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};

use super::*;
use crate::maintenance::{MaintenanceFailure, run_scheduled_cleanup};
use crate::observe::tests::Events;

#[test]
fn cleanup_buckets_separate_a_seventeen_second_pass_from_upper_tail_overflow() {
    let recorder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(CLEANUP_DURATION_METRIC.to_owned()),
            CLEANUP_DURATION_BUCKETS,
        )
        .unwrap()
        .build_recorder();
    metrics::with_local_recorder(&recorder, || {
        let mut failed = CleanupPass::start(Cleanup::Jobs);
        failed.started -= Duration::from_secs(17);
        failed.committed(500);
        failed.failed();
        drop(failed);
        let mut long = CleanupPass::start(Cleanup::Jobs);
        long.started -= Duration::from_secs(3601);
        long.completed();
        drop(long);
    });
    let scrape = recorder.handle().render();
    for (outcome, bound, count) in [
        ("failed", "15", 0),
        ("failed", "30", 1),
        ("completed", "3600", 0),
        ("completed", "+Inf", 1),
    ] {
        assert!(
            scrape.lines().any(|line| line
                .starts_with("postgres_cleanup_pass_duration_seconds_bucket{")
                && line.contains(&format!("outcome=\"{outcome}\""))
                && line.contains(&format!("le=\"{bound}\""))
                && line.ends_with(&format!(" {count}"))),
            "{scrape}"
        );
    }
    assert!(scrape.contains("postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 500"));
    assert!(scrape.contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0"));
}

#[test]
fn cleanup_reports_live_progress_and_one_payload_free_terminal_event_per_pass() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let events = Events::new(tracing::Level::INFO);
    let started = tokio::time::Instant::now();
    metrics::with_local_recorder(&recorder, || {
        tracing::subscriber::with_default(events.clone(), || {
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            let mut completed = CleanupPass::start(Cleanup::Jobs);
            let mut failed = CleanupPass::start(Cleanup::Jobs);
            let mut cancelled = CleanupPass::start(Cleanup::Jobs);
            completed.committed(500);
            completed.committed(0);
            failed.committed(500);
            cancelled.committed(500);
            let scrape = recorder.handle().render();
            for line in [
                "postgres_cleanup_active_passes{cleanup=\"jobs\"} 3",
                "postgres_cleanup_committed_batches_total{cleanup=\"jobs\"} 4",
                "postgres_cleanup_removed_rows_total{cleanup=\"jobs\"} 1500",
            ] {
                assert!(scrape.contains(line), "{line} missing from {scrape}");
            }
            assert!(!scrape.contains("postgres_cleanup_passes_total{"));
            assert!(events.records.lock().unwrap().is_empty());
            completed.completed();
            drop(completed);
            assert!(
                recorder
                    .handle()
                    .render()
                    .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 2")
            );
            failed.failed();
            drop(failed);
            assert!(
                recorder
                    .handle()
                    .render()
                    .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 1")
            );
            drop(cancelled);
        });
    });
    let scrape = recorder.handle().render();
    assert!(scrape.contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0"));
    for outcome in ["completed", "failed", "cancelled"] {
        for metric in [
            "postgres_cleanup_passes_total",
            "postgres_cleanup_pass_duration_seconds_count",
        ] {
            let line = format!("{metric}{{cleanup=\"jobs\",outcome=\"{outcome}\"}} 1");
            assert!(scrape.contains(&line), "{line} missing from {scrape}");
        }
    }
    let events = events.records.lock().unwrap();
    assert_eq!(events.len(), 3);
    for (event, (outcome, batches)) in
        events
            .iter()
            .zip([("completed", "2"), ("failed", "1"), ("cancelled", "1")])
    {
        assert_eq!(event["message"], "postgres_cleanup_pass_finished");
        assert_eq!(event["cleanup"], "\"jobs\"");
        assert_eq!(event["outcome"], format!("\"{outcome}\""));
        assert_eq!(event["committed_batches"], batches);
        assert_eq!(event["removed_rows"], "500");
        let elapsed = event["elapsed_seconds"].parse::<f64>().unwrap();
        assert!(elapsed >= 0.0 && elapsed <= started.elapsed().as_secs_f64());
        assert_eq!(event.len(), 6, "no identifiers, SQL, errors or payloads");
    }
}

#[test]
fn cleanup_releases_the_original_active_gauge_after_moving_threads() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let pass = metrics::with_local_recorder(&recorder, || CleanupPass::start(Cleanup::Jobs));
    std::thread::spawn(move || drop(pass)).join().unwrap();
    assert!(
        recorder
            .handle()
            .render()
            .contains("postgres_cleanup_active_passes{cleanup=\"jobs\"} 0")
    );
}

const RUNS_METRIC: &str = "test_cleanup_runs_total";

#[tokio::test(start_paused = true)]
async fn scheduled_cleanup_counts_a_timed_out_batch_and_runs_the_next_pass() {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _recording = metrics::set_default_local_recorder(&recorder);
    let cancel = CancellationToken::new();
    let mut passes = 0;

    let cleanup = run_scheduled_cleanup(
        Cleanup::Jobs,
        RUNS_METRIC,
        &cancel,
        || {
            passes += 1;
            let pass_number = passes;
            let cancel = &cancel;
            async move {
                match pass_number {
                    1 => {
                        let mut pass = CleanupPass::start(Cleanup::Jobs);
                        pass.await_batch(std::future::pending()).await?;
                        Ok::<_, CleanupError>(PassEnd::Completed)
                    }
                    2 => {
                        let mut pass = CleanupPass::start(Cleanup::Jobs);
                        pass.await_batch(std::future::ready(Ok(7))).await?;
                        pass.completed();
                        Ok(PassEnd::Completed)
                    }
                    _ => {
                        cancel.cancel();
                        Ok(PassEnd::Cancelled)
                    }
                }
            }
        },
        || std::future::ready(Err(MaintenanceFailure::Statement)),
    );
    tokio::time::timeout(Duration::from_secs(180), cleanup)
        .await
        .expect("the stalled batch must not stop later scheduled passes");

    assert_eq!(passes, 3);
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains(&format!(r#"{RUNS_METRIC}{{outcome="failed"}} 1"#)),
        "{scrape}"
    );
    assert!(
        scrape.contains(&format!(r#"{RUNS_METRIC}{{outcome="completed"}} 1"#)),
        "{scrape}"
    );
    let series = scrape
        .lines()
        .filter(|line| line.starts_with(RUNS_METRIC))
        .count();
    assert_eq!(series, 2, "the cancelled pass is not counted: {scrape}");
    assert!(
        scrape.contains(r#"postgres_cleanup_passes_total{cleanup="jobs",outcome="failed"} 1"#),
        "{scrape}"
    );
    assert!(
        scrape.contains(r#"postgres_cleanup_committed_batches_total{cleanup="jobs"} 1"#),
        "{scrape}"
    );
    assert!(
        scrape.contains(r#"postgres_cleanup_removed_rows_total{cleanup="jobs"} 7"#),
        "{scrape}"
    );
}
