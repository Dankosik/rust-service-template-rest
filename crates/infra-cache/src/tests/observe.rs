#![warn(clippy::let_underscore_must_use)]

use super::*;
use operation_context::OperationContext;

#[test]
fn redis_errors_map_to_bounded_error_types() {
    let cases = [
        (redis::ErrorKind::Io, "io"),
        (redis::ErrorKind::AuthenticationFailed, "auth"),
        (redis::ErrorKind::Parse, "parse"),
        (redis::ErrorKind::InvalidClientConfig, "other"),
        (redis::ErrorKind::Client, "other"),
    ];
    for (kind, expected) in cases {
        let error = redis::RedisError::from((kind, "hunter2 must not leak"));
        assert_eq!(crate::error::error_type(&error).label(), expected);
    }
    for kind in [std::io::ErrorKind::TimedOut, std::io::ErrorKind::WouldBlock] {
        let timed_out = redis::RedisError::from(std::io::Error::from(kind));
        assert_eq!(crate::error::error_type(&timed_out).label(), "timeout");
    }
    let server = redis::RedisError::from((
        redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError),
        "NOAUTH hunter2",
    ));
    assert_eq!(crate::error::error_type(&server).label(), "response");
}

#[test]
fn a_failed_command_names_its_cause_on_the_series() {
    use crate::error::ErrorType;
    use crate::observe::{NamespaceMetrics, Operation, OperationGuard};

    let recorder = observation_recorder();
    let server = crate::ServerEndpoint {
        host: "cache.example".to_owned(),
        port: 6379,
        database_index: 0,
    };
    metrics::with_local_recorder(&recorder, || {
        let metrics = NamespaceMetrics::new(crate::namespace!("causes"));
        for operation in [Operation::Get, Operation::Set, Operation::Delete] {
            for error_type in [
                ErrorType::Auth,
                ErrorType::Io,
                ErrorType::Timeout,
                ErrorType::ServerErrorReply,
                ErrorType::Parse,
                ErrorType::Other,
            ] {
                OperationGuard::start(&metrics, operation, &server).record_error(error_type, None);
            }
        }
    });
    let scrape = recorder.handle().render();
    for operation in ["get", "set", "delete"] {
        for (outcome, error_type) in [
            ("error", "auth"),
            ("error", "io"),
            ("timeout", "timeout"),
            ("error", "response"),
            ("error", "parse"),
            ("error", "other"),
        ] {
            let series = format!(
                "cache_operation_duration_seconds_count{{cache=\"causes\",operation=\"{operation}\",outcome=\"{outcome}\",error_type=\"{error_type}\"}} 1"
            );
            assert!(scrape.contains(&series), "{series} missing from {scrape}");
        }
    }
}

#[tokio::test]
async fn spans_identify_the_redis_transaction_and_known_mutation_outcomes() {
    use futures_util::FutureExt;
    use operation_context::Stopped;
    use tracing_subscriber::fmt::format::FmtSpan;

    use crate::observe::{NamespaceMetrics, Operation, OperationGuard, SuccessOutcome};
    use crate::{MutationError, MutationFailure, UnknownOutcome};

    let disabled_metrics = NamespaceMetrics::new(namespace!("disabled"));
    let disabled_server = crate::ServerEndpoint {
        host: "cache.example".to_owned(),
        port: 6379,
        database_index: 0,
    };
    OperationGuard::start(&disabled_metrics, Operation::Get, &disabled_server)
        .succeed(SuccessOutcome::Miss);

    let captured = super::connection::CapturedLogs::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(move || writer.clone())
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let server = FakeServer::start().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(10),
        ..options(&format!("redis://{}/7", server.address), true, true, None)
    })
    .await
    .unwrap();
    let namespace = cache.namespace(namespace!("items"));
    let caller = OperationContext::unbounded();
    assert_eq!(namespace.get(&caller, "secret-key").await, Ok(None));
    let expired = OperationContext::with_timeout(Duration::ZERO);
    assert_eq!(
        namespace.delete(&expired, "secret-key").await,
        Err(MutationError::NotApplied(MutationFailure::Stopped(
            Stopped::Deadline
        )))
    );

    server.stall_existing();
    assert!(
        namespace
            .delete(&caller, "secret-key")
            .now_or_never()
            .is_none()
    );
    let write = namespace.set(
        &caller,
        "secret-key",
        b"private-value",
        Duration::from_secs(1),
    );
    let mut write = std::pin::pin!(write);
    tokio::select! {
        result = &mut write => panic!("write ended before disconnect: {result:?}"),
        () = server.wait_for(Duration::from_secs(3), "write must reach Valkey", || {
            server.command_count("SET", Some("items:secret-key")) == 1
        }) => {}
    }
    server.hang_up();
    assert_eq!(
        write.await,
        Err(MutationError::OutcomeUnknown(UnknownOutcome::Unavailable))
    );

    let traces = captured.rendered();
    let span = |outcome| {
        traces
            .lines()
            .find(|line| line.contains(outcome))
            .expect(&traces)
    };
    let read = span("cache.outcome=\"miss\"");
    assert!(read.contains("db.operation.name=\"MULTI\""), "{read}");
    assert!(read.contains("db.operation.batch.size=2"), "{read}");
    assert!(read.contains("db.namespace=7"), "{read}");
    assert!(read.contains("cache.name=\"items\""), "{read}");
    assert!(!read.contains("cache.mutation.outcome"), "{read}");
    let refused = span("cache.mutation.outcome=\"not_applied\"");
    assert!(refused.contains("db.operation.name=\"DEL\""), "{refused}");
    assert!(refused.contains("error.type=\"timeout\""), "{refused}");
    assert!(refused.contains("otel.status_code=\"ERROR\""), "{refused}");
    assert!(refused.contains("cache.outcome=\"timeout\""), "{refused}");
    let unknown = span("cache.mutation.outcome=\"outcome_unknown\"");
    assert!(unknown.contains("db.operation.name=\"SET\""), "{unknown}");
    assert!(unknown.contains("error.type=\"io\""), "{unknown}");
    assert!(unknown.contains("otel.status_code=\"ERROR\""), "{unknown}");
    assert!(unknown.contains("cache.outcome=\"error\""), "{unknown}");
    let dropped = span("cache.outcome=\"cancelled\"");
    assert!(!dropped.contains("cache.mutation.outcome"), "{dropped}");
    assert!(
        !traces.contains("secret-key") && !traces.contains("private-value"),
        "{traces}"
    );
}

#[tokio::test]
async fn set_admits_only_valid_floored_ttls_before_dispatch_and_observation() {
    let caller = OperationContext::unbounded();
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("ttl"));
    assert_eq!(namespace.get(&caller, "ready").await, Ok(None));
    let connections = server.connections();
    let maximum = Duration::from_millis(i64::MAX.cast_unsigned());

    for ttl in [
        Duration::ZERO,
        Duration::from_nanos(999_999),
        maximum + Duration::from_millis(1),
        Duration::MAX,
    ] {
        assert_eq!(
            namespace.set(&caller, "key", b"value", ttl).await,
            Err(crate::MutationError::NotApplied(
                crate::MutationFailure::InvalidTtl
            )),
            "{ttl:?}"
        );
        let stopped = OperationContext::with_timeout(Duration::ZERO);
        assert_eq!(
            namespace.set(&stopped, "key", b"value", ttl).await,
            Err(crate::MutationError::NotApplied(
                crate::MutationFailure::InvalidTtl
            )),
            "invalid TTL takes precedence over stopped context: {ttl:?}"
        );
    }
    assert_eq!(namespace.get(&caller, "ready").await, Ok(None));
    assert_eq!(server.command_count("SET", None), 0);
    assert_eq!(
        server.connections(),
        connections,
        "invalid TTL retired the connection"
    );
    let scrape = recorder.handle().render();
    assert!(!scrape.contains("operation=\"set\""), "{scrape}");

    let admitted = [
        (Duration::from_millis(1), "1"),
        (Duration::from_micros(1999), "1"),
        (Duration::from_millis(27), "27"),
    ];
    for (ttl, _) in admitted {
        assert_eq!(namespace.set(&caller, "key", b"value", ttl).await, Ok(()));
    }
    let commands = server.observed.commands.lock().expect("commands lock");
    let sets: Vec<_> = commands
        .iter()
        .filter(|(_, args)| args.first().is_some_and(|name| name == "SET"))
        .map(|(_, args)| args.iter().map(String::as_str).collect::<Vec<_>>())
        .collect();
    let expected: Vec<_> = admitted
        .iter()
        .map(|(_, milliseconds)| vec!["SET", "ttl:key", "value", "PX", milliseconds])
        .collect();
    assert_eq!(sets, expected);
}

async fn silent_server() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("silent listener");
    let address = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _stream = stream;
                std::future::pending::<()>().await;
            });
        }
    });
    address
}

#[tokio::test]
async fn a_dropped_get_records_cancelled_without_the_key() {
    let caller = OperationContext::unbounded();
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let address = silent_server().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(30),
        ..options(&format!("redis://{address}"), true, true, None)
    })
    .await
    .expect("start connect admits without a server");
    let namespace = cache.namespace(namespace!("obs"));
    {
        let mut operation = std::pin::pin!(namespace.get(&caller, "hunter2-key"));
        tokio::time::timeout(Duration::from_millis(30), &mut operation)
            .await
            .expect_err("the GET must still be pending when dropped");
    }
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains("outcome=\"cancelled\"") && scrape.contains("cache=\"obs\""),
        "{scrape}"
    );
    assert!(!scrape.contains("hunter2"), "{scrape}");
}

#[tokio::test]
async fn a_silent_server_records_timeout() {
    let caller = OperationContext::unbounded();
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let address = silent_server().await;
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_millis(200),
        ..options(&format!("redis://{address}"), true, true, None)
    })
    .await
    .expect("start connect");
    let namespace = cache.namespace(namespace!("obs"));
    let started = std::time::Instant::now();
    let err = namespace.get(&caller, "hunter2-key").await.unwrap_err();
    assert_eq!(err.to_string(), "cache unavailable");
    assert!(started.elapsed() < Duration::from_millis(450));
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains("outcome=\"timeout\"")
            && scrape.contains("error_type=\"timeout\"")
            && scrape.contains("operation=\"get\"")
            && scrape.contains("cache=\"obs\""),
        "{scrape}"
    );
    assert!(!scrape.contains("hunter2"), "{scrape}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reply_inside_the_command_timeout_is_not_cut_short() {
    let caller = OperationContext::unbounded();
    let server = FakeServer::start().await;
    *server.observed.slow_get_delay.lock().expect("delay lock") = Duration::from_millis(700);
    let cache = Cache::start(CacheOptions {
        command_timeout: Duration::from_secs(1),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .await
    .expect("start connect");

    assert_eq!(
        cache
            .namespace(namespace!("slow"))
            .get(&caller, "slow")
            .await,
        Ok(None)
    );
}

#[tokio::test]
async fn each_outcome_records_into_its_own_series() {
    use metrics::Key;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    let caller = OperationContext::unbounded();
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true).await;
    let namespace = cache.namespace(namespace!("series"));
    assert_eq!(namespace.get(&caller, "key").await, Ok(None));
    assert_eq!(namespace.get(&caller, "key").await, Ok(None));
    assert_eq!(
        namespace
            .set(&caller, "key", b"value", Duration::from_secs(1))
            .await,
        Ok(())
    );
    let snapshot = snapshotter.snapshot().into_vec();
    let count = |operation: &'static str, outcome: &'static str| {
        let target = Key::from_parts(
            "cache_operation_duration_seconds",
            &[
                ("cache", "series"),
                ("operation", operation),
                ("outcome", outcome),
            ],
        );
        snapshot
            .iter()
            .find_map(|(composite, _, _, value)| match value {
                DebugValue::Histogram(samples) if composite.key() == &target => Some(samples.len()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{target} missing from {snapshot:?}"))
    };
    assert_eq!(count("get", "miss"), 2);
    assert_eq!(count("set", "ok"), 1);
    let histogram_series = snapshot
        .iter()
        .filter(|(_, _, _, value)| matches!(value, DebugValue::Histogram(_)))
        .count();
    assert_eq!(histogram_series, 2, "{snapshot:?}");
}
