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

use crate::{Cache, CacheError, CacheOptions, ClientCertificate};

fn options(
    dsn: &str,
    allow_plaintext: bool,
    allow_unauthenticated: bool,
    root_ca_path: Option<PathBuf>,
) -> CacheOptions {
    CacheOptions {
        dsn: SecretString::from(dsn.to_owned()),
        password_file: None,
        root_ca_path,
        client_certificate: None,
        allow_plaintext,
        allow_unauthenticated,
        command_timeout: Duration::from_millis(100),
    }
}

fn admitted(dsn: &str, allow_plaintext: bool, allow_unauthenticated: bool) -> Cache {
    on_runtime(|| {
        Cache::connect_lazy(options(dsn, allow_plaintext, allow_unauthenticated, None))
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
    let err = Cache::connect_lazy(options(
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
        "cache debug output must identify the server without the DSN"
    );
}

#[test]
fn a_missing_password_is_refused_without_the_allow_flag() {
    let err =
        Cache::connect_lazy(options("rediss://127.0.0.1:6379", false, false, None)).unwrap_err();
    assert_eq!(err, CacheError::UnauthenticatedRefused);
}

#[test]
fn an_authenticated_tls_address_is_admitted_without_a_server() {
    let cache = admitted("rediss://:hunter2@cache.example:6380", false, false);
    assert!(cache.server().tls);
    assert_eq!(cache.server().port, 6380);
    let rendered = format!("{cache:?} {}", cache.server().host);
    assert!(
        !rendered.contains("hunter2"),
        "cache debug output disclosed a password"
    );
}

#[test]
fn insecure_tls_fragment_is_refused() {
    let err = Cache::connect_lazy(options(
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
        Cache::connect_lazy(options("redis+unix:///tmp/cache.sock", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::UnsupportedAddress);
}

#[test]
fn a_root_ca_on_plaintext_is_refused_before_the_file_is_read() {
    let err = Cache::connect_lazy(options(
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
    let err = Cache::connect_lazy(options(
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
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_root_ca_without_a_pem_certificate_is_invalid() {
    let file = tempfile::NamedTempFile::new().expect("temp ca");
    std::fs::write(file.path(), b"not a certificate").expect("write ca");
    let err = Cache::connect_lazy(options(
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
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_root_ca_with_a_certificate_header_and_invalid_base64_is_invalid() {
    let file = tempfile::NamedTempFile::new().expect("temp ca");
    std::fs::write(
        file.path(),
        b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
    )
    .expect("write ca");
    let err = Cache::connect_lazy(options(
        "rediss://:hunter2@127.0.0.1:6379",
        false,
        false,
        Some(file.path().to_path_buf()),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidCa);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

/// A PEM file holding one block of `der` under `label`.
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn write_pem(label: &str, der: &[u8]) -> tempfile::NamedTempFile {
    use base64::Engine;

    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut body = String::new();
    for line in encoded.as_bytes().chunks(64) {
        body.push_str(std::str::from_utf8(line).expect("base64 is ascii"));
        body.push('\n');
    }
    let pem = format!("-----BEGIN {label}-----\n{body}-----END {label}-----\n");
    let file = tempfile::NamedTempFile::new().expect("temp pem");
    std::fs::write(file.path(), pem).expect("write pem");
    file
}

/// A self-signed certificate and its PKCS #8 key, each in its own PEM file.
fn client_identity() -> (tempfile::NamedTempFile, tempfile::NamedTempFile) {
    let key = rcgen::KeyPair::generate().expect("client key");
    let certificate = rcgen::CertificateParams::new(vec!["cache-client".to_owned()])
        .expect("client certificate params")
        .self_signed(&key)
        .expect("client certificate");
    (
        write_pem("CERTIFICATE", certificate.der()),
        write_pem("PRIVATE KEY", &key.serialize_der()),
    )
}

fn with_client_certificate(dsn: &str, cert_path: PathBuf, key_path: PathBuf) -> CacheOptions {
    CacheOptions {
        client_certificate: Some(ClientCertificate {
            cert_path,
            key_path,
        }),
        ..options(dsn, true, false, None)
    }
}

const TLS_DSN: &str = "rediss://:hunter2@127.0.0.1:6379";

#[test]
fn a_client_certificate_with_its_key_is_admitted_without_dialing() {
    let (cert, key) = client_identity();
    on_runtime(|| {
        Cache::connect_lazy(with_client_certificate(
            TLS_DSN,
            cert.path().to_path_buf(),
            key.path().to_path_buf(),
        ))
        .expect("a matching pair is admitted")
    });
}

#[test]
fn a_client_certificate_on_plaintext_is_refused_before_the_files_are_read() {
    let err = Cache::connect_lazy(with_client_certificate(
        "redis://:hunter2@127.0.0.1:6379",
        PathBuf::from("/no/such/client.crt"),
        PathBuf::from("/no/such/client.key"),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::ClientCertificateRequiresTls);
}

#[test]
fn a_missing_client_certificate_or_key_file_reports_only_the_io_kind() {
    let (cert, key) = client_identity();
    let err = Cache::connect_lazy(with_client_certificate(
        TLS_DSN,
        PathBuf::from("/no/such/client.crt"),
        key.path().to_path_buf(),
    ))
    .unwrap_err();
    assert!(
        matches!(err, CacheError::ClientCertificateFile { kind } if kind == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("/no/such"), "{err:?}");

    let err = Cache::connect_lazy(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        PathBuf::from("/no/such/client.key"),
    ))
    .unwrap_err();
    assert!(
        matches!(err, CacheError::ClientKeyFile { kind } if kind == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
    assert!(!format!("{err} {err:?}").contains("/no/such"), "{err:?}");
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_client_certificate_file_without_a_pem_certificate_is_invalid() {
    let (_, key) = client_identity();
    let file = tempfile::NamedTempFile::new().expect("temp certificate");
    std::fs::write(file.path(), b"not a certificate").expect("write certificate");
    let err = Cache::connect_lazy(with_client_certificate(
        TLS_DSN,
        file.path().to_path_buf(),
        key.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientCertificate);
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_client_key_that_is_unusable_or_belongs_to_another_certificate_is_invalid() {
    let (cert, _) = client_identity();
    let (_, other_key) = client_identity();
    let err = Cache::connect_lazy(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        other_key.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);

    let garbage = tempfile::NamedTempFile::new().expect("temp key");
    std::fs::write(garbage.path(), b"not a key").expect("write key");
    let err = Cache::connect_lazy(with_client_certificate(
        TLS_DSN,
        cert.path().to_path_buf(),
        garbage.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::InvalidClientKey);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn an_unparseable_dsn_does_not_echo_the_password() {
    let err = Cache::connect_lazy(options("not a url hunter2", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::InvalidDsn);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
fn an_admitted_dsn_outside_a_runtime_is_refused() {
    let err = Cache::connect_lazy(options("redis://127.0.0.1:6379", true, true, None)).unwrap_err();
    assert_eq!(err, CacheError::NoRuntime);
}

#[test]
fn options_debug_redacts_the_dsn() {
    let rendered = format!(
        "{:?}",
        options("redis://:hunter2@127.0.0.1:6379", true, true, None)
    );
    assert!(
        !rendered.contains("hunter2"),
        "cache debug output disclosed a password"
    );
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
        assert_eq!(crate::observe::error_type(&error).label(), expected);
        assert_ne!(error.to_string(), "");
    }
    let server = redis::RedisError::from((
        redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError),
        "NOAUTH hunter2",
    ));
    assert_eq!(crate::observe::error_type(&server).label(), "response");
}

fn with_password_file(dsn: &str, password_file: PathBuf) -> CacheOptions {
    CacheOptions {
        password_file: Some(password_file),
        ..options(dsn, true, false, None)
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_password_file_admits_a_dsn_without_a_password() {
    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), "hunter2\n").expect("write password");
    // No password in the DSN and no `allow_unauthenticated`.
    on_runtime(|| {
        Cache::connect_lazy(with_password_file(
            "redis://127.0.0.1:6379",
            file.path().to_path_buf(),
        ))
        .expect("the file is the password source")
    });
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn a_password_in_the_dsn_and_a_password_file_are_refused_together() {
    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), "from-file").expect("write password");
    let err = Cache::connect_lazy(with_password_file(
        "redis://:hunter2@127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordInDsn);
    assert!(!format!("{err} {err:?}").contains("hunter2"));
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
fn an_unreadable_or_empty_password_file_fails_admission() {
    let missing = PathBuf::from("/no/such/cache-password");
    let err =
        Cache::connect_lazy(with_password_file("redis://127.0.0.1:6379", missing)).unwrap_err();
    assert!(
        matches!(err, CacheError::PasswordFile { kind } if kind == std::io::ErrorKind::NotFound),
        "{err:?}"
    );
    assert!(!err.to_string().contains("/no/such"), "{err}");

    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), "\n").expect("write password");
    let err = Cache::connect_lazy(with_password_file(
        "redis://127.0.0.1:6379",
        file.path().to_path_buf(),
    ))
    .unwrap_err();
    assert_eq!(err, CacheError::PasswordFileEmpty);
}

#[test]
fn a_failed_command_names_its_cause_on_the_series() {
    use crate::observe::{ErrorType, Histograms, Operation, OperationGuard};

    let recorder = observation_recorder();
    let server = crate::ServerIdentity {
        host: "cache.example".to_owned(),
        port: 6379,
        tls: true,
    };
    metrics::with_local_recorder(&recorder, || {
        let histograms = Histograms::default();
        for error_type in [
            ErrorType::Auth,
            ErrorType::Io,
            ErrorType::Timeout,
            ErrorType::Other,
        ] {
            let _ = OperationGuard::start("causes", &histograms, Operation::Set, &server)
                .fail(error_type);
        }
    });
    let scrape = recorder.handle().render();
    for (outcome, error_type) in [
        ("error", "auth"),
        ("error", "io"),
        ("timeout", "timeout"),
        ("error", "other"),
    ] {
        let series = format!(
            "cache_operation_duration_seconds_count{{cache=\"causes\",operation=\"set\",outcome=\"{outcome}\",error_type=\"{error_type}\"}} 1"
        );
        assert!(scrape.contains(&series), "{series} missing from {scrape}");
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
                    namespace.set_with_context("key", b"value", ttl, &stopped).await,
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
            let cache = Cache::connect_lazy(CacheOptions {
                command_timeout: Duration::from_secs(30),
                ..options(&format!("redis://{address}"), true, true, None)
            })
            .expect("lazy connect admits without a server");
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
            let cache = Cache::connect_lazy(CacheOptions {
                dsn: SecretString::from(format!("redis://{address}")),
                password_file: None,
                root_ca_path: None,
                client_certificate: None,
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
    let cache = Cache::connect_lazy(CacheOptions {
        dsn: SecretString::from(format!("redis://{address}")),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
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
/// server counts those attempts so tests can observe the complete setup retry
/// allowance. Each accepted connection records the primary
/// generation; a `SET` on an older generation is `READONLY`. With a
/// `password` set, authentication succeeds only with that password.
struct FakeServer {
    address: std::net::SocketAddr,
    accept_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    password: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    auth_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    primary: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    sessions: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    observed: std::sync::Arc<SocketObservations>,
    listener: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    async fn start() -> Self {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake server bind");
        let address = listener.local_addr().expect("fake server address");
        let accept_auth = std::sync::Arc::new(AtomicBool::new(false));
        let password = std::sync::Arc::new(std::sync::Mutex::new(None));
        let auth_attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let primary = std::sync::Arc::new(AtomicUsize::new(0));
        let connections = std::sync::Arc::new(AtomicUsize::new(0));
        let sessions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (accept, expected, attempts, generation, accepted, open) = (
            accept_auth.clone(),
            password.clone(),
            auth_attempts.clone(),
            primary.clone(),
            connections.clone(),
            sessions.clone(),
        );
        let observed = std::sync::Arc::new(SocketObservations::default());
        let events = observed.clone();
        let listener = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let id = accepted.fetch_add(1, Ordering::SeqCst) + 1;
                let socket = ObservedSocket::new(id, events.clone());
                let recorded = generation.load(Ordering::SeqCst);
                let session = tokio::spawn(serve_resp(
                    stream,
                    accept.clone(),
                    expected.clone(),
                    attempts.clone(),
                    recorded,
                    generation.clone(),
                    socket,
                ));
                open.lock().expect("sessions lock").push(session);
            }
        });
        Self {
            address,
            accept_auth,
            password,
            auth_attempts,
            primary,
            connections,
            sessions,
            observed,
            listener,
        }
    }

    /// Close every open connection; the listener keeps accepting.
    fn hang_up(&self) {
        for session in self.sessions.lock().expect("sessions lock").drain(..) {
            session.abort();
        }
    }

    /// Accept only this password from now on.
    fn require_password(&self, password: &str) {
        *self.password.lock().expect("password lock") = Some(password.to_owned());
    }

    fn attempts(&self) -> usize {
        self.auth_attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }
}

async fn read_resp_arguments(
    read: &mut tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    line: &mut String,
) -> Option<Vec<String>> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    line.clear();
    if read.read_line(line).await.unwrap_or(0) == 0 {
        return None;
    }
    let count = line.trim_end().strip_prefix('*')?.parse::<usize>().ok()?;
    let mut arguments = Vec::with_capacity(count);
    for _ in 0..count {
        line.clear();
        if read.read_line(line).await.unwrap_or(0) == 0 {
            return None;
        }
        let length = line.trim_end().strip_prefix('$')?.parse::<usize>().ok()?;
        let mut bulk = vec![0; length + 2];
        read.read_exact(&mut bulk).await.ok()?;
        bulk.truncate(length);
        arguments.push(String::from_utf8_lossy(&bulk).into_owned());
    }
    Some(arguments)
}

async fn serve_resp(
    stream: tokio::net::TcpStream,
    accept_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    password: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    auth_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    generation: usize,
    primary: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    socket: ObservedSocket,
) {
    use std::sync::atomic::Ordering;
    use tokio::io::{AsyncWriteExt, BufReader};

    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut line = String::new();
    while let Some(arguments) = read_resp_arguments(&mut read, &mut line).await {
        socket
            .observed
            .commands
            .lock()
            .expect("commands lock")
            .push((socket.id, arguments.clone()));
        socket.observed.changed.notify_waiters();
        if socket.observed.silence_all.load(Ordering::SeqCst)
            || socket.id <= socket.observed.silence_through.load(Ordering::SeqCst)
        {
            // Keep reading to detect client EOF and count dispatched commands,
            // but never answer a slot on a deliberately stalled connection.
            continue;
        }
        let auth_error = format!(
            "-WRONGPASS {}\r\n",
            socket.observed.auth_error.lock().expect("auth error lock")
        );
        let authenticates = match arguments.first().map(String::as_str) {
            Some("AUTH") => true,
            Some("HELLO") => arguments.iter().any(|argument| argument == "AUTH"),
            _ => false,
        };
        let reply: &[u8] = match arguments.first().map(String::as_str) {
            _ if authenticates => {
                auth_attempts.fetch_add(1, Ordering::SeqCst);
                let accepted = match password.lock().expect("password lock").as_ref() {
                    Some(expected) => arguments.last() == Some(expected),
                    None => accept_auth.load(Ordering::SeqCst),
                } && socket
                    .observed
                    .username
                    .lock()
                    .expect("username lock")
                    .as_ref()
                    .is_none_or(|expected| arguments.get(arguments.len() - 2) == Some(expected));
                if accepted {
                    socket
                        .observed
                        .auth_successes
                        .fetch_add(1, Ordering::SeqCst);
                    socket.observed.changed.notify_waiters();
                    b"+OK\r\n"
                } else {
                    socket
                        .observed
                        .auth_rejections
                        .fetch_add(1, Ordering::SeqCst);
                    socket.observed.changed.notify_waiters();
                    auth_error.as_bytes()
                }
            }
            Some("GET") => b"$-1\r\n",
            Some("PING") => b"+PONG\r\n",
            Some("DEL") => b":1\r\n",
            Some("SET") => {
                if generation == primary.load(Ordering::SeqCst) {
                    b"+OK\r\n"
                } else {
                    b"-READONLY You can't write against a read only replica.\r\n"
                }
            }
            _ => b"+OK\r\n",
        };
        let reply_delay = match arguments.first().map(String::as_str) {
            Some("AUTH") => *socket.observed.auth_reply_delay.lock().expect("delay lock"),
            Some("PING") => socket
                .observed
                .ping_reply_delays
                .lock()
                .expect("delay lock")
                .pop_front()
                .unwrap_or_default(),
            _ => Duration::ZERO,
        };
        if !reply_delay.is_zero() {
            tokio::time::sleep(reply_delay).await;
        }
        if write.write_all(reply).await.is_err() {
            return;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_auth_is_retried_after_the_client_gives_up() {
    let gate = FakeServer::start().await;
    let cache = Cache::connect_lazy(CacheOptions {
        dsn: SecretString::from(format!("redis://:secret@{}", gate.address)),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
        allow_plaintext: true,
        allow_unauthenticated: false,
        command_timeout: Duration::from_millis(200),
    })
    .expect("lazy connect");
    let namespace = cache.namespace("auth");

    // Keep AUTH unavailable for a complete setup chain, then make the
    // endpoint usable. Recovery must outlive the initial retry allowance.
    let chain = 7; // The accepted setup chain is one attempt plus six retries.
    gate.wait_for(
        Duration::from_secs(20),
        "setup retry chain did not progress without traffic",
        || gate.attempts() >= chain,
    )
    .await;
    gate.accept_auth
        .store(true, std::sync::atomic::Ordering::SeqCst);
    gate.wait_for(
        Duration::from_secs(5),
        "authentication did not recover without traffic after the first setup chain",
        || {
            gate.observed
                .auth_successes
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
        },
    )
    .await;
    assert_eq!(namespace.get("key").await, Ok(None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_readonly_reply_reconnects_to_the_new_primary() {
    let server = FakeServer::start().await;
    let cache = Cache::connect_lazy(CacheOptions {
        dsn: SecretString::from(format!("redis://{}", server.address)),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_hello_is_reported_as_auth() {
    use health::Probe;

    let gate = FakeServer::start().await;
    let cache = Cache::connect_lazy(CacheOptions {
        dsn: SecretString::from(format!("redis://:secret@{}", gate.address)),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_replaced_connection_dials_without_waiting_for_a_call() {
    let server = FakeServer::start().await;
    let cache = admitted(&format!("redis://{}", server.address), true, true);
    let namespace = cache.namespace("warm");
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
        Err(crate::SetError::Unavailable(crate::Unavailable))
    );

    // No cache call from here on: recovery must dial on its own.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.connections() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the replaced connection waited for a call before it dialed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn ephemeral_password() -> String {
    let mut password_bytes = [0_u8; 32];
    let random = rustls::crypto::aws_lc_rs::default_provider().secure_random;
    random
        .fill(&mut password_bytes)
        .expect("ephemeral password");
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, password_bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn a_rotated_password_file_authenticates_the_next_connection() {
    let server = FakeServer::start().await;
    let first = ephemeral_password();
    let second = ephemeral_password();
    server.require_password(&first);
    let file = tempfile::NamedTempFile::new().expect("temp password");
    std::fs::write(file.path(), format!("{first}\n")).expect("write password");
    let cache = Cache::connect_lazy(with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    ))
    .expect("lazy connect");
    let namespace = cache.namespace("rotation");
    assert_eq!(namespace.get("key").await, Ok(None));
    // Successful setup must authenticate with the file's current bytes.
    let opened = server.attempts();
    assert!(opened >= 1, "the file's password was not sent");

    // The platform rotates the password; the old connection is gone.
    std::fs::write(file.path(), format!("{second}\n")).expect("rotate password");
    server.require_password(&second);
    server.hang_up();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if namespace.get("key").await == Ok(None) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the cache did not authenticate with the rotated password"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        server.attempts() > opened,
        "the new connection did not authenticate"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_connection_dials_without_waiting_for_a_call() {
    let server = FakeServer::start().await;
    let _cache = admitted(&format!("redis://{}", server.address), true, true);

    // No cache call at all: admission itself must start the connection.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.connections() < 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first connection waited for a call before it dialed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[derive(Default)]
struct SocketObservations {
    active: std::sync::Mutex<std::collections::HashSet<usize>>,
    commands: std::sync::Mutex<Vec<(usize, Vec<String>)>>,
    silence_through: std::sync::atomic::AtomicUsize,
    silence_all: std::sync::atomic::AtomicBool,
    auth_successes: std::sync::atomic::AtomicUsize,
    auth_rejections: std::sync::atomic::AtomicUsize,
    auth_error: std::sync::Mutex<String>,
    username: std::sync::Mutex<Option<String>>,
    auth_reply_delay: std::sync::Mutex<Duration>,
    ping_reply_delays: std::sync::Mutex<std::collections::VecDeque<Duration>>,
    changed: tokio::sync::Notify,
}

struct ObservedSocket {
    id: usize,
    observed: std::sync::Arc<SocketObservations>,
}

impl ObservedSocket {
    fn new(id: usize, observed: std::sync::Arc<SocketObservations>) -> Self {
        observed.active.lock().expect("active lock").insert(id);
        observed.changed.notify_waiters();
        Self { id, observed }
    }
}

impl Drop for ObservedSocket {
    fn drop(&mut self) {
        self.observed
            .active
            .lock()
            .expect("active lock")
            .remove(&self.id);
        self.observed.changed.notify_waiters();
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.listener.abort();
        self.hang_up();
    }
}

impl FakeServer {
    async fn wait_for(&self, budget: Duration, reason: &str, ready: impl Fn() -> bool) {
        tokio::time::timeout(budget, async {
            loop {
                let changed = self.observed.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if ready() {
                    break;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{reason}"));
    }

    fn active(&self) -> usize {
        self.observed.active.lock().expect("active lock").len()
    }

    fn command_count(&self, command: &str, key: Option<&str>) -> usize {
        self.observed
            .commands
            .lock()
            .expect("commands lock")
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|name| name == command)
                    && key.is_none_or(|key| args.get(1).is_some_and(|arg| arg == key))
            })
            .count()
    }

    fn stall_existing(&self) -> usize {
        let through = self.connections();
        self.observed
            .silence_through
            .store(through, std::sync::atomic::Ordering::SeqCst);
        through
    }

    fn closed_through(&self, through: usize) -> bool {
        self.observed
            .active
            .lock()
            .expect("active lock")
            .iter()
            .all(|id| *id > through)
    }
}

fn assert_work_metrics(
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    in_flight: usize,
    refused: usize,
    retirements: usize,
) {
    let scrape = recorder.handle().render();
    for (name, expected) in [
        ("cache_commands_in_flight", in_flight),
        ("cache_command_admission_refused_total", refused),
        ("cache_connection_retirements_total", retirements),
    ] {
        let series = format!("{name} {expected}");
        assert!(
            scrape.lines().any(|line| line == series),
            "{series} missing from {scrape}"
        );
    }
}

// These tests exercise real socket readiness and EOF. Bounded event waits keep
// the clock running with I/O, avoiding paused-time auto-advance past OS events.
async fn poll_once<F: Future + ?Sized>(
    mut future: std::pin::Pin<&mut F>,
) -> std::task::Poll<F::Output> {
    std::future::poll_fn(|context| std::task::Poll::Ready(future.as_mut().poll(context))).await
}

#[tokio::test]
async fn reliability_admission_bounds_connection_waits_without_cancelling_setup() {
    use health::Probe;
    use std::task::Poll;

    let server = FakeServer::start().await;
    let recorder = observation_recorder();
    let cache = metrics::with_local_recorder(&recorder, || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    assert_work_metrics(&recorder, 0, 0, 0);
    let namespace = cache.namespace("waiting");
    let mut waiting = Vec::new();
    // No scheduler yield: the supervisor has not published a connection yet.
    for _ in 0..256 {
        let mut command = Box::pin(namespace.get("pending"));
        assert!(poll_once(command.as_mut()).await.is_pending());
        waiting.push(command);
    }
    assert_work_metrics(&recorder, 256, 0, 0);
    // Another link shares the unlabelled series without resetting active work.
    let other = metrics::with_local_recorder(&recorder, || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    assert_work_metrics(&recorder, 256, 0, 0);
    drop(other);
    let mut excess = Box::pin(namespace.get("refused"));
    assert_eq!(
        poll_once(excess.as_mut()).await,
        Poll::Ready(Err(crate::Unavailable))
    );
    assert_work_metrics(&recorder, 256, 1, 0);
    let probe = cache.probe();
    let mut waiting_probe = probe.check();
    assert!(poll_once(waiting_probe.as_mut()).await.is_pending());
    let mut excess_probe = probe.check();
    assert!(matches!(
        poll_once(excess_probe.as_mut()).await,
        Poll::Ready(Err(_))
    ));
    assert_work_metrics(&recorder, 257, 2, 0);
    drop(waiting_probe);
    drop(waiting.pop());
    assert_work_metrics(&recorder, 255, 2, 0);
    let mut replacement = Box::pin(namespace.get("replacement"));
    assert!(poll_once(replacement.as_mut()).await.is_pending());
    drop(waiting);
    assert_work_metrics(&recorder, 1, 2, 0);
    assert_eq!(replacement.await, Ok(None));
    assert_work_metrics(&recorder, 0, 2, 0);
    probe
        .check()
        .await
        .expect("setup and probe capacity survive acquisition cancellation");
    assert_work_metrics(&recorder, 0, 2, 0);
    assert_eq!(server.connections(), 1);
    assert_eq!(server.command_count("GET", Some("waiting:pending")), 0);
    assert_eq!(server.command_count("GET", Some("waiting:refused")), 0);
}

#[tokio::test]
async fn reliability_full_application_window_leaves_probe_and_maintenance_admission() {
    use health::Probe;
    use std::task::Poll;

    let server = FakeServer::start().await;
    let cache = Cache::connect_lazy(CacheOptions {
        command_timeout: Duration::from_secs(30),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("lazy cache");
    let namespace = cache.namespace("capacity");
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
    let mut pending: Vec<_> = (0..256)
        .map(|_| Box::pin(namespace.get("pending")))
        .collect();
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
    let sibling = cache.namespace("other_capacity");
    let mut excess = Box::pin(sibling.delete("refused"));
    assert_eq!(
        poll_once(excess.as_mut()).await,
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
    assert!(matches!(
        poll_once(excess_probe.as_mut()).await,
        Poll::Ready(Err(error)) if error.to_string() == "cache ping failed: other"
    ));
    // Leave caller futures unpolled; the supervisor must progress independently
    // of all 257 occupied application/probe slots, even after the probe deadline.
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
    drop(pending_probe);
    drop(pending);
    server
        .wait_for(
            Duration::from_secs(4),
            "saturated generation did not recover",
            || server.connections() > old && server.closed_through(old),
        )
        .await;
    assert_eq!(namespace.get("recovered").await, Ok(None));
    probe.check().await.expect("probe capacity is reusable");
}

#[tokio::test]
async fn reliability_stalled_generations_recover_without_replaying_writes() {
    let server = FakeServer::start().await;
    let recorder = observation_recorder();
    let cache = metrics::with_local_recorder(&recorder, || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    let namespace = cache.namespace("stalled");
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
        assert_work_metrics(&recorder, 0, 0, cycle + 1);
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
    assert_work_metrics(&recorder, 0, 0, 4);
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
async fn reliability_cancelled_long_command_and_probe_slots_are_retired() {
    use health::Probe;
    use std::task::Poll;

    let server = FakeServer::start().await;
    let recorder = observation_recorder();
    let cache = metrics::with_local_recorder(&recorder, || {
        Cache::connect_lazy(CacheOptions {
            command_timeout: Duration::from_secs(30),
            ..options(&format!("redis://{}", server.address), true, true, None)
        })
        .expect("lazy cache")
    });
    let namespace = cache.namespace("cancelled");
    let probe = cache.probe();
    for (cycle, command) in ["SET", "DEL", "PING"].into_iter().enumerate() {
        assert_eq!(namespace.get("ready").await, Ok(None));
        let old = server.stall_existing();
        let late_key = format!("late-{command}");
        let peer_key = format!("peer-{command}");
        let mut late = Box::pin(namespace.get(&late_key));
        let mut peer = Box::pin(namespace.get(&peer_key));
        tokio::select! {
            () = std::future::poll_fn(|context| {
                assert!(late.as_mut().poll(context).is_pending());
                assert!(peer.as_mut().poll(context).is_pending());
                Poll::<()>::Pending
            }) => unreachable!(),
            () = server.wait_for(
                Duration::from_secs(1),
                "peers never reached established socket",
                || server.command_count("GET", Some(&format!("cancelled:{late_key}"))) == 1
                    && server.command_count("GET", Some(&format!("cancelled:{peer_key}"))) == 1,
            ) => {}
        }
        assert_work_metrics(&recorder, 2, 0, cycle);
        let before = server.command_count(command, None);
        let mut cancelled = Box::pin(async {
            match command {
                "SET" => {
                    namespace
                        .set("set-once", b"may-have-landed", Duration::from_secs(1))
                        .await
                }
                "DEL" => namespace.delete("delete-once").await.map_err(crate::SetError::Unavailable),
                _ => probe.check().await.map_err(|_| crate::SetError::Unavailable(crate::Unavailable)),
            }
        });
        tokio::select! {
            result = &mut cancelled => panic!("silent command completed before cancellation: {result:?}"),
            () = server.wait_for(
                Duration::from_secs(1),
                "cancelled command never reached established socket",
                || server.command_count(command, None) > before,
            ) => {}
        }
        assert_work_metrics(&recorder, 3, 0, cycle);
        drop(cancelled);
        assert_work_metrics(&recorder, 2, 0, cycle + 1);
        // No yield after drop: peers already observe retirement, and reused
        // capacity cannot dispatch another command on the old generation.
        assert_eq!(
            poll_once(peer.as_mut()).await,
            Poll::Ready(Err(crate::Unavailable))
        );
        assert_work_metrics(&recorder, 1, 0, cycle + 1);
        let mut replacement = Box::pin(namespace.get("replacement"));
        assert!(poll_once(replacement.as_mut()).await.is_pending());
        assert_work_metrics(&recorder, 2, 0, cycle + 1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(4), replacement)
                .await
                .expect("normal supervisor recovery"),
            Ok(None)
        );
        assert_work_metrics(&recorder, 1, 0, cycle + 1);
        assert!(server.connections() > old);
        // This peer kept its old identity unpolled while the successor served
        // a response. Its late failure must not withdraw that successor.
        assert_eq!(
            poll_once(late.as_mut()).await,
            Poll::Ready(Err(crate::Unavailable))
        );
        assert_work_metrics(&recorder, 0, 0, cycle + 1);
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
    assert_eq!(server.command_count("SET", Some("cancelled:set-once")), 1);
    assert_eq!(
        server.command_count("DEL", Some("cancelled:delete-once")),
        1
    );
    probe
        .check()
        .await
        .expect("healthy probe after cancellation");
}

#[tokio::test]
async fn reliability_final_owner_drop_cancels_inflight_setup() {
    let server = FakeServer::start().await;
    server
        .observed
        .silence_all
        .store(true, std::sync::atomic::Ordering::SeqCst);
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
    let namespace = cache.namespace("context");
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
async fn caller_cutoff_bounds_connection_acquisition() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    server
        .observed
        .silence_all
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let cache = Cache::connect_lazy(CacheOptions {
        command_timeout: Duration::from_secs(5),
        ..options(&format!("redis://{}", server.address), true, true, None)
    })
    .expect("cache");
    let namespace = cache.namespace("acquire");
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
async fn cancellation_after_write_dispatch_retires_once_without_replay() {
    use operation_context::OperationContext;

    let server = FakeServer::start().await;
    let recorder = observation_recorder();
    let cache = metrics::with_local_recorder(&recorder, || {
        Cache::connect_lazy(CacheOptions {
            command_timeout: Duration::from_secs(5),
            ..options(&format!("redis://{}", server.address), true, true, None)
        })
        .expect("cache")
    });
    let namespace = cache.namespace("cancel_effect");
    assert_eq!(namespace.get("ready").await, Ok(None));
    let old = server.stall_existing();
    let context = OperationContext::with_timeout(Duration::from_secs(5));
    let write = namespace.set_with_context("once", b"value", Duration::from_secs(1), &context);
    tokio::pin!(write);
    tokio::select! {
        result = &mut write => panic!("stalled write completed: {result:?}"),
        () = server.wait_for(Duration::from_secs(1), "write must reach provider", || server.command_count("SET", Some("cancel_effect:once")) == 1) => {},
    }
    assert_work_metrics(&recorder, 1, 0, 0);
    context.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(200), write)
            .await
            .expect("cancellation must end waiting before the local ceiling"),
        Err(crate::SetError::Unavailable(crate::Unavailable))
    );
    assert_work_metrics(&recorder, 0, 0, 1);
    server
        .wait_for(
            Duration::from_secs(5),
            "recovery must replace the retired generation",
            || server.connections() > old && server.closed_through(old),
        )
        .await;
    assert_eq!(namespace.get("recovered").await, Ok(None));
    assert_eq!(server.command_count("SET", Some("cancel_effect:once")), 1);
}

#[tokio::test]
async fn reliability_namespace_and_probe_retain_owner_until_live_maintenance_is_cancelled() {
    use health::Probe;

    let server = FakeServer::start().await;
    let recorder = observation_recorder();
    let cache = metrics::with_local_recorder(&recorder, || {
        admitted(&format!("redis://{}", server.address), true, true)
    });
    let namespace = cache.namespace("lifetime");
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
    // Maintenance is outside application/probe admission accounting.
    assert_work_metrics(&recorder, 0, 0, 0);
    drop(probe);
    assert_work_metrics(&recorder, 0, 0, 1);
    server
        .wait_for(
            Duration::from_millis(500),
            "last probe retained cache-owned maintenance",
            || server.active() == 0,
        )
        .await;
    // The supervisor's exit cleanup cannot retire the same generation twice.
    assert_work_metrics(&recorder, 0, 0, 1);
}

#[tokio::test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one retained connection must carry rejection, recovery, file outage and cumulative metric assertions through the same scenario"
)]
async fn reliability_rejected_unchanged_password_recovers_without_traffic() {
    use std::sync::atomic::Ordering;

    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);

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
    let file = tempfile::NamedTempFile::new().expect("password file");
    std::fs::write(file.path(), format!("{initial}\n")).expect("initial password");
    let mut options = with_password_file(
        &format!("redis://{}", server.address),
        file.path().to_path_buf(),
    );
    options.command_timeout = Duration::from_secs(1);
    let cache = Cache::connect_lazy(options).expect("lazy cache");
    let namespace = cache.namespace("retry_password");
    assert_eq!(namespace.get("ready").await, Ok(None));
    assert!(
        password_refresh_samples(&recorder).is_empty(),
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
        password_refresh_samples(&recorder).is_empty(),
        "a pending reply has no completed outcome"
    );
    server.require_password(&pending);
    // Acceptance changes while the rejected reply is still delayed. Every
    // fixture reply arrives within 1 s (the configured command/PING budget).
    // Completion-relative refresh scheduling lets the PINGs at 6, 8.4 and
    // 10.8 s push this controlled recovery past 7 s; the general AUTH hang
    // guard remains the adapter's five seconds.
    // Keep file bytes and sockets unchanged, and issue no cache operations;
    // the client must consume a successful reply, not merely send another AUTH.
    captured
        .wait_for(Duration::from_secs(7), |logs| {
            logs.contains("cache_password_reloaded")
        })
        .await;
    assert_eq!(
        server.connections(),
        connections,
        "the retained-socket recovery bound must not be satisfied by reconnecting"
    );
    assert_eq!(namespace.get("recovered").await, Ok(None));
    assert_eq!(
        password_refresh_samples(&recorder),
        [
            "cache_password_file_refreshes_total{outcome=\"auth_accepted\",reason=\"none\"} 1",
            "cache_password_file_refreshes_total{outcome=\"refresh_failed\",reason=\"auth\"} 1",
        ]
    );

    let authenticated = server.observed.auth_successes.load(Ordering::SeqCst);
    let connections = server.connections();
    std::fs::remove_file(file.path()).expect("temporarily unavailable password file");
    captured
        .wait_for(Duration::from_secs(7), |logs| {
            logs.contains("cache_password_file_unreadable")
        })
        .await;
    assert_eq!(namespace.get("usable-during-file-outage").await, Ok(None));
    assert!(
        password_refresh_samples(&recorder).contains(
            &"cache_password_file_refreshes_total{outcome=\"read_failed\",reason=\"none\"} 1"
                .to_owned()
        )
    );
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
        .await;
    server
        .wait_for(
            Duration::from_secs(7),
            "unchanged successful read was not observed",
            || {
                password_refresh_samples(&recorder)
                    .iter()
                    .any(|sample| sample.contains("outcome=\"unchanged\""))
            },
        )
        .await;
    assert_eq!(
        password_refresh_samples(&recorder),
        [
            "cache_password_file_refreshes_total{outcome=\"auth_accepted\",reason=\"none\"} 2",
            "cache_password_file_refreshes_total{outcome=\"read_failed\",reason=\"none\"} 1",
            "cache_password_file_refreshes_total{outcome=\"refresh_failed\",reason=\"auth\"} 1",
            "cache_password_file_refreshes_total{outcome=\"unchanged\",reason=\"none\"} 1",
        ]
    );
    let scrape = recorder.handle().render();
    for secret in [
        initial.as_str(),
        pending.as_str(),
        later.as_str(),
        file.path().to_str().expect("UTF-8 fixture path"),
    ] {
        assert!(!scrape.contains(secret));
    }
}

fn password_refresh_samples(
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
) -> Vec<String> {
    let mut samples: Vec<_> = recorder
        .handle()
        .render()
        .lines()
        .filter(|line| line.starts_with("cache_password_file_refreshes_total{"))
        .map(str::to_owned)
        .collect();
    samples.sort_unstable();
    samples
}

#[tokio::test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned credential replacement completes before refresh observes it"
)]
async fn password_refresh_timeout_is_observed_but_cancelled_work_is_not() {
    for cancel in [false, true] {
        let recorder = observation_recorder();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let server = FakeServer::start().await;
        let initial = ephemeral_password();
        let replacement = ephemeral_password();
        server.require_password(&initial);
        let file = tempfile::NamedTempFile::new().expect("password file");
        std::fs::write(file.path(), &initial).expect("initial password");
        let cache = Cache::connect_lazy(with_password_file(
            &format!("redis://{}", server.address),
            file.path().to_path_buf(),
        ))
        .expect("lazy cache");
        assert_eq!(
            cache.namespace("refresh_completion").get("ready").await,
            Ok(None)
        );
        let old = server.connections();
        server.require_password(&replacement);
        *server.observed.auth_reply_delay.lock().expect("delay lock") = Duration::from_millis(1500);
        std::fs::write(file.path(), &replacement).expect("replacement password");
        server
            .wait_for(
                Duration::from_secs(7),
                "refresh AUTH was not dispatched",
                || server.command_count("AUTH", None) == 1,
            )
            .await;
        assert!(
            password_refresh_samples(&recorder).is_empty(),
            "sending AUTH does not complete authentication"
        );
        if cancel {
            drop(cache);
        }
        server
            .wait_for(
                Duration::from_secs(2),
                "unfinished refresh did not retire its connection",
                || server.closed_through(old),
            )
            .await;
        let samples = password_refresh_samples(&recorder);
        if cancel {
            assert!(
                samples.is_empty(),
                "cancellation is not a completed refresh"
            );
        } else {
            assert_eq!(
                samples,
                [
                    "cache_password_file_refreshes_total{outcome=\"refresh_failed\",reason=\"timeout\"} 1"
                ]
            );
        }
    }
}

#[tokio::test]
#[allow(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
async fn reliability_password_file_preserves_case_crlf_and_username() {
    for user in [None, Some("ServiceUser")] {
        let server = FakeServer::start().await;
        let password = format!(" MiXeD-{} ", ephemeral_password());
        server.require_password(&password);
        *server.observed.username.lock().expect("username lock") =
            Some(user.unwrap_or("default").to_owned());
        let file = tempfile::NamedTempFile::new().expect("password file");
        std::fs::write(file.path(), format!("{password}\r\n")).expect("password with CRLF");
        let authority = user.map_or_else(
            || server.address.to_string(),
            |user| format!("{user}@{}", server.address),
        );
        let cache = Cache::connect_lazy(with_password_file(
            &format!("redis://{authority}"),
            file.path().to_path_buf(),
        ))
        .expect("lazy cache");
        assert_eq!(
            cache.namespace("credential_bytes").get("ready").await,
            Ok(None),
            "AUTH must preserve password case and spaces, remove one CRLF, and select the correct user"
        );
    }
}

#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    changed: std::sync::Arc<tokio::sync::Notify>,
}

impl CapturedLogs {
    fn rendered(&self) -> String {
        String::from_utf8(self.bytes.lock().expect("logs lock").clone()).expect("UTF-8 logs")
    }

    async fn wait_for(&self, budget: Duration, ready: impl Fn(&str) -> bool) {
        tokio::time::timeout(budget, async {
            loop {
                if ready(&self.rendered()) {
                    break;
                }
                self.changed.notified().await;
            }
        })
        .await
        .expect("expected credential diagnostic was not emitted");
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .expect("logs lock")
            .extend_from_slice(bytes);
        self.changed.notify_one();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
#[allow(
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
        let file = tempfile::NamedTempFile::new().expect("password file");
        std::fs::write(file.path(), format!("{accepted}\n")).expect("initial password");
        let dsn = format!("redis://{}", server.address);
        let cache = Cache::connect_lazy(with_password_file(&dsn, file.path().to_path_buf()))
            .expect("lazy cache");
        let namespace = cache.namespace("logs");
        assert_eq!(namespace.get("sensitive-key").await, Ok(None));
        std::fs::write(file.path(), format!("{rejected}\n")).expect("rejected password");
        tokio::time::timeout(Duration::from_secs(7), async {
            loop {
                let rendered = captured.rendered();
                if rendered.contains("Failed to re-authenticate")
                    || rendered.contains("error.type=\"auth\"")
                    || rendered.contains("error.type=auth")
                {
                    break;
                }
                captured.changed.notified().await;
            }
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
