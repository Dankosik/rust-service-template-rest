use std::sync::atomic::Ordering;

use metrics::{Key, Level, Metadata, Recorder as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};

use super::*;

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
    .expect("start connect");
    let namespace = cache.namespace(namespace!("auth"));

    let chain = crate::connection::SETUP_BACKOFF_MAX_RETRIES + 1;
    gate.wait_for(
        Duration::from_secs(20),
        "setup retry chain did not progress without traffic",
        || gate.attempts() >= chain,
    )
    .await;
    gate.accept_auth.store(true, Ordering::SeqCst);
    gate.wait_for(
        Duration::from_secs(5),
        "authentication did not recover without traffic after the first setup chain",
        || gate.observed.auth_successes.load(Ordering::SeqCst) > 0,
    )
    .await;
    assert_eq!(namespace.get("key").await, Ok(None));
}

#[test]
fn late_reconnect_delays_spread_below_the_ceiling_instead_of_pinning_to_it() {
    let chain = crate::connection::SETUP_BACKOFF_MAX_RETRIES + 1;
    let mut backoff = crate::connection::SetupBackoff::new();
    let delays: Vec<Duration> = (0..20 * chain).map(|_| backoff.next_delay()).collect();

    let ceiling = Duration::from_secs(2);
    assert!(
        delays
            .iter()
            .all(|delay| (Duration::from_millis(100)..=ceiling).contains(delay)),
        "{delays:?}"
    );
    let pauses: Vec<Duration> = delays
        .iter()
        .copied()
        .skip(chain - 1)
        .step_by(chain)
        .collect();
    assert!(
        pauses.iter().all(|pause| *pause >= Duration::from_secs(1)),
        "a chain must end in a pause: {pauses:?}"
    );
    let late: Vec<Duration> = delays
        .iter()
        .copied()
        .filter(|delay| *delay >= Duration::from_secs(1))
        .collect();
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
    .expect("start connect");

    let namespace = cache.namespace(namespace!("replies"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    assert_eq!(namespace.get("wrongtype").await, Err(crate::Unavailable));
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
        Err(crate::SetError::Unavailable(crate::Unavailable))
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
async fn a_readonly_reply_after_cancellation_reconnects_to_the_new_primary() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(15),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
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
        Ok(Err(crate::SetError::Unavailable(crate::Unavailable)))
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
    assert_eq!(
        tokio::time::timeout(limit, namespace.set("new", b"value", ttl)).await,
        Ok(Ok(()))
    );
    assert_eq!(
        server.command_count("SET", Some("late_readonly:cancelled")),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_hello_is_reported_as_auth() {
    use health::Probe;

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
    .expect("start connect");

    let refused = tokio::time::timeout(Duration::from_secs(20), cache.probe().check())
        .await
        .expect("the reconnect chain ends")
        .expect_err("the server refuses the password");
    assert_eq!(refused.to_string(), "cache ping failed: auth");
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn a_password_file_unreadable_at_setup_is_not_reported_as_auth() {
    use health::Probe;

    let server = FakeServer::start().await;
    let password = ephemeral_password();
    server.require_password(&password);
    let file = temp_file(&password);
    let cache = Cache::start(with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    ))
    .expect("start cache");
    std::fs::remove_file(file.path()).expect("remove password file");

    let refused = tokio::time::timeout(Duration::from_secs(5), cache.probe().check())
        .await
        .expect("the setup attempt ends")
        .expect_err("setup cannot read the password");
    assert_eq!(refused.to_string(), "cache ping failed: other");
    assert_eq!(server.command_count("HELLO", None), 0);
}

#[tokio::test]
async fn caller_cancellation_records_cancelled_and_an_expired_deadline_timeout() {
    use operation_context::OperationContext;

    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true);
    let namespace = cache.namespace(namespace!("stop"));
    assert_eq!(namespace.get("ready").await, Ok(None));

    let cancelled = OperationContext::with_timeout(Duration::from_secs(1));
    cancelled.cancel();
    assert_eq!(
        namespace.get_with_context("key", &cancelled).await,
        Err(crate::Unavailable)
    );
    let expired = OperationContext::with_timeout(Duration::ZERO);
    assert_eq!(
        namespace.get_with_context("key", &expired).await,
        Err(crate::Unavailable)
    );

    let scrape = recorder.handle().render();
    for series in [
        "cache_operation_duration_seconds_count{cache=\"stop\",operation=\"get\",outcome=\"cancelled\"} 1",
        "cache_operation_duration_seconds_count{cache=\"stop\",operation=\"get\",outcome=\"timeout\",error_type=\"timeout\"} 1",
    ] {
        assert!(scrape.contains(series), "{series} missing from {scrape}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_reply_within_command_timeout_survives_the_maintenance_ping() {
    let server = FakeServer::start().await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_millis(7_500);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(9),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("budget"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();

    assert_eq!(namespace.get("slow").await, Ok(None));
    assert!(server.command_count("PING", None) > 0, "no PING was queued");
    assert_eq!(server.connections(), connections);
}

#[tokio::test]
async fn a_large_command_timeout_keeps_probes_and_maintenance_usable() {
    use health::Probe;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::MAX,
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("large_budget"));
    let probe = cache.probe();
    let limit = Duration::from_secs(5);
    tokio::time::timeout(limit, probe.check())
        .await
        .expect("probe must complete")
        .expect("probe must succeed");

    server
        .wait_for(limit, "maintenance must send its own PING", || {
            server.command_count("PING", None) >= 2
        })
        .await;
    assert_eq!(
        tokio::time::timeout(limit, namespace.get("ready")).await,
        Ok(Ok(None))
    );
}

#[tokio::test]
async fn ping_permission_denial_keeps_the_connection_usable() {
    use health::Probe;

    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true);
    let namespace = cache.namespace(namespace!("ping_permission"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    let pings = server.command_count("PING", None);
    server.observed.deny_ping.store(true, Ordering::SeqCst);

    assert!(cache.probe().check().await.is_err());
    assert_eq!(namespace.get("after_probe").await, Ok(None));
    server
        .wait_for(
            Duration::from_secs(7),
            "maintenance PING did not recur",
            || server.command_count("PING", None) >= pings + 3,
        )
        .await;
    assert_eq!(namespace.get("after_maintenance").await, Ok(None));
    assert_eq!(server.connections(), connections);
}

#[tokio::test]
async fn a_slow_command_survives_an_overlapping_readiness_probe() {
    use health::Probe;

    let server = FakeServer::start().await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_secs(7);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(10),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("probe_budget"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let probe = cache.probe();

    let (command, readiness) = tokio::join!(namespace.get("slow"), async {
        server
            .wait_for(
                Duration::from_secs(1),
                "slow GET did not reach the server",
                || server.command_count("GET", Some("probe_budget:slow")) == 1,
            )
            .await;
        probe.check().await
    });
    assert_eq!(command, Ok(None));
    assert!(readiness.is_ok(), "{readiness:?}");
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
    let cache = admitted(&format!("redis://{}", server.address), true, true);
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
    assert_eq!(namespace.get("key").await, Ok(None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_connection_dials_without_waiting_for_a_call() {
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true);
    let namespace = cache.namespace(namespace!("warm"));
    let ttl = Duration::from_secs(1);
    namespace
        .set("key", b"value", ttl)
        .await
        .expect("set on the current primary");
    server.primary.fetch_add(1, Ordering::SeqCst);
    assert_eq!(
        namespace.set("key", b"value", ttl).await,
        Err(crate::SetError::Unavailable(crate::Unavailable))
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
    let _cache = admitted(&format!("redis://{}", server.address), true, true);

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
    use health::Probe;
    use std::task::Poll;

    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let cache = metrics::with_local_recorder(metrics.recorder(), || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    assert_work_metrics(&metrics, 0, 0, 0);
    let namespace = cache.namespace(namespace!("waiting"));
    let mut waiting = Vec::new();
    for _ in 0..256 {
        let mut command = Box::pin(namespace.get("pending"));
        assert!(futures_util::poll!(command.as_mut()).is_pending());
        waiting.push(command);
    }
    assert_work_metrics(&metrics, 256, 0, 0);
    let other = metrics::with_local_recorder(metrics.recorder(), || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    assert_work_metrics(&metrics, 256, 0, 0);
    drop(other);
    let mut excess = Box::pin(namespace.get("refused"));
    assert_eq!(
        futures_util::poll!(excess.as_mut()),
        Poll::Ready(Err(crate::Unavailable))
    );
    assert_work_metrics(&metrics, 256, 1, 0);
    let probe = cache.probe();
    let mut waiting_probe = probe.check();
    assert!(futures_util::poll!(waiting_probe.as_mut()).is_pending());
    let mut excess_probe = probe.check();
    std::assert_matches!(
        futures_util::poll!(excess_probe.as_mut()),
        Poll::Ready(Err(_))
    );
    assert_work_metrics(&metrics, 257, 2, 0);
    drop(waiting_probe);
    drop(waiting.pop());
    assert_work_metrics(&metrics, 255, 2, 0);
    let mut replacement = Box::pin(namespace.get("replacement"));
    assert!(futures_util::poll!(replacement.as_mut()).is_pending());
    drop(waiting);
    assert_work_metrics(&metrics, 1, 2, 0);
    assert_eq!(replacement.await, Ok(None));
    assert_work_metrics(&metrics, 0, 2, 0);
    probe
        .check()
        .await
        .expect("setup and probe capacity survive acquisition cancellation");
    assert_work_metrics(&metrics, 0, 2, 0);
    assert_eq!(server.connections(), 1);
    assert_eq!(server.command_count("GET", Some("waiting:pending")), 0);
    assert_eq!(server.command_count("GET", Some("waiting:refused")), 0);
}

#[tokio::test]
async fn reliability_full_application_window_leaves_probe_and_maintenance_admission() {
    use health::Probe;
    use std::task::Poll;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(5),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("capacity"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
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
        Poll::Ready(Err(crate::Unavailable))
    );
    let probe = cache.probe();
    let pings = server.command_count("PING", None);
    let mut pending_probe = probe.check();
    tokio::select! {
        result = &mut pending_probe => panic!("silent probe completed: {result:?}"),
        () = server.wait_for(
            Duration::from_secs(1),
            "application saturation blocked the external probe",
            || server.command_count("PING", None) > pings,
        ) => {}
    }
    let mut excess_probe = probe.check();
    std::assert_matches!(
        futures_util::poll!(excess_probe.as_mut()),
        Poll::Ready(Err(error)) if error.to_string() == "cache ping failed: other"
    );
    server
        .wait_for(
            Duration::from_secs(3),
            "maintenance queued behind admission",
            || server.command_count("PING", None) == pings + 2,
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
    drop(pending_probe);
    drop(pending);
    server
        .wait_for(Duration::from_secs(1), "old socket stayed open", || {
            server.closed_through(old)
        })
        .await;
    assert_eq!(namespace.get("recovered").await, Ok(None));
    probe.check().await.expect("probe capacity is reusable");
}

#[tokio::test]
async fn reliability_stalled_generations_recover_without_replaying_writes() {
    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let cache = metrics::with_local_recorder(metrics.recorder(), || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
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
            Err(crate::SetError::Unavailable(crate::Unavailable))
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "SET exceeded its 100 ms budget plus scheduling allowance"
        );
        assert_work_metrics(&metrics, 0, 0, cycle + 1);
        server
            .wait_for(
                Duration::from_secs(4),
                "stalled generation was not replaced and released",
                || server.connections() > old && server.closed_through(old),
            )
            .await;
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
        Err(crate::Unavailable)
    );
    assert_work_metrics(&metrics, 0, 0, 4);
    server
        .wait_for(
            Duration::from_secs(4),
            "DEL timeout did not recover",
            || server.connections() > old && server.closed_through(old),
        )
        .await;
    assert_eq!(namespace.get("ready").await, Ok(None));
    assert_eq!(
        server.command_count("DEL", Some("stalled:delete-once")),
        1,
        "effect-ambiguous DEL was replayed"
    );
}

#[tokio::test]
async fn reliability_a_probe_reply_timeout_retires_the_connection() {
    use health::Probe;
    use std::task::Poll;

    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let cache = metrics::with_local_recorder(metrics.recorder(), || {
        Cache::start(CacheOptions {
            command_timeout: Duration::from_secs(5),
            ..options(&format!("redis://{}", server.address), true, true, None)
        })
        .expect("start cache")
    });
    let namespace = cache.namespace(namespace!("probe_timeout"));
    let probe = cache.probe();
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
    let before = server.command_count("PING", None);
    let mut pending_probe = probe.check();
    tokio::select! {
        biased;
        result = &mut pending_probe => panic!("silent probe completed before dispatch: {result:?}"),
        () = server.wait_for(
            Duration::from_secs(1),
            "probe did not reach the established socket",
            || server.command_count("PING", None) > before,
        ) => {}
    }
    assert_work_metrics(&metrics, 1, 0, 0);
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
            || server.command_count("GET", Some(&format!("probe_timeout:{late_key}"))) == 1
                && server.command_count("GET", Some(&format!("probe_timeout:{peer_key}"))) == 1,
        ) => {}
    }
    assert_work_metrics(&metrics, 3, 0, 0);
    let error = tokio::time::timeout(Duration::from_secs(6), pending_probe)
        .await
        .expect("probe must observe its own reply timeout")
        .expect_err("the stalled connection did not answer PING");
    assert_eq!(error.to_string(), "cache ping failed: timeout");
    assert_work_metrics(&metrics, 2, 0, 1);
    assert_eq!(
        futures_util::poll!(peer.as_mut()),
        Poll::Ready(Err(crate::Unavailable))
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
        Poll::Ready(Err(crate::Unavailable))
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
    probe.check().await.expect("healthy probe after timeout");
}

#[tokio::test]
async fn a_caller_stop_before_the_ceiling_keeps_the_connection_for_peers() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_millis(300);
    let metrics = TestMetrics::new();
    let cache = metrics::with_local_recorder(metrics.recorder(), || {
        Cache::start(CacheOptions {
            command_timeout: Duration::from_secs(5),
            ..options(&format!("redis://{}", server.address), true, true, None)
        })
        .expect("start cache")
    });
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
    assert_eq!(stopped, Err(crate::Unavailable));
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
    let cache = admitted(&format!("redis://{}", server.address), true, true);
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
    let cache = admitted(&format!("redis://{}", server.address), true, true);
    let namespace = cache.namespace(namespace!("context"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let connections = server.connections();
    for expired in [false, true] {
        let context = OperationContext::with_timeout(if expired {
            Duration::ZERO
        } else {
            Duration::from_secs(1)
        });
        if !expired {
            context.cancel();
        }
        assert_eq!(
            namespace.get_with_context("stopped", &context).await,
            Err(crate::Unavailable)
        );
        assert_eq!(
            namespace
                .set_with_context("stopped", b"value", Duration::from_secs(1), &context)
                .await,
            Err(crate::SetError::Unavailable(crate::Unavailable))
        );
        assert_eq!(
            namespace.delete_with_context("stopped", &context).await,
            Err(crate::Unavailable)
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
async fn dropping_a_command_or_probe_before_dispatch_keeps_the_connection() {
    use health::Probe;
    use tokio::task::coop::{consume_budget, has_budget_remaining};

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("yield_before_send"));
    let probe = cache.probe();
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
        {
            let mut stopped = probe.check();
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
        Err(crate::Unavailable)
    );
    assert_eq!(server.command_count("GET", Some("acquire:pending")), 0);
}

#[tokio::test]
async fn acknowledged_write_keeps_its_result_after_disconnect() {
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(10),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("acknowledged_write"));
    assert_eq!(namespace.get("ready").await, Ok(None));
    let write = namespace.set("key", b"value", Duration::from_secs(10));
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
    assert_eq!(read.await, Err(crate::Unavailable));
    assert_eq!(namespace.get("live").await, Ok(None));
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn dropping_an_exchange_with_a_delivered_reply_keeps_the_connection() {
    use health::Probe;

    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(2),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("start cache");
    let namespace = cache.namespace(namespace!("drop_ready_reply"));
    let probe = cache.probe();
    assert_eq!(namespace.get("ready").await, Ok(None));

    for check_probe in [false, true] {
        let pings = server.command_count("PING", None);
        {
            let exchange = async {
                if check_probe {
                    probe.check().await.expect("PING would succeed");
                } else {
                    assert_eq!(namespace.get("discarded").await, Ok(None));
                }
            };
            tokio::pin!(exchange);
            assert!(futures_util::poll!(exchange.as_mut()).is_pending());
            assert_eq!(namespace.get("fence").await, Ok(None));
            if check_probe {
                assert!(server.command_count("PING", None) > pings);
            } else {
                assert_eq!(
                    server.command_count("GET", Some("drop_ready_reply:discarded")),
                    1
                );
            }
        }
        assert_eq!(namespace.get("live").await, Ok(None));
        assert_eq!(server.connections(), 1);
    }
}

#[test]
fn probe_reports_the_exit_of_a_supervisor_that_never_ran() {
    use health::Probe;

    let cache = admitted("redis://127.0.0.1:6379", true, true);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("probe runtime");
    runtime.block_on(async {
        let error = tokio::time::timeout(Duration::from_secs(1), cache.probe().check())
            .await
            .expect("probe must observe the supervisor exit")
            .expect_err("the supervisor cannot dial");
        assert_eq!(error.to_string(), "cache ping failed: io");
    });
}

#[tokio::test]
async fn cancellation_after_write_dispatch_leaves_a_stalled_server_to_maintenance_without_replay() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let cache = metrics::with_local_recorder(metrics.recorder(), || {
        Cache::start(CacheOptions {
            command_timeout: Duration::from_secs(1),
            ..options(&format!("redis://{}", server.address), true, true, None)
        })
        .expect("cache")
    });
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
        Err(crate::SetError::Unavailable(crate::Unavailable))
    );
    assert_work_metrics(&metrics, 0, 0, 0);
    server
        .wait_for(
            Duration::from_secs(5),
            "maintenance must replace the stalled generation",
            || server.connections() > old && server.closed_through(old),
        )
        .await;
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
async fn reliability_namespace_and_probe_retain_owner_until_live_maintenance_is_cancelled() {
    use health::Probe;

    let server = FakeServer::start().await;
    let metrics = TestMetrics::new();
    let cache = metrics::with_local_recorder(metrics.recorder(), || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    let namespace = cache.namespace(namespace!("lifetime"));
    let probe = cache.probe();
    assert_eq!(namespace.get("ready").await, Ok(None));
    drop(cache);
    assert_eq!(namespace.get("namespace-owner").await, Ok(None));
    drop(namespace);
    probe.check().await.expect("healthy probe");
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
    drop(probe);
    assert_work_metrics(&metrics, 0, 0, 1);
    server
        .wait_for(
            Duration::from_millis(500),
            "last probe retained cache-owned maintenance",
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

    let captured = CapturedLogs::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
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
    let cache = Cache::start(options).expect("start cache");
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
    let captured = CapturedLogs::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let server = FakeServer::start().await;
    let initial = ephemeral_password();
    let refused = ephemeral_password();
    server.require_password(&initial);
    let file = temp_file(format!("{initial}\n"));
    let cache = Cache::start(with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    ))
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

#[cfg(unix)]
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "test-owned password fixtures; the FIFO writer runs on an owned blocking thread"
)]
async fn a_hung_password_read_is_never_joined_by_a_second() {
    let directory = tempfile::tempdir().expect("password directory");
    let path = directory.path().join("password");
    std::fs::write(&path, "admitted\n").expect("admitted password");
    let file =
        crate::credentials::PasswordFile::admit(path.clone(), None).expect("admit password file");
    std::fs::remove_file(&path).expect("remove password");
    let status = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .expect("run mkfifo");
    assert!(status.success());

    assert!(
        tokio::time::timeout(Duration::from_millis(50), file.read())
            .await
            .is_err(),
        "the FIFO read must hang"
    );
    let second = tokio::time::timeout(Duration::from_secs(1), file.read()).await;

    let fifo = directory.path().join("stale");
    std::fs::rename(&path, &fifo).expect("move FIFO aside");
    std::fs::write(&path, "rotated\n").expect("rotated password");
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .open(fifo)
            .and_then(|mut writer| writer.write_all(b"stale"))
    })
    .await
    .expect("FIFO writer task")
    .expect("release the hung read");
    assert_eq!(
        second,
        Ok(Err(crate::credentials::ReadFailure::Busy)),
        "a second read must fail at once instead of starting another"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match file.read().await {
            Ok(password) => {
                assert_eq!(password.as_str(), "rotated");
                break;
            }
            Err(error) => assert!(
                tokio::time::Instant::now() < deadline,
                "the released read never finished: {error}"
            ),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
        let cache = Cache::start(with_password_file(
            &format!("redis://{}", server.address),
            file.path().to_path_buf(),
        ))
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
        let cache =
            Cache::start(with_password_file(&dsn, file.path().to_path_buf())).expect("start cache");
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
