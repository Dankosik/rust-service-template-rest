use std::sync::atomic::Ordering;

use metrics::{Key, Level, Metadata, Recorder as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use operation_context::Stopped;

use super::*;
use crate::{MutationError, OperationError, SetError};

const RESTORE_METADATA: Metadata<'static> = Metadata::new(module_path!(), Level::INFO, None);

struct TestMetrics {
    recorder: DebuggingRecorder,
    snapshotter: Snapshotter,
}

impl TestMetrics {
    fn new() -> Self {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        Self {
            recorder,
            snapshotter,
        }
    }

    fn recorder(&self) -> &DebuggingRecorder {
        &self.recorder
    }

    fn snapshot(&self) -> Vec<(metrics_util::CompositeKey, DebugValue)> {
        self.snapshotter
            .snapshot()
            .into_vec()
            .into_iter()
            .map(|(composite, _, _, value)| {
                match &value {
                    DebugValue::Counter(count) if *count != 0 => {
                        self.recorder
                            .register_counter(composite.key(), &RESTORE_METADATA)
                            .increment(*count);
                    }
                    DebugValue::Gauge(gauge) if gauge.into_inner() != 0.0 => {
                        self.recorder
                            .register_gauge(composite.key(), &RESTORE_METADATA)
                            .increment(gauge.into_inner());
                    }
                    _ => {}
                }
                (composite, value)
            })
            .collect()
    }

    fn retirements(&self) -> u64 {
        let target = Key::from_name(crate::observe::RETIREMENTS_METRIC);
        self.snapshot()
            .iter()
            .find_map(|(composite, value)| match value {
                DebugValue::Counter(count) if composite.key() == &target => Some(*count),
                _ => None,
            })
            .unwrap_or(0)
    }

    async fn wait_for_retirements(&self, expected: u64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.retirements() < expected {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the supervisor did not record {expected} retirements"));
    }
}

async fn wait_until_connected(cache: &Cache, budget: Duration) {
    let probe = cache.namespace(namespace!("connected_probe"));
    tokio::time::timeout(budget, async {
        while probe.get("probe").await.is_err() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("cache did not publish a connected generation within {budget:?}"));
}

fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
    let captured = CapturedLogs::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (captured, tracing::subscriber::set_default(subscriber))
}

fn connection_failed_with(logs: &str, error_type: &str) -> bool {
    logs.lines().any(|line| {
        line.contains("cache_connection_failed")
            && (line.contains(&format!("error.type=\"{error_type}\""))
                || line.contains(&format!("error.type={error_type}")))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_auth_is_retried_after_the_client_gives_up() {
    let gate = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_millis(200),
        ..options(
            &format!("redis://:secret@{}", gate.address),
            true,
            false,
            None,
        )
    })
    .await
    .expect("start connect");
    let namespace = cache.namespace(namespace!("auth"));

    gate.wait_for(
        Duration::from_secs(20),
        "setup retries did not progress without traffic",
        || gate.attempts() >= 7,
    )
    .await;
    gate.accept_auth.store(true, Ordering::SeqCst);
    gate.wait_for(
        Duration::from_secs(5),
        "authentication did not recover without traffic",
        || gate.observed.auth_successes.load(Ordering::SeqCst) > 0,
    )
    .await;
    wait_until_connected(&cache, Duration::from_secs(1)).await;
    assert_eq!(namespace.get("key").await, Ok(None));
}

#[test]
fn late_reconnect_delays_spread_below_the_ceiling_instead_of_pinning_to_it() {
    let delays: Vec<Duration> = crate::connection::setup_backoff().take(140).collect();
    assert_eq!(delays.len(), 140);

    let ceiling = Duration::from_secs(2);
    assert!(
        delays
            .iter()
            .all(|delay| (Duration::from_millis(100)..=ceiling).contains(delay)),
        "{delays:?}"
    );
    let late = &delays[4..];
    assert!(
        late.iter()
            .all(|delay| (Duration::from_secs(1)..=ceiling).contains(delay)),
        "capped retries must never drop below 1s: {late:?}"
    );
    assert!(
        late.iter()
            .any(|delay| *delay < Duration::from_millis(1_500))
            && late
                .iter()
                .any(|delay| *delay >= Duration::from_millis(1_500)),
        "capped retries must spread across [1s, 2s]: {late:?}"
    );
    let distinct: std::collections::BTreeSet<Duration> = late.iter().copied().collect();
    assert!(
        distinct.len() > late.len() / 2,
        "clients restarted together must not retry in lockstep: {late:?}"
    );
}

#[tokio::test]
async fn a_server_error_reply_redis_rs_does_not_name_is_a_response_failure() {
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start connect");

    let namespace = cache.namespace(namespace!("replies"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    assert_eq!(
        namespace.get("wrongtype").await,
        Err(OperationError::Unavailable)
    );
    assert_eq!(namespace.get("after").await, Ok(None));
    assert_eq!(
        server.connections(),
        connections,
        "an error reply answers its own command and keeps the shared connection"
    );
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains(
            "cache_operation_duration_seconds_count{cache=\"replies\",operation=\"get\",outcome=\"error\",error_type=\"response\"} 1"
        ),
        "{scrape}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_readonly_reply_reconnects_to_the_new_primary() {
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_millis(200),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start connect");
    let namespace = cache.namespace(namespace!("failover"));
    let ttl = Duration::from_secs(1);

    namespace
        .set("key", b"value", ttl)
        .await
        .expect("set on the current primary");
    server.primary.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        namespace.set("key", b"value", ttl).await,
        Err(SetError::Mutation(MutationError::NotApplied(
            OperationError::Unavailable
        )))
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if namespace.set("key", b"value", ttl).await.is_ok() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cache stayed unavailable after the primary generation advanced"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        server.connections() >= 2,
        "READONLY must open a connection to the new primary"
    );
}

#[tokio::test]
async fn mutations_require_a_recognized_acknowledgement_or_refusal() {
    let refused = MutationError::NotApplied(OperationError::Unavailable);
    let unknown = MutationError::OutcomeUnknown(OperationError::Unavailable);
    for (set, reply, expected) in [
        (true, &b"-NOPERM access denied\r\n"[..], Err(refused)),
        (true, b"-NOREPLICAS not enough replicas\r\n", Err(refused)),
        (false, b"-BUSY script running\r\n", Err(refused)),
        (true, b"-REDIRECT primary:6379\r\n", Err(refused)),
        (false, b"-ERR unspecified failure\r\n", Err(unknown)),
        (true, b":1\r\n", Err(unknown)),
        (false, b"+OK\r\n", Err(unknown)),
        (false, b":2\r\n", Err(unknown)),
        (false, b":0\r\n", Ok(())),
    ] {
        let server = FakeServer::start().await;
        let cache = admitted(&format!("redis://{}", server.address), true, true).await;
        wait_until_connected(&cache, Duration::from_secs(2)).await;
        let namespace = cache.namespace(namespace!("replies"));
        *server
            .observed
            .next_mutation_reply
            .lock()
            .expect("mutation reply lock") = Some(reply);
        if set {
            assert_eq!(
                namespace.set("key", b"value", Duration::from_secs(1)).await,
                expected.map_err(SetError::Mutation),
                "{reply:?}"
            );
        } else {
            assert_eq!(namespace.delete("key").await, expected, "{reply:?}");
        }
        let command = if set { "SET" } else { "DEL" };
        assert_eq!(server.command_count(command, Some("replies:key")), 1);
    }
}

#[tokio::test]
async fn a_readonly_reply_after_cancellation_reconnects_to_the_new_primary() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(15),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("late_readonly"));
    let limit = Duration::from_secs(5);
    let ttl = Duration::from_secs(1);
    assert_eq!(
        tokio::time::timeout(limit, namespace.get("ready")).await,
        Ok(Ok(None))
    );
    server.primary.fetch_add(1, Ordering::SeqCst);
    let (release, held) = tokio::sync::oneshot::channel();
    *server
        .observed
        .next_set_reply
        .lock()
        .expect("SET reply gate lock") = Some(held);

    let context = OperationContext::unbounded();
    let write = namespace.set_with_context("cancelled", b"value", ttl, &context);
    tokio::pin!(write);
    tokio::select! {
        result = &mut write => panic!("held write completed: {result:?}"),
        () = server.wait_for(limit, "write must reach the old primary", || {
            server.command_count("SET", Some("late_readonly:cancelled")) == 1
        }) => {}
    }
    context.cancel();
    assert_eq!(
        tokio::time::timeout(limit, write).await,
        Ok(Err(SetError::Mutation(MutationError::OutcomeUnknown(
            OperationError::Stopped(Stopped::Cancelled)
        ))))
    );
    assert_eq!(server.connections(), 1);
    assert!(
        !server.closed_through(1),
        "early cancellation must leave the socket open"
    );
    release
        .send(())
        .expect("the old primary is holding its reply");

    server
        .wait_for(limit, "late READONLY must trigger a reconnect", || {
            server.connections() == 2 && server.closed_through(1)
        })
        .await;
    wait_until_connected(&cache, limit).await;
    assert_eq!(
        tokio::time::timeout(limit, namespace.set("new", b"value", ttl)).await,
        Ok(Ok(()))
    );
    assert_eq!(
        server.command_count("SET", Some("late_readonly:cancelled")),
        1
    );
}

#[tokio::test]
async fn a_refused_hello_is_reported_as_auth() {
    let (captured, _subscriber) = capture_logs();
    let gate = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_millis(200),
        ..options(
            &format!("redis://:secret@{}", gate.address),
            true,
            false,
            None,
        )
    })
    .await
    .expect("start connect");

    captured
        .wait_for(Duration::from_secs(20), |logs| {
            connection_failed_with(logs, "auth")
        })
        .await
        .expect("the refused password was not reported as auth");
    assert_eq!(
        cache.namespace(namespace!("auth")).get("key").await,
        Err(OperationError::Unavailable)
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn a_password_file_unreadable_at_setup_is_not_reported_as_auth() {
    let (captured, _subscriber) = capture_logs();
    let server = FakeServer::start().await;
    let password = ephemeral_password();
    server.require_password(&password);
    let file = temp_file(&password);
    let _cache = Cache::start(with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    ))
    .await
    .expect("start cache");
    std::fs::remove_file(file.path()).expect("remove password file");

    captured
        .wait_for(Duration::from_secs(5), |logs| {
            connection_failed_with(logs, "other")
        })
        .await
        .expect("the unreadable password file was not reported as other");
    assert!(!connection_failed_with(&captured.rendered(), "auth"));
    assert_eq!(server.command_count("HELLO", None), 0);
}

#[tokio::test]
async fn caller_cancellation_records_cancelled_and_an_expired_deadline_timeout() {
    use operation_context::OperationContext;

    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("stop"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    let cancelled = OperationContext::with_timeout(Duration::from_secs(1));
    cancelled.cancel();
    assert_eq!(
        namespace.get_with_context("key", &cancelled).await,
        Err(OperationError::Stopped(Stopped::Cancelled))
    );
    let expired = OperationContext::with_timeout(Duration::ZERO);
    assert_eq!(
        namespace.get_with_context("key", &expired).await,
        Err(OperationError::Stopped(Stopped::Deadline))
    );

    let scrape = recorder.handle().render();
    for series in [
        "cache_operation_duration_seconds_count{cache=\"stop\",operation=\"get\",outcome=\"cancelled\"} 1",
        "cache_operation_duration_seconds_count{cache=\"stop\",operation=\"get\",outcome=\"timeout\",error_type=\"timeout\"} 1",
    ] {
        assert!(scrape.contains(series), "{series} missing from {scrape}");
    }

    let during_build = OperationContext::unbounded();
    let write = cache.link.command::<()>(
        || {
            let mut command = redis::cmd("DEL");
            command.arg("stop:during_build");
            during_build.cancel();
            command
        },
        Some(&during_build),
        crate::namespace::Operation::Delete,
    );
    assert!(matches!(
        write.await,
        Err(crate::connection::CommandError::NotApplied(
            crate::connection::CommandFailure::Stopped(Stopped::Cancelled)
        ))
    ));
    assert_eq!(namespace.get("after").await, Ok(None));
    assert_eq!(server.command_count("DEL", Some("stop:during_build")), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_reply_within_command_timeout_survives_the_maintenance_ping() {
    let server = FakeServer::start().await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_millis(7_500);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(9),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("budget"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();

    assert_eq!(namespace.get("slow").await, Ok(None));
    assert!(server.command_count("PING", None) > 0, "no PING was queued");
    assert_eq!(server.connections(), connections);
}

#[tokio::test]
async fn a_large_command_timeout_keeps_maintenance_and_commands_usable() {
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::MAX,
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("large_budget"));
    let limit = Duration::from_secs(6);
    assert_eq!(
        tokio::time::timeout(limit, namespace.get("first")).await,
        Ok(Ok(None))
    );

    server
        .wait_for(limit, "maintenance must send recurring PINGs", || {
            server.command_count("PING", None) >= 2
        })
        .await;
    assert_eq!(
        tokio::time::timeout(limit, namespace.get("ready")).await,
        Ok(Ok(None))
    );
}

#[tokio::test]
async fn ping_permission_denial_keeps_the_connection_usable_and_is_reported_once() {
    let (captured, _subscriber) = capture_logs();
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("ping_permission"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    let pings = server.command_count("PING", None);
    server.observed.deny_ping.store(true, Ordering::SeqCst);

    server
        .wait_for(
            Duration::from_secs(7),
            "maintenance PING did not recur",
            || server.command_count("PING", None) >= pings + 3,
        )
        .await;
    assert_eq!(namespace.get("after_maintenance").await, Ok(None));
    assert_eq!(server.connections(), connections);
    assert_eq!(
        captured.rendered().matches("cache_ping_failed").count(),
        1,
        "a run of refused PINGs must be reported once"
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "finite writes simulate password rotation in the fixture"
)]
async fn a_slow_command_survives_overlapping_password_rotation() {
    let server = FakeServer::start().await;
    let initial = ephemeral_password();
    let rotated = ephemeral_password();
    server.require_password(&initial);
    let file = temp_file(&initial);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(10),
        ..with_password_file(
            &format!("redis://{}", server.address),
            file.path().to_path_buf(),
        )
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("rotation_budget"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    server
        .wait_for(
            Duration::from_secs(5),
            "maintenance did not reach its second PING",
            || server.command_count("PING", None) >= 2,
        )
        .await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_secs(8);

    tokio::join!(
        async { assert_eq!(namespace.get("slow").await, Ok(None)) },
        async {
            server
                .wait_for(
                    Duration::from_secs(1),
                    "slow GET did not reach the server",
                    || server.command_count("GET", Some("rotation_budget:slow")) == 1,
                )
                .await;
            server.require_password(&rotated);
            std::fs::write(file.path(), &rotated).expect("rotated password");
            server
                .wait_for(
                    Duration::from_secs(10),
                    "rotation AUTH did not reach the server",
                    || server.command_count("AUTH", None) == 1,
                )
                .await;
        }
    );
    assert_eq!(server.connections(), connections);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_idle_connection_reconnects_before_the_next_call() {
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("idle"));
    assert_eq!(namespace.get("key").await, Ok(None));
    assert_eq!(server.connections(), 1);

    server.hang_up();
    server
        .wait_for(
            Duration::from_secs(5),
            "the client waited for a call before it reconnected",
            || server.connections() >= 2,
        )
        .await;
    wait_until_connected(&cache, Duration::from_secs(1)).await;
    assert_eq!(namespace.get("key").await, Ok(None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_connection_dials_without_waiting_for_a_call() {
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("warm"));
    let ttl = Duration::from_secs(1);
    namespace
        .set("key", b"value", ttl)
        .await
        .expect("set on the current primary");
    server.primary.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        namespace.set("key", b"value", ttl).await,
        Err(SetError::Mutation(MutationError::NotApplied(
            OperationError::Unavailable
        )))
    );

    server
        .wait_for(
            Duration::from_secs(5),
            "the replaced connection waited for a call before it dialed",
            || server.connections() >= 2,
        )
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_connection_dials_without_waiting_for_a_call() {
    let server = FakeServer::start().await;
    let _cache = admitted(&format!("redis://{}", server.address), true, true).await;

    server
        .wait_for(
            Duration::from_secs(5),
            "the first connection waited for a call before it dialed",
            || server.connections() >= 1,
        )
        .await;
}

fn assert_work_metrics(metrics: &TestMetrics, in_flight: u32, refused: u64, retirements: u64) {
    let snapshot = metrics.snapshot();
    for (name, expected) in [
        (
            crate::observe::COMMANDS_IN_FLIGHT_METRIC,
            DebugValue::Gauge(f64::from(in_flight).into()),
        ),
        (
            crate::observe::ADMISSION_REFUSED_METRIC,
            DebugValue::Counter(refused),
        ),
        (
            crate::observe::RETIREMENTS_METRIC,
            DebugValue::Counter(retirements),
        ),
    ] {
        let target = Key::from_name(name);
        let actual = snapshot
            .iter()
            .find_map(|(composite, value)| (composite.key() == &target).then_some(value));
        assert_eq!(
            actual,
            Some(&expected),
            "{target} missing from {snapshot:?}"
        );
    }
}

#[tokio::test]
async fn reliability_admission_bounds_connection_waits_without_cancelling_setup() {
    use std::task::Poll;

    use crate::connection::{CommandError, CommandFailure, ErrorType};

    let server = FakeServer::start().await;
    *server
        .observed
        .hello_reply_delay
        .lock()
        .expect("delay lock") = Duration::from_millis(50);
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(1),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    assert_work_metrics(&metrics, 0, 0, 0);
    let namespace = cache.namespace(namespace!("waiting"));
    let mut waiting = Vec::new();
    for _ in 0..256 {
        let mut command = Box::pin(namespace.get("pending"));
        assert!(futures_util::poll!(command.as_mut()).is_pending());
        waiting.push(command);
    }
    assert_work_metrics(&metrics, 256, 0, 0);
    let other = admitted(&format!("redis://{}", server.address), true, true).await;
    assert_work_metrics(&metrics, 256, 0, 0);
    drop(other);
    let mut excess = Box::pin(namespace.get("refused"));
    assert_eq!(
        futures_util::poll!(excess.as_mut()),
        Poll::Ready(Err(OperationError::Unavailable))
    );
    let context = operation_context::OperationContext::unbounded();
    let refused = cache.link.command::<()>(
        || panic!("a refused command must not be built"),
        Some(&context),
        crate::namespace::Operation::Delete,
    );
    assert!(matches!(
        futures_util::poll!(std::pin::pin!(refused)),
        Poll::Ready(Err(CommandError::NotApplied(CommandFailure::Failed(
            ErrorType::Other
        ))))
    ));
    assert_work_metrics(&metrics, 256, 2, 0);
    drop(waiting.pop());
    assert_work_metrics(&metrics, 255, 2, 0);
    let mut replacement = Box::pin(namespace.get("replacement"));
    assert!(futures_util::poll!(replacement.as_mut()).is_pending());
    drop(waiting);
    assert_work_metrics(&metrics, 1, 2, 0);
    assert_eq!(replacement.await, Ok(None));
    assert_work_metrics(&metrics, 0, 2, 0);
    assert_eq!(server.connections(), 1);
    assert_eq!(server.command_count("GET", Some("waiting:pending")), 0);
    assert_eq!(server.command_count("GET", Some("waiting:refused")), 0);
}

#[tokio::test]
async fn reliability_full_application_window_leaves_maintenance_admission() {
    use std::task::Poll;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(5),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("capacity"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
    let pings = server.command_count("PING", None);
    let mut pending = Vec::from_fn(256, |_| Box::pin(namespace.get("pending")));
    tokio::select! {
        () = std::future::poll_fn(|context| {
            for command in &mut pending {
                assert!(command.as_mut().poll(context).is_pending());
            }
            Poll::<()>::Pending
        }) => unreachable!(),
        () = server.wait_for(
            Duration::from_secs(1),
            "application window never reached the socket",
            || server.command_count("GET", Some("capacity:pending")) == 256,
        ) => {}
    }
    let sibling = cache.namespace(namespace!("other_capacity"));
    let mut excess = Box::pin(sibling.delete("refused"));
    assert_eq!(
        futures_util::poll!(excess.as_mut()),
        Poll::Ready(Err(MutationError::NotApplied(OperationError::Unavailable)))
    );
    server
        .wait_for(
            Duration::from_secs(3),
            "maintenance queued behind admission",
            || server.command_count("PING", None) == pings + 1,
        )
        .await;
    assert_eq!(
        server.connections(),
        old,
        "refusal retired the current connection"
    );
    assert_eq!(
        server.command_count("DEL", Some("other_capacity:refused")),
        0
    );
    server
        .wait_for(
            Duration::from_secs(7),
            "maintenance did not replace the saturated generation",
            || server.connections() > old,
        )
        .await;
    server
        .wait_for(Duration::from_secs(1), "old socket stayed open", || {
            server.closed_through(old)
        })
        .await;
    drop(pending);
    assert_eq!(namespace.get("recovered").await, Ok(None));
}

#[tokio::test]
async fn reliability_stalled_generations_recover_without_replaying_writes() {
    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("stalled"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    for cycle in 0..3 {
        let old = server.stall_existing();
        let key = format!("write-{cycle}");
        let started = tokio::time::Instant::now();
        assert_eq!(
            namespace
                .set(&key, b"effect-may-have-happened", Duration::from_secs(1))
                .await,
            Err(SetError::Mutation(MutationError::OutcomeUnknown(
                OperationError::Unavailable
            )))
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "SET exceeded its 100 ms budget plus scheduling allowance"
        );
        metrics.wait_for_retirements(cycle + 1).await;
        assert_work_metrics(&metrics, 0, 0, cycle + 1);
        server
            .wait_for(
                Duration::from_secs(4),
                "stalled generation was not replaced and released",
                || server.connections() > old && server.closed_through(old),
            )
            .await;
        wait_until_connected(&cache, Duration::from_secs(1)).await;
        assert_eq!(namespace.get("ready").await, Ok(None));
        assert_eq!(
            server.command_count("SET", Some(&format!("stalled:{key}"))),
            1,
            "effect-ambiguous SET was replayed"
        );
        assert_eq!(
            server.active(),
            1,
            "retired sockets accumulated across recovery cycles"
        );
    }

    let old = server.stall_existing();
    assert_eq!(
        namespace.delete("delete-once").await,
        Err(MutationError::OutcomeUnknown(OperationError::Unavailable))
    );
    metrics.wait_for_retirements(4).await;
    assert_work_metrics(&metrics, 0, 0, 4);
    server
        .wait_for(
            Duration::from_secs(4),
            "DEL timeout did not recover",
            || server.connections() > old && server.closed_through(old),
        )
        .await;
    wait_until_connected(&cache, Duration::from_secs(1)).await;
    assert_eq!(namespace.get("ready").await, Ok(None));
    assert_eq!(
        server.command_count("DEL", Some("stalled:delete-once")),
        1,
        "effect-ambiguous DEL was replayed"
    );
}

#[tokio::test]
async fn reliability_a_command_reply_timeout_retires_the_connection() {
    use std::task::Poll;

    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(5),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("reply_timeout"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
    let mut first = Box::pin(namespace.get("first"));
    tokio::select! {
        biased;
        result = &mut first => panic!("stalled command completed before timeout: {result:?}"),
        () = server.wait_for(
            Duration::from_secs(1),
            "command did not reach the established socket",
            || server.command_count("GET", Some("reply_timeout:first")) == 1,
        ) => {}
    }
    assert_work_metrics(&metrics, 1, 0, 0);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let late_key = "late";
    let peer_key = "peer";
    let mut late = Box::pin(namespace.get(late_key));
    let mut peer = Box::pin(namespace.get(peer_key));
    tokio::select! {
        () = std::future::poll_fn(|context| {
            assert!(late.as_mut().poll(context).is_pending());
            assert!(peer.as_mut().poll(context).is_pending());
            Poll::<()>::Pending
        }) => unreachable!(),
        () = server.wait_for(
            Duration::from_secs(1),
            "peers never reached established socket",
            || server.command_count("GET", Some(&format!("reply_timeout:{late_key}"))) == 1
                && server.command_count("GET", Some(&format!("reply_timeout:{peer_key}"))) == 1,
        ) => {}
    }
    assert_work_metrics(&metrics, 3, 0, 0);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(6), first)
            .await
            .expect("command must observe its own reply timeout"),
        Err(OperationError::Unavailable)
    );
    metrics.wait_for_retirements(1).await;
    assert_work_metrics(&metrics, 2, 0, 1);
    assert_eq!(
        futures_util::poll!(peer.as_mut()),
        Poll::Ready(Err(OperationError::Unavailable))
    );
    assert_work_metrics(&metrics, 1, 0, 1);
    let mut replacement = Box::pin(namespace.get("replacement"));
    assert!(futures_util::poll!(replacement.as_mut()).is_pending());
    assert_work_metrics(&metrics, 2, 0, 1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(4), replacement)
            .await
            .expect("normal supervisor recovery"),
        Ok(None)
    );
    assert_work_metrics(&metrics, 1, 0, 1);
    assert!(server.connections() > old);
    assert_eq!(
        futures_util::poll!(late.as_mut()),
        Poll::Ready(Err(OperationError::Unavailable))
    );
    assert_work_metrics(&metrics, 0, 0, 1);
    assert_eq!(namespace.get("successor-after-old-failure").await, Ok(None));
    server
        .wait_for(
            Duration::from_secs(1),
            "old response slots survived",
            || server.closed_through(old),
        )
        .await;
    assert_eq!(server.active(), 1);
}

#[tokio::test]
async fn a_caller_stop_before_the_ceiling_keeps_the_connection_for_peers() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_millis(300);
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(5),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("caller_stop"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();

    let short = OperationContext::with_timeout(Duration::from_millis(50));
    let (peer, dropped, stopped) = tokio::join!(
        namespace.get("slow"),
        tokio::time::timeout(Duration::from_millis(50), namespace.get("slow")),
        namespace.get_with_context("slow", &short),
    );
    assert_eq!(peer, Ok(None));
    assert!(
        dropped.is_err(),
        "the dropped GET was answered first: {dropped:?}"
    );
    assert_eq!(stopped, Err(OperationError::Stopped(Stopped::Deadline)));
    assert_eq!(namespace.get("after").await, Ok(None));
    assert_eq!(
        server.command_count("GET", Some("caller_stop:slow")),
        3,
        "a stopped GET never reached the connection"
    );
    assert_work_metrics(&metrics, 0, 0, 0);
    assert_eq!(server.connections(), connections);
}

#[tokio::test]
async fn reliability_final_owner_drop_cancels_inflight_setup() {
    let server = FakeServer::start().await;
    server.observed.silence_all.store(true, Ordering::SeqCst);
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    server
        .wait_for(
            Duration::from_secs(1),
            "setup never reached the socket",
            || server.command_count("HELLO", None) > 0,
        )
        .await;
    drop(cache);
    server
        .wait_for(
            Duration::from_millis(500),
            "final drop retained setup until the connection or warm-up timeout",
            || server.active() == 0,
        )
        .await;
}

#[tokio::test]
async fn stopped_contexts_dispatch_no_commands_or_retire_shared_connection() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("context"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    for reason in [Stopped::Cancelled, Stopped::Deadline] {
        let context = match reason {
            Stopped::Cancelled => {
                let context = OperationContext::unbounded();
                context.cancel();
                context
            }
            Stopped::Deadline => OperationContext::with_timeout(Duration::ZERO),
        };
        let cause = OperationError::Stopped(reason);
        assert_eq!(
            namespace.get_with_context("stopped", &context).await,
            Err(cause)
        );
        assert_eq!(
            namespace
                .set_with_context("stopped", b"value", Duration::from_secs(1), &context)
                .await,
            Err(SetError::Mutation(MutationError::NotApplied(cause)))
        );
        assert_eq!(
            namespace.delete_with_context("stopped", &context).await,
            Err(MutationError::NotApplied(cause))
        );
    }
    assert_eq!(namespace.get("live").await, Ok(None));
    assert_eq!(
        server.connections(),
        connections,
        "pre-dispatch caller stop must not retire the shared connection"
    );
    for command in ["GET", "SET", "DEL"] {
        assert_eq!(server.command_count(command, Some("context:stopped")), 0);
    }
}

#[tokio::test]
async fn dropping_a_command_before_dispatch_keeps_the_connection() {
    use tokio::task::coop::{consume_budget, has_budget_remaining};

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("yield_before_send"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();

    tokio::spawn(async move {
        while has_budget_remaining() {
            consume_budget().await;
        }
        {
            let stopped = namespace.get("stopped");
            tokio::pin!(stopped);
            assert!(futures_util::poll!(stopped.as_mut()).is_pending());
        }
        assert_eq!(namespace.get("live").await, Ok(None));
    })
    .await
    .expect("command task completes");

    assert_eq!(server.connections(), connections);
    assert_eq!(
        server.command_count("GET", Some("yield_before_send:stopped")),
        0
    );
}

#[tokio::test]
async fn caller_cutoff_bounds_connection_acquisition() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    server.observed.silence_all.store(true, Ordering::SeqCst);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(5),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("cache");
    let namespace = cache.namespace(namespace!("acquire"));
    let context = OperationContext::with_timeout(Duration::from_millis(100));
    assert_eq!(
        tokio::time::timeout(
            Duration::from_millis(500),
            namespace.get_with_context("pending", &context)
        )
        .await
        .expect("parent cutoff must end acquisition before the five-second local ceiling"),
        Err(OperationError::Stopped(Stopped::Deadline))
    );
    assert_eq!(server.command_count("GET", Some("acquire:pending")), 0);
}

#[tokio::test]
async fn acknowledged_write_keeps_its_result_after_cancellation_and_disconnect() {
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(10),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("acknowledged_write"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let context = operation_context::OperationContext::unbounded();
    let write = namespace.set_with_context("key", b"value", Duration::from_secs(10), &context);
    tokio::pin!(write);
    assert!(futures_util::poll!(write.as_mut()).is_pending());
    server
        .wait_for(Duration::from_secs(1), "SET was not sent", || {
            server.command_count("SET", Some("acknowledged_write:key")) == 1
        })
        .await;
    assert_eq!(namespace.get("fence").await, Ok(None));
    let old = server.connections();
    assert_eq!(old, 1);
    context.cancel();
    server.hang_up();
    server
        .wait_for(Duration::from_secs(5), "cache did not reconnect", || {
            server.connections() > old
        })
        .await;

    assert_eq!(write.await, Ok(()));
}

#[tokio::test]
async fn cancelling_a_read_with_a_ready_reply_keeps_the_connection() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("cancel_ready_read"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let context = OperationContext::unbounded();
    let read = namespace.get_with_context("key", &context);
    tokio::pin!(read);
    assert!(futures_util::poll!(read.as_mut()).is_pending());
    server
        .wait_for(Duration::from_secs(1), "GET was not sent", || {
            server.command_count("GET", Some("cancel_ready_read:key")) == 1
        })
        .await;
    assert_eq!(namespace.get("fence").await, Ok(None));

    context.cancel();
    assert_eq!(read.await, Err(OperationError::Stopped(Stopped::Cancelled)));
    assert_eq!(namespace.get("live").await, Ok(None));
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn dropping_an_exchange_with_a_delivered_reply_keeps_the_connection() {
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("drop_ready_reply"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    {
        let exchange = namespace.get("discarded");
        tokio::pin!(exchange);
        assert!(futures_util::poll!(exchange.as_mut()).is_pending());
        assert_eq!(namespace.get("fence").await, Ok(None));
        assert_eq!(
            server.command_count("GET", Some("drop_ready_reply:discarded")),
            1
        );
    }
    assert_eq!(namespace.get("live").await, Ok(None));
    assert_eq!(server.connections(), 1);
}

#[test]
fn a_command_after_the_supervisor_exited_fails_instead_of_hanging() {
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let cache = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("setup runtime")
        .block_on(admitted("redis://127.0.0.1:6379", true, true));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("wait runtime");
    runtime.block_on(async {
        let namespace = cache.namespace(namespace!("supervisor_exit"));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), namespace.get("key")).await,
            Ok(Err(OperationError::Unavailable))
        );
    });
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains(
            "cache_operation_duration_seconds_count{cache=\"supervisor_exit\",operation=\"get\",outcome=\"error\",error_type=\"io\"} 1"
        ),
        "{scrape}"
    );
}

#[tokio::test]
async fn cancellation_after_write_dispatch_leaves_a_stalled_server_to_maintenance_without_replay() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(1),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("cache");
    let namespace = cache.namespace(namespace!("cancel_effect"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
    let context = OperationContext::with_timeout(Duration::from_secs(5));
    let write = namespace.set_with_context("once", b"value", Duration::from_secs(1), &context);
    tokio::pin!(write);
    tokio::select! {
        result = &mut write => panic!("stalled write completed: {result:?}"),
        () = server.wait_for(Duration::from_secs(1), "write must reach provider", || server.command_count("SET", Some("cancel_effect:once")) == 1) => {},
    }
    assert_work_metrics(&metrics, 1, 0, 0);
    context.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(200), write)
            .await
            .expect("cancellation must end waiting before the local ceiling"),
        Err(SetError::Mutation(MutationError::OutcomeUnknown(
            OperationError::Stopped(Stopped::Cancelled)
        )))
    );
    assert_work_metrics(&metrics, 0, 0, 0);
    server
        .wait_for(
            Duration::from_secs(5),
            "maintenance must replace the stalled generation",
            || server.connections() > old && server.closed_through(old),
        )
        .await;
    wait_until_connected(&cache, Duration::from_secs(1)).await;
    assert_eq!(namespace.get("recovered").await, Ok(None));
    assert_eq!(server.command_count("SET", Some("cancel_effect:once")), 1);
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned credential replacement completes before refresh observes it"
)]
async fn an_overdue_ping_runs_before_another_slow_password_refresh() {
    let server = FakeServer::start().await;
    let initial = ephemeral_password();
    server.require_password(&initial);
    let file = temp_file(initial);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(10),
        ..with_password_file(
            &format!("redis://{}", server.address),
            file.path().to_path_buf(),
        )
    })
    .await
    .expect("start cache");
    assert_eq!(
        cache.namespace(namespace!("refresh")).get("ready").await,
        Ok(None)
    );

    *server.observed.auth_reply_delay.lock().expect("delay lock") = Duration::from_millis(5_500);
    std::fs::write(file.path(), ephemeral_password()).expect("changed password");
    server
        .wait_for(
            Duration::from_secs(8),
            "password refresh did not send AUTH",
            || server.command_count("AUTH", None) == 1,
        )
        .await;
    let ping_followed_auth = || {
        let commands = server.observed.commands.lock().expect("commands lock");
        commands
            .iter()
            .map(|(_, args)| args[0].as_str())
            .skip_while(|command| *command != "AUTH")
            .nth(1)
            .map(|command| command == "PING")
    };
    server
        .wait_for(
            Duration::from_secs(8),
            "no maintenance command followed AUTH",
            || ping_followed_auth().is_some(),
        )
        .await;
    assert_eq!(
        ping_followed_auth(),
        Some(true),
        "another refresh overtook PING"
    );
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn reliability_namespace_retains_owner_until_live_maintenance_is_cancelled() {
    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let first = cache.namespace(namespace!("lifetime"));
    let second = cache.namespace(namespace!("other_lifetime"));
    assert_eq!(first.get("ready").await, Ok(None));
    drop(cache);
    assert_eq!(first.get("first-owner").await, Ok(None));
    drop(first);
    assert_eq!(second.get("second-owner").await, Ok(None));
    server.stall_existing();
    let pings = server.command_count("PING", None);
    server
        .wait_for(
            Duration::from_secs(3),
            "live maintenance did not reach the socket",
            || server.command_count("PING", None) > pings,
        )
        .await;
    assert_work_metrics(&metrics, 0, 0, 0);
    drop(second);
    assert_work_metrics(&metrics, 0, 0, 1);
    server
        .wait_for(
            Duration::from_millis(500),
            "last namespace retained cache-owned maintenance",
            || server.active() == 0,
        )
        .await;
    assert_work_metrics(&metrics, 0, 0, 1);
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one retained connection must carry rejection, recovery, file outage and cumulative metric assertions through the same scenario"
)]
async fn reliability_rejected_unchanged_password_recovers_without_traffic() {
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());

    let (captured, _subscriber) = capture_logs();
    let server = FakeServer::start().await;
    let initial = ephemeral_password();
    let pending = ephemeral_password();
    let later = ephemeral_password();
    server.require_password(&initial);
    let file = temp_file(format!("{initial}\n"));
    let mut options = with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    );
    options.command_timeout = Duration::from_secs(1);
    let cache = Cache::start(options).await.expect("start cache");
    let namespace = cache.namespace(namespace!("retry_password"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    assert!(
        password_refresh_samples(&metrics).is_empty(),
        "connection setup is not maintenance"
    );
    *server.observed.auth_reply_delay.lock().expect("delay lock") = Duration::from_millis(900);
    *server
        .observed
        .ping_reply_delays
        .lock()
        .expect("delay lock") = [0, 0, 400, 400, 900].map(Duration::from_millis).into();
    std::fs::write(file.path(), format!("{pending}\n")).expect("pending password");
    server
        .wait_for(
            Duration::from_secs(7),
            "changed password was never attempted",
            || server.observed.auth_rejections.load(Ordering::SeqCst) > 0,
        )
        .await;
    let connections = server.connections();
    assert!(
        password_refresh_samples(&metrics).is_empty(),
        "a pending reply has no completed outcome"
    );
    server.require_password(&pending);
    captured
        .wait_for(Duration::from_secs(7), |logs| {
            logs.contains("cache_password_reloaded")
        })
        .await
        .expect("expected credential diagnostic was not emitted");
    assert_eq!(
        server.connections(),
        connections,
        "the retained-socket recovery bound must not be satisfied by reconnecting"
    );
    assert_eq!(namespace.get("recovered").await, Ok(None));
    assert_eq!(
        password_refresh_samples(&metrics),
        [
            ("auth_accepted".to_owned(), "none".to_owned(), 1),
            ("refresh_failed".to_owned(), "auth".to_owned(), 1),
        ]
    );

    let authenticated = server.observed.auth_successes.load(Ordering::SeqCst);
    let connections = server.connections();
    std::fs::remove_file(file.path()).expect("temporarily unavailable password file");
    captured
        .wait_for(Duration::from_secs(7), |logs| {
            logs.contains("cache_password_file_unreadable")
        })
        .await
        .expect("expected credential diagnostic was not emitted");
    assert_eq!(namespace.get("usable-during-file-outage").await, Ok(None));
    assert!(password_refresh_samples(&metrics).contains(&(
        "read_failed".to_owned(),
        "none".to_owned(),
        1,
    )));
    assert_eq!(
        server.observed.auth_successes.load(Ordering::SeqCst),
        authenticated
    );
    assert_eq!(
        server.connections(),
        connections,
        "unreadable file must preserve the usable authenticated socket"
    );

    server.require_password(&later);
    std::fs::write(file.path(), format!("{later}\r\n")).expect("restore usable password file");
    server
        .wait_for(
            Duration::from_secs(11),
            "file outage ended future credential refresh",
            || server.observed.auth_successes.load(Ordering::SeqCst) > authenticated,
        )
        .await;
    assert_eq!(namespace.get("recovered-after-file-outage").await, Ok(None));
    captured
        .wait_for(Duration::from_secs(2), |logs| {
            logs.matches("cache_password_reloaded").count() == 2
        })
        .await
        .expect("expected credential diagnostic was not emitted");
    server
        .wait_for(
            Duration::from_secs(7),
            "unchanged successful read was not observed",
            || {
                password_refresh_samples(&metrics)
                    .iter()
                    .any(|(outcome, _, _)| outcome == "unchanged")
            },
        )
        .await;
    assert_eq!(
        password_refresh_samples(&metrics),
        [
            ("auth_accepted".to_owned(), "none".to_owned(), 2),
            ("read_failed".to_owned(), "none".to_owned(), 1),
            ("refresh_failed".to_owned(), "auth".to_owned(), 1),
            ("unchanged".to_owned(), "none".to_owned(), 1),
        ]
    );
    let debug = format!("{:?}", metrics.snapshot());
    for secret in [
        initial.as_str(),
        pending.as_str(),
        later.as_str(),
        file.path().to_str().expect("UTF-8 fixture path"),
    ] {
        assert!(!debug.contains(secret));
    }
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn a_password_the_server_keeps_refusing_is_reported_once() {
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let (captured, _subscriber) = capture_logs();
    let server = FakeServer::start().await;
    let initial = ephemeral_password();
    let refused = ephemeral_password();
    server.require_password(&initial);
    let file = temp_file(format!("{initial}\n"));
    let cache = Cache::start(with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    ))
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("refused_password"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    std::fs::write(file.path(), format!("{refused}\n")).expect("refused password");
    server
        .wait_for(
            Duration::from_secs(12),
            "the refused password was not retried",
            || {
                password_refresh_samples(&metrics).contains(&(
                    "refresh_failed".to_owned(),
                    "auth".to_owned(),
                    2,
                ))
            },
        )
        .await;
    assert_eq!(
        captured
            .rendered()
            .matches("cache_password_refresh_failed")
            .count(),
        1
    );
    assert_eq!(namespace.get("still-usable").await, Ok(None));
}

#[test]
fn a_stalled_blocking_pool_bounds_startup_and_keeps_one_pending_password_read() {
    let password = temp_file("admitted\n");
    let file = crate::credentials::PasswordFile::admit(password.path().to_owned(), None)
        .expect("admit password file");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let blocker = tokio::task::spawn_blocking(move || gate.blocking_recv());
        let options = with_password_file("redis://127.0.0.1:6379", password.path().to_owned());
        let error = tokio::time::timeout(Duration::from_secs(7), Cache::start(options))
            .await
            .expect("admission must time out while the blocking worker is unavailable")
            .unwrap_err();
        assert_eq!(error, CacheError::AdmissionTimedOut);

        {
            let mut read = std::pin::pin!(file.read());
            assert!(futures_util::poll!(read.as_mut()).is_pending());
        }
        std::assert_matches!(
            futures_util::poll!(std::pin::pin!(file.read())),
            std::task::Poll::Ready(Err(crate::credentials::ReadFailure::Busy))
        );
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        assert_eq!(file.read().await.unwrap().as_str(), "admitted");
    });
}

#[cfg(unix)]
#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned credential files are rotated atomically"
)]
fn replacing_an_unserviced_fifo_restores_password_refresh_and_reconnect() {
    use nix::sys::stat::Mode;

    super::admission::with_unserviced_fifo(async {
        for reconnect in [false, true] {
            let (captured, _subscriber) = capture_logs();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("password");
            let replacement = directory.path().join("replacement");
            let initial = ephemeral_password();
            let rotated = ephemeral_password();
            let server = FakeServer::start().await;
            server.require_password(&initial);
            std::fs::write(&path, initial).unwrap();
            let cache = Cache::start(with_password_file(
                &format!("redis://{}", server.address),
                path.clone(),
            ))
            .await
            .unwrap();
            let namespace = cache.namespace(namespace!("recover_password_file"));
            assert_eq!(namespace.get("ready").await, Ok(None));
            nix::unistd::mkfifo(&replacement, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            if reconnect {
                server.hang_up();
            }
            captured
                .wait_for(Duration::from_secs(7), |logs| {
                    if reconnect {
                        connection_failed_with(logs, "other")
                    } else {
                        logs.contains("cache_password_file_unreadable")
                    }
                })
                .await
                .expect("the cache must observe the unusable FIFO");
            if !reconnect {
                assert_eq!(namespace.get("during-outage").await, Ok(None));
            }

            server.require_password(&rotated);
            std::fs::write(&replacement, rotated).unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            server
                .wait_for(
                    Duration::from_secs(7),
                    "replacement password was not accepted",
                    || server.observed.auth_successes.load(Ordering::SeqCst) == 2,
                )
                .await;
            wait_until_connected(&cache, Duration::from_secs(1)).await;
            assert_eq!(namespace.get("recovered").await, Ok(None));
            assert_eq!(server.connections(), if reconnect { 2 } else { 1 });
        }
    });
}

fn password_refresh_samples(metrics: &TestMetrics) -> Vec<(String, String, u64)> {
    let mut samples: Vec<_> = metrics
        .snapshot()
        .into_iter()
        .filter(|(composite, _)| {
            composite.key().name() == crate::observe::PASSWORD_REFRESHES_METRIC
        })
        .map(|(composite, value)| {
            let DebugValue::Counter(count) = value else {
                panic!("unexpected refresh metric value: {value:?}");
            };
            let label = |name: &str| {
                composite
                    .key()
                    .labels()
                    .find_map(|label| (label.key() == name).then(|| label.value().to_owned()))
                    .unwrap_or_else(|| panic!("missing {name} label on {}", composite.key()))
            };
            (label("outcome"), label("reason"), count)
        })
        .collect();
    samples.sort_unstable();
    samples
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned credential replacement completes before refresh observes it"
)]
async fn password_refresh_timeout_is_observed_but_cancelled_work_is_not() {
    for cancel in [false, true] {
        let metrics = TestMetrics::new();
        let _recorder = metrics::set_default_local_recorder(metrics.recorder());
        let server = FakeServer::start().await;
        let initial = ephemeral_password();
        let replacement = ephemeral_password();
        server.require_password(&initial);
        let file = temp_file(&initial);
        let cache = Cache::start(CacheOptions {
            command_timeout: Duration::from_secs(1),
            ..with_password_file(
                &format!("redis://{}", server.address),
                file.path().to_path_buf(),
            )
        })
        .await
        .expect("start cache");
        assert_eq!(
            cache
                .namespace(namespace!("refresh_completion"))
                .get("ready")
                .await,
            Ok(None)
        );
        let old = server.connections();
        server.require_password(&replacement);
        server.observed.silence_auth.store(true, Ordering::SeqCst);
        std::fs::write(file.path(), &replacement).expect("replacement password");
        server
            .wait_for(
                Duration::from_secs(7),
                "refresh AUTH was not dispatched",
                || server.command_count("AUTH", None) == 1,
            )
            .await;
        assert!(
            password_refresh_samples(&metrics).is_empty(),
            "sending AUTH does not complete authentication"
        );
        if cancel {
            drop(cache);
        }
        server
            .wait_for(
                Duration::from_secs(if cancel { 2 } else { 6 }),
                "unfinished refresh did not retire its connection",
                || server.closed_through(old),
            )
            .await;
        let samples = password_refresh_samples(&metrics);
        if cancel {
            assert!(
                samples.is_empty(),
                "cancellation is not a completed refresh"
            );
        } else {
            assert_eq!(
                samples,
                [("refresh_failed".to_owned(), "timeout".to_owned(), 1)]
            );
        }
    }
}

#[tokio::test]
async fn reliability_password_file_preserves_case_crlf_and_username() {
    for user in [None, Some("ServiceUser")] {
        let server = FakeServer::start().await;
        let password = format!(" MiXeD-{} ", ephemeral_password());
        server.require_password(&password);
        *server.observed.username.lock().expect("username lock") =
            Some(user.unwrap_or("default").to_owned());
        let file = temp_file(format!("{password}\r\n"));
        let authority = user.map_or_else(
            || server.address.to_string(),
            |user| format!("{user}@{}", server.address),
        );
        let cache = Cache::start(with_password_file(
            &format!("redis://{authority}"),
            file.path().to_path_buf(),
        ))
        .await
        .expect("start cache");
        assert_eq!(
            cache
                .namespace(namespace!("credential_bytes"))
                .get("ready")
                .await,
            Ok(None),
            "AUTH must preserve password case and spaces, remove one CRLF, and select the correct user"
        );
    }
}

#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: tokio::sync::watch::Sender<Vec<u8>>,
}

impl CapturedLogs {
    fn rendered(&self) -> String {
        String::from_utf8(self.bytes.borrow().clone()).expect("UTF-8 logs")
    }

    async fn wait_for(
        &self,
        budget: Duration,
        ready: impl Fn(&str) -> bool,
    ) -> Result<(), tokio::time::error::Elapsed> {
        tokio::time::timeout(
            budget,
            self.bytes
                .subscribe()
                .wait_for(|bytes| ready(std::str::from_utf8(bytes).expect("UTF-8 logs"))),
        )
        .await?
        .expect("capture owns the log sender");
        Ok(())
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.send_modify(|logs| logs.extend_from_slice(bytes));
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn reliability_auth_errors_are_sanitized_through_the_dependency_log_bridge() {
    static BRIDGE: std::sync::Once = std::sync::Once::new();
    BRIDGE.call_once(|| {
        tracing_log::LogTracer::init().expect("install actual dependency log bridge");
    });
    let captured = CapturedLogs::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    tracing_log::log::warn!("cache-test-log-bridge-control");
    assert!(
        captured
            .rendered()
            .contains("cache-test-log-bridge-control"),
        "the real log bridge must be active"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let server = FakeServer::start().await;
        let marker = "raw-server-auth-marker-9e3b7a";
        let accepted = ephemeral_password();
        let rejected = ephemeral_password();
        *server.observed.auth_error.lock().expect("auth error lock") =
            format!("{marker} {rejected}");
        server.require_password(&accepted);
        let file = temp_file(format!("{accepted}\n"));
        let dsn = format!("redis://{}", server.address);
        let cache = Cache::start(with_password_file(&dsn, file.path().to_path_buf()))
            .await
            .expect("start cache");
        let namespace = cache.namespace(namespace!("logs"));
        assert_eq!(namespace.get("sensitive-key").await, Ok(None));
        std::fs::write(file.path(), format!("{rejected}\n")).expect("rejected password");
        captured
            .wait_for(Duration::from_secs(7), |logs| {
                logs.contains("Failed to re-authenticate")
                    || logs.contains("error.type=\"auth\"")
                    || logs.contains("error.type=auth")
            })
            .await
            .expect("AUTH rejection must emit a bounded failure classification");
        let rendered = format!("{} {cache:?} {namespace:?}", captured.rendered());
        for (category, secret) in [
            ("raw server text", marker),
            ("accepted credential", accepted.as_str()),
            ("rejected credential", rejected.as_str()),
            ("cache key", "sensitive-key"),
            ("DSN", dsn.as_str()),
        ] {
            assert!(
                !rendered.contains(secret),
                "dependency diagnostics disclosed {category}"
            );
        }
        assert!(
            !rendered.contains("cache_password_reloaded"),
            "reading a rejected credential must not report authenticated acceptance"
        );
    });
}

#[tokio::test]
async fn caller_timeout_after_setup_wait_keeps_the_healthy_connection() {
    let server = FakeServer::start().await;
    *server
        .observed
        .hello_reply_delay
        .lock()
        .expect("delay lock") = Duration::from_millis(250);
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_millis(200);
    let metrics = TestMetrics::new();
    let _recorder = metrics::set_default_local_recorder(metrics.recorder());
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_millis(350),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("setup_wait"));

    let parent = operation_context::OperationContext::with_timeout(Duration::from_secs(5));
    assert_eq!(
        namespace.get_with_context("slow", &parent).await,
        Err(OperationError::Unavailable)
    );
    assert_eq!(parent.check(), Ok(()));
    assert_eq!(
        server.command_count("GET", Some("setup_wait:slow")),
        1,
        "command must dispatch after setup before exhausting the caller deadline"
    );
    assert_work_metrics(&metrics, 0, 0, 0);
    assert_eq!(namespace.get("ready").await, Ok(None));
    assert_work_metrics(&metrics, 0, 0, 0);
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn commands_fail_fast_while_a_background_setup_retry_is_in_flight() {
    use std::task::Poll;

    let (captured, _subscriber) = capture_logs();
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(
            &format!("redis://:secret@{}", server.address),
            true,
            false,
            None,
        )
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("retry_fail_fast"));

    captured
        .wait_for(Duration::from_secs(2), |logs| {
            connection_failed_with(logs, "auth")
        })
        .await
        .expect("initial setup attempt must be refused");
    server.observed.silence_all.store(true, Ordering::SeqCst);
    server
        .wait_for(
            Duration::from_secs(2),
            "retry setup attempt did not reach the silenced server",
            || server.command_count("HELLO", None) >= 2,
        )
        .await;

    let mut command = Box::pin(namespace.get("key"));
    assert_eq!(
        futures_util::poll!(command.as_mut()),
        Poll::Ready(Err(OperationError::Unavailable))
    );
    assert_eq!(server.command_count("GET", None), 0);
}

#[tokio::test]
async fn commands_fail_fast_during_generation_cooldown_after_early_disconnect() {
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start cache");
    let namespace = cache.namespace(namespace!("cooldown"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    server.hang_up();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while namespace.get("probe").await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "disconnect during cooldown did not transition to fast failure"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let started = tokio::time::Instant::now();
    assert_eq!(namespace.get("key").await, Err(OperationError::Unavailable));
    assert!(
        started.elapsed() < Duration::from_millis(50),
        "command blocked during generation cooldown instead of failing fast"
    );
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains(
            "cache_operation_duration_seconds_count{cache=\"cooldown\",operation=\"get\",outcome=\"error\",error_type=\"io\"}"
        ),
        "{scrape}"
    );
}

#[tokio::test]
async fn every_published_connection_is_reported_as_connected() {
    let (captured, _subscriber) = capture_logs();
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("announce"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    let connected = |logs: &str| {
        logs.lines()
            .filter(|line| line.contains("cache_connected"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    captured
        .wait_for(Duration::from_secs(1), |logs| connected(logs).len() == 1)
        .await
        .expect("the first connection was not reported");
    let first = connected(&captured.rendered());
    assert!(first[0].contains("server.address=127.0.0.1"), "{first:?}");
    assert!(
        first[0].contains(&format!("server.port={}", server.address.port())),
        "{first:?}"
    );
    assert!(first[0].contains("cache.tls=false"), "{first:?}");

    server.hang_up();
    captured
        .wait_for(Duration::from_secs(5), |logs| connected(logs).len() == 2)
        .await
        .expect("the reconnection was not reported");
    assert_eq!(server.connections(), 2);
}
