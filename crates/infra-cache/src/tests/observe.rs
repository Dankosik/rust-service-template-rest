use super::*;

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
        assert_eq!(crate::connection::error_type(&error).label(), expected);
        assert_ne!(error.to_string(), "");
    }
    let server = redis::RedisError::from((
        redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError),
        "NOAUTH hunter2",
    ));
    assert_eq!(crate::connection::error_type(&server).label(), "response");
}

#[test]
fn a_failed_command_names_its_cause_on_the_series() {
    use crate::Operation;
    use crate::connection::ErrorType;
    use crate::observe::{NamespaceHistograms, OperationGuard};

    let recorder = observation_recorder();
    let server = crate::ServerEndpoint {
        host: "cache.example".to_owned(),
        port: 6379,
        tls: true,
    };
    metrics::with_local_recorder(&recorder, || {
        let histograms = NamespaceHistograms::default();
        for operation in [Operation::Get, Operation::Set, Operation::Delete] {
            for error_type in [
                ErrorType::Auth,
                ErrorType::Io,
                ErrorType::Timeout,
                ErrorType::Response,
                ErrorType::Parse,
                ErrorType::Other,
            ] {
                let _ = OperationGuard::start("causes", &histograms, operation, &server)
                    .fail(error_type);
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

#[test]
fn set_admits_only_valid_floored_ttls_before_dispatch_and_observation() {
    let recorder = observation_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            let server = FakeServer::start().await;
            let cache = admitted(&format!("redis://{}", server.address), true, true);
            let namespace = cache.namespace("ttl");
            assert_eq!(namespace.get("ready").await, Ok(None));
            let connections = server.connections();
            let maximum = Duration::from_millis(i64::MAX as u64);

            for ttl in [
                Duration::ZERO,
                Duration::from_nanos(999_999),
                maximum + Duration::from_millis(1),
                Duration::MAX,
            ] {
                assert_eq!(
                    namespace.set("key", b"value", ttl).await,
                    Err(crate::SetError::InvalidTtl),
                    "{ttl:?}"
                );
                let stopped = operation_context::OperationContext::with_timeout(Duration::ZERO);
                assert_eq!(
                    namespace
                        .set_with_context("key", b"value", ttl, &stopped)
                        .await,
                    Err(crate::SetError::InvalidTtl),
                    "invalid TTL takes precedence over stopped context: {ttl:?}"
                );
            }
            // The GET reply fences any earlier commands on the same connection.
            assert_eq!(namespace.get("ready").await, Ok(None));
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
                (maximum, "9223372036854775807"),
                (
                    maximum + Duration::from_nanos(999_999),
                    "9223372036854775807",
                ),
            ];
            for (ttl, _) in admitted {
                assert_eq!(namespace.set("key", b"value", ttl).await, Ok(()));
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
        });
    });
}

#[test]
fn a_dropped_get_records_cancelled_without_the_key() {
    let recorder = observation_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("silent listener");
            let address = listener.local_addr().expect("listener address");
            let _held = tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let _stream = stream;
                        std::future::pending::<()>().await;
                    });
                }
            });
            // A command timeout far above the drop below: on a stalled
            // runner both timers could otherwise be due at one poll, and the
            // command's own would record `timeout`.
            let cache = Cache::start(CacheOptions {
                command_timeout: Duration::from_secs(30),
                ..options(&format!("redis://{address}"), true, true, None)
            })
            .expect("start connect admits without a server");
            let namespace = cache.namespace("obs");
            let mut operation = std::pin::pin!(namespace.get("hunter2-key"));
            let _ = tokio::time::timeout(Duration::from_millis(30), &mut operation).await;
        });
    });
    let scrape = recorder.handle().render();
    assert!(
        scrape.contains("outcome=\"cancelled\"") && scrape.contains("cache=\"obs\""),
        "{scrape}"
    );
    assert!(!scrape.contains("hunter2"), "{scrape}");
}

#[test]
fn a_silent_server_records_timeout() {
    let recorder = observation_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("silent listener");
            let address = listener.local_addr().expect("listener address");
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let _stream = stream;
                        std::future::pending::<()>().await;
                    });
                }
            });
            let cache = Cache::start(CacheOptions {
                dsn: SecretString::from(format!("redis://{address}")),
                password_file: None,
                root_ca_path: None,
                client_certificate: None,
                allow_plaintext: true,
                allow_unauthenticated: true,
                command_timeout: Duration::from_millis(200),
            })
            .expect("start connect");
            let namespace = cache.namespace("obs");
            let started = std::time::Instant::now();
            let err = namespace.get("hunter2-key").await.unwrap_err();
            assert_eq!(err.to_string(), "cache unavailable");
            assert!(started.elapsed() < Duration::from_millis(450));
        });
    });
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("slow listener");
    let address = listener.local_addr().expect("listener address");
    tokio::spawn(async move {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        // Commands arrive pipelined (setup, the background `PING`, the `GET`)
        // and each is answered in order; only a read that holds `GET` is slow.
        let mut request = [0; 512];
        loop {
            let read = stream.read(&mut request).await.unwrap_or(0);
            if read == 0 {
                return;
            }
            let mut reply = Vec::new();
            let mut slow = false;
            let mut lines = request[..read].split(|&b| b == b'\n');
            while let Some(line) = lines.next() {
                if line.first() != Some(&b'*') {
                    continue;
                }
                // The argument count is followed by the name's length, then the name.
                let _length = lines.next();
                if lines.next().is_some_and(|name| name.starts_with(b"GET")) {
                    slow = true;
                    reply.extend_from_slice(b"$-1\r\n");
                } else {
                    reply.extend_from_slice(b"+OK\r\n");
                }
            }
            if slow {
                tokio::time::sleep(Duration::from_millis(700)).await;
            }
            if stream.write_all(&reply).await.is_err() {
                return;
            }
        }
    });
    let cache = Cache::start(CacheOptions {
        dsn: SecretString::from(format!("redis://{address}")),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        command_timeout: Duration::from_secs(1),
    })
    .expect("start connect");

    assert_eq!(cache.namespace("slow").get("key").await, Ok(None));
}

#[test]
fn each_outcome_records_into_its_own_series() {
    let recorder = observation_recorder();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    metrics::with_local_recorder(&recorder, || {
        runtime.block_on(async {
            let server = FakeServer::start().await;
            let cache = admitted(&format!("redis://{}", server.address), true, true);
            let namespace = cache.namespace("series");
            assert_eq!(namespace.get("key").await, Ok(None));
            assert_eq!(namespace.get("key").await, Ok(None));
            assert_eq!(
                namespace.set("key", b"value", Duration::from_secs(1)).await,
                Ok(())
            );
        });
    });
    let scrape = recorder.handle().render();
    let count = |operation: &str, outcome: &str| {
        let series = format!(
            "cache_operation_duration_seconds_count{{cache=\"series\",operation=\"{operation}\",outcome=\"{outcome}\"}} "
        );
        scrape
            .lines()
            .find_map(|line| line.strip_prefix(series.as_str()))
            .unwrap_or_else(|| panic!("{series} missing from {scrape}"))
            .to_owned()
    };
    assert_eq!(count("get", "miss"), "2");
    assert_eq!(count("set", "ok"), "1");
    assert_eq!(scrape.matches("_count{").count(), 2, "{scrape}");
}
