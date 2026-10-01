#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "cache unit tests assert admission and observation contracts"
)]

use std::path::PathBuf;
use std::time::Duration;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use secrecy::SecretString;

use crate::{Cache, CacheError, CacheOptions};

fn options(
    dsn: &str,
    allow_plaintext: bool,
    allow_unauthenticated: bool,
    root_ca_path: Option<PathBuf>,
) -> CacheOptions {
    CacheOptions {
        dsn: SecretString::from(dsn.to_owned()),
        root_ca_path,
        allow_plaintext,
        allow_unauthenticated,
        command_timeout: Duration::from_millis(100),
    }
}

fn admitted(dsn: &str, allow_plaintext: bool, allow_unauthenticated: bool) -> Cache {
    on_runtime(|| {
        Cache::connect(options(dsn, allow_plaintext, allow_unauthenticated, None))
            .expect("lazy connect admits without a server")
    })
}

fn on_runtime<T>(body: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current().is_ok() {
        return body();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _guard = runtime.enter();
    body()
}

#[test]
fn plaintext_without_the_allow_flag_is_refused() {
    let err = Cache::connect(options(
        "redis://:hunter2@127.0.0.1:6379",
        false,
        true,
        None,
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PlaintextRefused);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn plaintext_and_a_missing_password_are_admitted_when_allowed() {
    let cache = admitted("redis://127.0.0.1:6379", true, true);
    assert_eq!(cache.server().host, "127.0.0.1");
    assert_eq!(cache.server().port, 6379);
    assert!(!cache.server().tls);
    let debug = format!("{cache:?}");
    assert!(
        debug.contains("127.0.0.1") && !debug.contains("redis://"),
        "{debug}"
    );
}

#[test]
fn a_missing_password_is_refused_without_the_allow_flag() {
    let err = Cache::connect(options("rediss://127.0.0.1:6379", false, false, None)).unwrap_err();
    assert_eq!(err, CacheError::UnauthenticatedRefused);
}

#[test]
fn an_authenticated_tls_address_is_admitted_without_dialing() {
    let cache = admitted("rediss://:hunter2@cache.example:6380", false, false);
    assert!(cache.server().tls);
    assert_eq!(cache.server().port, 6380);
    let rendered = format!("{cache:?} {}", cache.server().host);
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
fn insecure_tls_fragment_is_refused() {
    let err = Cache::connect(options(
        "rediss://:hunter2@127.0.0.1:6379#insecure",
        false,
        true,
        None,
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InsecureTlsRefused);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn a_unix_socket_is_unsupported() {
    let err =
        Cache::connect(options("redis+unix:///tmp/cache.sock", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::UnsupportedAddress);
}

#[test]
fn a_root_ca_on_plaintext_is_refused_before_the_file_is_read() {
    let err = Cache::connect(options(
        "redis://127.0.0.1:6379",
        true,
        true,
        Some(PathBuf::from("/no/such/cache-ca.pem")),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::CaRequiresTls);
}

#[test]
fn a_missing_root_ca_file_reports_only_the_io_kind() {
    let missing = PathBuf::from("/no/such/cache-ca.pem");
    let err = Cache::connect(options(
        "rediss://:secret@127.0.0.1:6379",
        false,
        false,
        Some(missing),
    ))
    .unwrap_err();
    assert!(
        matches!(err, CacheError::CaFile { kind } if kind == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("secret"));
}

#[test]
fn a_root_ca_without_a_pem_certificate_is_invalid() {
    let file = tempfile::NamedTempFile::new().expect("temp ca");
    std::fs::write(file.path(), b"not a certificate").expect("write ca");
    let err = Cache::connect(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn a_root_ca_with_a_certificate_header_and_invalid_base64_is_invalid() {
    let file = tempfile::NamedTempFile::new().expect("temp ca");
    std::fs::write(
        file.path(),
        b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
    )
    .expect("write ca");
    let err = Cache::connect(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn an_unparseable_dsn_does_not_echo_the_password() {
    let err = Cache::connect(options("not a url hunter2", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::InvalidDsn);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn an_admitted_dsn_outside_a_runtime_is_refused() {
    let err = Cache::connect(options("redis://127.0.0.1:6379", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::NoRuntime);
}

#[test]
fn options_debug_redacts_the_dsn() {
    let rendered = format!(
        "{:?}",
        options("redis://:hunter2@127.0.0.1:6379", true, true, None)
    );
    assert!(!rendered.contains("hunter2"), "{rendered}");
}

#[test]
#[should_panic(expected = "cache namespace")]
fn a_namespace_name_outside_the_grammar_panics() {
    let cache = admitted("redis://127.0.0.1:6379", true, true);
    let _ = cache.namespace("Bad-Name");
}

#[test]
fn a_namespace_name_at_the_length_bound_is_accepted() {
    let cache = admitted("redis://127.0.0.1:6379", true, true);
    let name = "a".repeat(64);
    let name = Box::leak(name.into_boxed_str());
    let _ = cache.namespace(name);
}

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
        assert_eq!(crate::observe::error_type(&error), expected);
        assert!(!error.to_string().is_empty());
    }
    let server = redis::RedisError::from((
        redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError),
        "NOAUTH hunter2",
    ));
    assert_eq!(crate::observe::error_type(&server), "response");
}

#[test]
#[should_panic(expected = "cache ttl must be at least 1 ms")]
fn a_sub_millisecond_ttl_panics() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(async {
        let cache = admitted("redis://127.0.0.1:1", true, true);
        let namespace = cache.namespace("ttl");
        let _ = namespace
            .set("key", b"value", Duration::from_micros(500))
            .await;
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
            let cache = admitted(&format!("redis://{address}"), true, true);
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
            let cache = Cache::connect(CacheOptions {
                dsn: SecretString::from(format!("redis://{address}")),
                root_ca_path: None,
                allow_plaintext: true,
                allow_unauthenticated: true,
                command_timeout: Duration::from_millis(200),
            })
            .expect("lazy connect");
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
        // Setup commands arrive pipelined and are answered at once; only
        // `GET` is slow.
        let mut request = [0; 512];
        loop {
            let read = stream.read(&mut request).await.unwrap_or(0);
            if read == 0 {
                return;
            }
            let request = &request[..read];
            let reply = if request.windows(5).any(|w| w == b"\r\nGET") {
                tokio::time::sleep(Duration::from_millis(700)).await;
                b"$-1\r\n".to_vec()
            } else {
                let commands = request
                    .split(|&b| b == b'\n')
                    .filter(|l| l.first() == Some(&b'*'));
                b"+OK\r\n".repeat(commands.count())
            };
            if stream.write_all(&reply).await.is_err() {
                return;
            }
        }
    });
    let cache = Cache::connect(CacheOptions {
        dsn: SecretString::from(format!("redis://{address}")),
        root_ca_path: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        command_timeout: Duration::from_secs(1),
    })
    .expect("lazy connect");

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

fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(crate::OPERATION_DURATION_METRIC.to_owned()),
            crate::OPERATION_DURATION_BUCKETS,
        )
        .expect("observation buckets are valid")
        .build_recorder()
}

/// A server that refuses authentication until told otherwise and answers
/// every `GET` with a miss. The client authenticates inside `HELLO 3`; the
/// server counts those attempts so the test can wait for the client's own
/// reconnect chain to give up. Each accepted connection records the primary
/// generation; a `SET` on an older generation is `READONLY`.
struct FakeServer {
    address: std::net::SocketAddr,
    accept_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    auth_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    primary: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    sessions: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl FakeServer {
    async fn start() -> Self {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake server bind");
        let address = listener.local_addr().expect("fake server address");
        let accept_auth = std::sync::Arc::new(AtomicBool::new(false));
        let auth_attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let primary = std::sync::Arc::new(AtomicUsize::new(0));
        let connections = std::sync::Arc::new(AtomicUsize::new(0));
        let sessions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (accept, attempts, generation, accepted, open) = (
            accept_auth.clone(),
            auth_attempts.clone(),
            primary.clone(),
            connections.clone(),
            sessions.clone(),
        );
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                accepted.fetch_add(1, Ordering::SeqCst);
                let recorded = generation.load(Ordering::SeqCst);
                let session = tokio::spawn(serve_resp(
                    stream,
                    accept.clone(),
                    attempts.clone(),
                    recorded,
                    generation.clone(),
                ));
                open.lock().expect("sessions lock").push(session);
            }
        });
        Self {
            address,
            accept_auth,
            auth_attempts,
            primary,
            connections,
            sessions,
        }
    }

    /// Close every open connection; the listener keeps accepting.
    fn hang_up(&self) {
        for session in self.sessions.lock().expect("sessions lock").drain(..) {
            session.abort();
        }
    }

    fn attempts(&self) -> usize {
        self.auth_attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }
}

async fn serve_resp(
    stream: tokio::net::TcpStream,
    accept_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    auth_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    generation: usize,
    primary: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let Some(count) = line
            .trim_end()
            .strip_prefix('*')
            .and_then(|n| n.parse::<usize>().ok())
        else {
            return;
        };
        let mut arguments = Vec::with_capacity(count);
        for _ in 0..count {
            line.clear();
            if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let Some(length) = line
                .trim_end()
                .strip_prefix('$')
                .and_then(|n| n.parse::<usize>().ok())
            else {
                return;
            };
            let mut bulk = vec![0; length + 2];
            if read.read_exact(&mut bulk).await.is_err() {
                return;
            }
            bulk.truncate(length);
            arguments.push(String::from_utf8_lossy(&bulk).to_ascii_uppercase());
        }
        let authenticates = match arguments.first().map(String::as_str) {
            Some("AUTH") => true,
            Some("HELLO") => arguments.iter().any(|argument| argument == "AUTH"),
            _ => false,
        };
        let reply: &[u8] = match arguments.first().map(String::as_str) {
            _ if authenticates => {
                auth_attempts.fetch_add(1, Ordering::SeqCst);
                if accept_auth.load(Ordering::SeqCst) {
                    b"+OK\r\n"
                } else {
                    b"-WRONGPASS invalid username-password pair\r\n"
                }
            }
            Some("GET") => b"$-1\r\n",
            Some("PING") => b"+PONG\r\n",
            Some("SET") => {
                if generation == primary.load(Ordering::SeqCst) {
                    b"+OK\r\n"
                } else {
                    b"-READONLY You can't write against a read only replica.\r\n"
                }
            }
            _ => b"+OK\r\n",
        };
        if write.write_all(reply).await.is_err() {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_auth_is_retried_after_the_client_gives_up() {
    let gate = FakeServer::start().await;
    let cache = Cache::connect(CacheOptions {
        dsn: SecretString::from(format!("redis://:secret@{}", gate.address)),
        root_ca_path: None,
        allow_plaintext: true,
        allow_unauthenticated: false,
        command_timeout: Duration::from_millis(200),
    })
    .expect("lazy connect");
    let namespace = cache.namespace("auth");

    // Drive redis's own reconnect chain (one attempt plus its retries) until it
    // gives up; the lazy chain advances only while a caller awaits it. From
    // then on redis's manager answers every call with the stored AUTH failure
    // and never dials again.
    let chain = crate::NUMBER_OF_RETRIES + 1;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while gate.attempts() < chain {
        assert!(
            tokio::time::Instant::now() < deadline,
            "reconnect chain never finished"
        );
        assert_eq!(namespace.get("key").await, Err(crate::Unavailable));
    }

    gate.accept_auth
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if namespace.get("key").await == Ok(None) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cache stayed unavailable after the server accepted AUTH again"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_readonly_reply_reconnects_to_the_new_primary() {
    let server = FakeServer::start().await;
    let cache = Cache::connect(CacheOptions {
        dsn: SecretString::from(format!("redis://{}", server.address)),
        root_ca_path: None,
        allow_plaintext: true,
        allow_unauthenticated: true,
        command_timeout: Duration::from_millis(200),
    })
    .expect("lazy connect");
    let namespace = cache.namespace("failover");
    let ttl = Duration::from_secs(1);

    namespace
        .set("key", b"value", ttl)
        .await
        .expect("set on the current primary");
    server
        .primary
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        namespace.set("key", b"value", ttl).await,
        Err(crate::Unavailable)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_hello_is_reported_as_auth() {
    use health::Probe;

    let gate = FakeServer::start().await;
    let cache = Cache::connect(CacheOptions {
        dsn: SecretString::from(format!("redis://:secret@{}", gate.address)),
        root_ca_path: None,
        allow_plaintext: true,
        allow_unauthenticated: false,
        command_timeout: Duration::from_millis(200),
    })
    .expect("lazy connect");

    let refused = tokio::time::timeout(Duration::from_secs(20), cache.probe().check())
        .await
        .expect("the reconnect chain ends")
        .expect_err("the server refuses the password");
    assert_eq!(refused.to_string(), "cache ping failed: auth");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_idle_connection_reconnects_before_the_next_call() {
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true);
    let namespace = cache.namespace("idle");
    assert_eq!(namespace.get("key").await, Ok(None));
    assert_eq!(server.connections(), 1);

    server.hang_up();
    // No cache call from here on: the client must notice the closed socket
    // and dial again on its own.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.connections() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the client waited for a call before it reconnected"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(namespace.get("key").await, Ok(None));
}
