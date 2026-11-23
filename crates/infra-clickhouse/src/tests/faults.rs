use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use operation_context::{OperationContext, Stopped};
use serde::Serialize;
use tls_test_support::TlsMaterial;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};

use super::{
    SampleRow, base_options, rbwnat_describe_id_column, rbwnat_u64_rows, spawn_http_server,
};
use crate::{
    AsyncDeduplication, AsyncInsertOptions, BatchInsertOptions, ClickHouse, ClientCertificate,
    CloseOutcome, InsertError, InsertFailure, InsertReport, QueryError, QuerySettings, Row,
    UnknownOutcome,
};

async fn spawn_read_fault_server(
    captured_uris: Arc<Mutex<Vec<String>>>,
    stall_entered: Arc<Notify>,
    stall_release: Arc<Notify>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    spawn_http_server(move |req: Request<Incoming>| {
        let uris_req = Arc::clone(&captured_uris);
        let entered = Arc::clone(&stall_entered);
        let release = Arc::clone(&stall_release);
        async move {
            uris_req.lock().await.push(req.uri().to_string());
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let s = String::from_utf8_lossy(&body);
            let (status, resp_body) = match () {
                () if s.contains("zero_rows") => (StatusCode::OK, rbwnat_u64_rows("id", &[])),
                () if s.contains("two_rows") => (StatusCode::OK, rbwnat_u64_rows("id", &[1, 2])),
                () if s.contains("http400_code159") => (
                    StatusCode::BAD_REQUEST,
                    Bytes::from_static(b"Code: 159. DB::Exception: Timeout exceeded"),
                ),
                () if s.contains("http500_code47") => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Bytes::from_static(b"Code: 47. DB::Exception: Unknown column"),
                ),
                () if s.contains("bad_decode") => (StatusCode::OK, rbwnat_u64_rows("wrong", &[1])),
                () if s.contains("stall_hold") => {
                    entered.notify_waiters();
                    release.notified().await;
                    (StatusCode::OK, rbwnat_u64_rows("id", &[7]))
                }
                () if s.contains("stall") => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                () => (StatusCode::OK, rbwnat_u64_rows("id", &[7])),
            };
            Response::builder()
                .status(status)
                .body(Full::new(resp_body))
                .expect("resp")
        }
    })
    .await
}

#[tokio::test]
async fn read_pre_dispatch_limits_and_cardinality() {
    let captured_uris = Arc::new(Mutex::new(Vec::new()));
    let (addr, srv) = spawn_read_fault_server(
        Arc::clone(&captured_uris),
        Arc::new(Notify::new()),
        Arc::new(Notify::new()),
    )
    .await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.max_read_concurrency = 1;
    opts.max_write_concurrency = 1;
    opts.max_result_rows = 2;
    opts.query_timeout = Duration::from_millis(80);
    let ch = ClickHouse::start(opts).await.expect("start");

    let cancelled = OperationContext::unbounded();
    cancelled.cancel();
    assert_eq!(
        ch.query("c", "SELECT 1")
            .fetch_one::<SampleRow>(&cancelled)
            .await,
        Err(QueryError::Stopped(Stopped::Cancelled))
    );
    let expired = OperationContext::with_timeout(Duration::ZERO);
    assert_eq!(
        ch.query("e", "SELECT 1")
            .fetch_one::<SampleRow>(&expired)
            .await,
        Err(QueryError::Stopped(Stopped::Deadline))
    );

    let ctx = OperationContext::with_timeout(Duration::from_secs(2));
    for bad in [
        ch.query("blank", "   \n "),
        ch.query("zero_to", "SELECT 1").timeout(Duration::ZERO),
        ch.query("param_pfx", "SELECT 1").param("param_x", 1_u64),
        ch.query("param_empty", "SELECT 1").param("", 1_u64),
        ch.query("param_digit", "SELECT 1").param("1a", 1_u64),
        ch.query("param_dup", "SELECT 1")
            .param("x", 1_u64)
            .param("x", 2_u64),
    ] {
        assert_eq!(
            bad.fetch_all::<SampleRow>(&ctx).await,
            Err(QueryError::Rejected)
        );
    }
    assert_eq!(captured_uris.lock().await.len(), 0);

    let two = ch
        .query("exact_two", "SELECT two_rows")
        .max_result_rows(NonZeroU64::new(999).expect("nz"))
        .fetch_all::<SampleRow>(&ctx)
        .await
        .expect("== 2 rows succeeds");
    assert_eq!(two.len(), 2);
    assert!(
        captured_uris
            .lock()
            .await
            .last()
            .expect("uri")
            .contains("max_result_rows=2")
    );

    assert_eq!(
        ch.query("one_0", "SELECT zero_rows")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::NotFound)
    );
    assert_eq!(
        ch.query("opt_0", "SELECT zero_rows")
            .fetch_optional::<SampleRow>(&ctx)
            .await,
        Ok(None)
    );
    assert_eq!(
        ch.query("one_2", "SELECT two_rows")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::TooManyRows)
    );
    assert_eq!(
        ch.query("opt_2", "SELECT two_rows")
            .fetch_optional::<SampleRow>(&ctx)
            .await,
        Err(QueryError::TooManyRows)
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

async fn assert_server_settings_and_closed_read(
    addr: std::net::SocketAddr,
    captured_uris: &Arc<Mutex<Vec<String>>>,
    ch: ClickHouse,
    ctx: &OperationContext,
) {
    let mut srv_opts = base_options(format!("http://{addr}"));
    srv_opts.query_settings = QuerySettings::Server;
    let ch_srv = ClickHouse::start(srv_opts).await.expect("start server");
    let before = captured_uris.lock().await.len();
    assert_eq!(
        ch_srv
            .query("ro", "SELECT 1")
            .fetch_one::<SampleRow>(ctx)
            .await,
        Ok(SampleRow { id: 7 })
    );
    let ro_uri = captured_uris.lock().await[before].clone();
    assert!(ro_uri.contains("wait_end_of_query=1") && ro_uri.contains("buffer_size="));
    for forbidden in [
        "max_execution_time",
        "max_result_rows",
        "max_result_bytes",
        "max_memory_usage",
        "readonly=",
        "session_timezone=",
        "cancel_http_readonly",
    ] {
        assert!(!ro_uri.contains(forbidden), "{ro_uri}");
    }

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    assert_eq!(
        ch_srv.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    let count_after_close = captured_uris.lock().await.len();
    assert_eq!(
        ch.query("closed", "SELECT 1")
            .fetch_one::<SampleRow>(ctx)
            .await,
        Err(QueryError::Unavailable)
    );
    assert_eq!(captured_uris.lock().await.len(), count_after_close);
}

#[tokio::test]
async fn read_precedence_bytes_cap_busy_and_server_settings() {
    let captured_uris = Arc::new(Mutex::new(Vec::new()));
    let stall_entered = Arc::new(Notify::new());
    let stall_release = Arc::new(Notify::new());
    let (addr, srv) = spawn_read_fault_server(
        Arc::clone(&captured_uris),
        Arc::clone(&stall_entered),
        Arc::clone(&stall_release),
    )
    .await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.max_read_concurrency = 1;
    opts.max_write_concurrency = 1;
    opts.query_timeout = Duration::from_secs(2);
    let ch = ClickHouse::start(opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(2));

    assert_eq!(
        ch.query("http400_159", "SELECT http400_code159")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::ResourceLimit)
    );
    assert!(!QueryError::ResourceLimit.is_retryable());
    assert_eq!(
        ch.query("http500_47", "SELECT http500_code47")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Rejected)
    );
    assert_eq!(
        ch.query("decode", "SELECT bad_decode")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Decode)
    );
    assert_eq!(
        ch.query("local_to", "SELECT stall")
            .timeout(Duration::from_millis(30))
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Unavailable)
    );

    let entered = stall_entered.notified();
    tokio::pin!(entered);
    entered.as_mut().enable();
    let ch_busy = ch.clone();
    let ctx_busy = ctx.clone();
    let holder = tokio::spawn(async move {
        ch_busy
            .query("holder", "SELECT stall_hold")
            .fetch_one::<SampleRow>(&ctx_busy)
            .await
    });
    entered.await;
    assert_eq!(
        ch.query("shed", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Busy)
    );
    stall_release.notify_waiters();
    assert_eq!(holder.await.expect("join holder"), Ok(SampleRow { id: 7 }));

    let raw_bytes = rbwnat_u64_rows("id", &[7]);
    assert_eq!(
        ch.query("bytes_eq", "SELECT 1")
            .max_result_bytes(NonZeroUsize::new(raw_bytes.len()).expect("nz"))
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Ok(SampleRow { id: 7 })
    );
    assert_eq!(
        ch.query("bytes_over", "SELECT 1")
            .max_result_bytes(NonZeroUsize::new(raw_bytes.len() - 1).expect("nz"))
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::TooLarge)
    );

    assert_server_settings_and_closed_read(addr, &captured_uris, ch, &ctx).await;
    srv.abort();
}

async fn spawn_write_fault_server(
    captured_uris: Arc<Mutex<Vec<String>>>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    spawn_http_server(move |req: Request<Incoming>| {
        let uris_req = Arc::clone(&captured_uris);
        async move {
            let uri = req.uri().to_string();
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if body_str.contains("DESCRIBE") || uri.contains("DESCRIBE") {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            uris_req.lock().await.push(uri.clone());
            let (status, body) = match () {
                () if uri.contains("fail_252") => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Bytes::from_static(b"Code: 252. DB::Exception: Too many parts"),
                ),
                () if uri.contains("fail_159") => (
                    StatusCode::BAD_REQUEST,
                    Bytes::from_static(b"Code: 159. DB::Exception: Timeout"),
                ),
                () if uri.contains("fail_319") => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Bytes::from_static(
                        b"Code: 319. DB::Exception: Unknown status of distributed insert",
                    ),
                ),
                () if uri.contains("fail_999") => (
                    StatusCode::BAD_REQUEST,
                    Bytes::from_static(b"Code: 999. DB::Exception: Unrecognized"),
                ),
                () if uri.contains("fail_prose") => (
                    StatusCode::BAD_REQUEST,
                    Bytes::from_static(b"400 gateway prose"),
                ),
                () => (StatusCode::OK, Bytes::new()),
            };
            Response::builder()
                .status(status)
                .body(Full::new(body))
                .expect("resp")
        }
    })
    .await
}

async fn assert_write_code_precedence_and_close(ch: ClickHouse, ctx: &OperationContext) {
    for (table, expected) in [
        (
            "fail_252",
            InsertError::NotApplied(InsertFailure::TooManyParts),
        ),
        (
            "fail_159",
            InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        ),
        (
            "fail_319",
            InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        ),
        (
            "fail_999",
            InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        ),
        (
            "fail_prose",
            InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        ),
    ] {
        assert_eq!(
            ch.insert_batch(
                ctx,
                "code_probe",
                table,
                &[SampleRow { id: 1 }],
                BatchInsertOptions::new()
            )
            .await,
            Err(expected),
            "{table}"
        );
    }

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    assert_eq!(
        ch.insert_batch(
            ctx,
            "closed",
            "events",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new()
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Unavailable))
    );
}

#[tokio::test]
async fn write_pre_dispatch_validation_and_empty_noop() {
    let captured_uris = Arc::new(Mutex::new(Vec::new()));
    let (addr, srv) = spawn_write_fault_server(Arc::clone(&captured_uris)).await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let bad_table_long = "a".repeat(129);
    for table in [
        "",
        "db.table",
        "t a",
        "t`",
        "t'",
        "t\"",
        "t;",
        "t\n",
        &bad_table_long,
    ] {
        assert_eq!(
            ch.insert_batch(
                &ctx,
                "bad_table",
                table,
                &[SampleRow { id: 1 }],
                BatchInsertOptions::new()
            )
            .await,
            Err(InsertError::NotApplied(InsertFailure::InvalidInput))
        );
    }
    let bad_tok_long = "k".repeat(257);
    for token in ["", "tok\n", "tok\0", &bad_tok_long] {
        assert_eq!(
            ch.insert_async(
                &ctx,
                "bad_tok",
                "events",
                &[SampleRow { id: 1 }],
                AsyncInsertOptions::with_token(token)
            )
            .await,
            Err(InsertError::NotApplied(InsertFailure::InvalidInput))
        );
    }
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "zero_to",
            "events",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new().timeout(Duration::ZERO)
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::InvalidInput))
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "empty",
            "events",
            &[] as &[SampleRow],
            BatchInsertOptions::new()
        )
        .await,
        Ok(InsertReport { rows: 0 })
    );
    assert_eq!(captured_uris.lock().await.len(), 0);
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

#[tokio::test]
async fn write_settings_idx_nonzero_and_code_precedence() {
    #[derive(Row)]
    #[clickhouse(crate = "crate::__clickhouse")]
    struct SecondFailRow {
        id: u64,
    }

    impl Serialize for SecondFailRow {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if self.id == 2 {
                return Err(serde::ser::Error::custom("second row boom"));
            }
            self.id.serialize(serializer)
        }
    }

    let captured_uris = Arc::new(Mutex::new(Vec::new()));
    let (addr, srv) = spawn_write_fault_server(Arc::clone(&captured_uris)).await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    ch.insert_batch(
        &ctx,
        "batch",
        "events",
        &[SampleRow { id: 1 }],
        BatchInsertOptions::new().deduplication_token("tok-b"),
    )
    .await
    .expect("batch ok");
    ch.insert_async(
        &ctx,
        "async_disabled",
        "events",
        &[SampleRow { id: 2 }],
        AsyncInsertOptions::without_deduplication(),
    )
    .await
    .expect("async disabled ok");
    ch.insert_async(
        &ctx,
        "async_hash",
        "events",
        &[SampleRow { id: 3 }],
        AsyncInsertOptions::with_payload_hash(),
    )
    .await
    .expect("async hash ok");
    ch.insert_async(
        &ctx,
        "async_tok",
        "events",
        &[SampleRow { id: 4 }],
        AsyncInsertOptions::new(AsyncDeduplication::ByToken("tok-a")),
    )
    .await
    .expect("async tok ok");

    assert_captured_write_uris(&captured_uris.lock().await);

    assert_eq!(
        ch.insert_batch(
            &ctx,
            "idx1_fail",
            "events",
            &[SecondFailRow { id: 1 }, SecondFailRow { id: 2 }],
            BatchInsertOptions::new(),
        )
        .await,
        Err(InsertError::OutcomeUnknown(UnknownOutcome::Unavailable))
    );

    assert_write_code_precedence_and_close(ch, &ctx).await;
    srv.abort();
}

fn assert_captured_write_uris(uris: &[String]) {
    assert!(
        uris[0].contains("async_insert=0")
            && uris[0].contains("max_execution_time=")
            && uris[0].contains("timeout_overflow_mode=throw")
            && uris[0].contains("max_memory_usage=536870912")
            && uris[0].contains("deduplicate_insert=enable")
            && uris[0].contains("insert_deduplication_token=tok-b"),
        "{}",
        uris[0]
    );
    assert!(
        uris[1].contains("async_insert=1")
            && uris[1].contains("wait_for_async_insert=1")
            && uris[1].contains("wait_for_async_insert_timeout=")
            && uris[1].contains("max_execution_time=")
            && uris[1].contains("max_memory_usage=536870912")
            && uris[1].contains("deduplicate_insert=disable"),
        "{}",
        uris[1]
    );
    assert!(
        uris[2].contains("async_insert=1")
            && uris[2].contains("wait_for_async_insert=1")
            && uris[2].contains("wait_for_async_insert_timeout=")
            && uris[2].contains("deduplicate_insert=enable"),
        "{}",
        uris[2]
    );
    assert!(
        uris[3].contains("async_insert=1")
            && uris[3].contains("wait_for_async_insert=1")
            && uris[3].contains("wait_for_async_insert_timeout=")
            && uris[3].contains("deduplicate_insert=enable")
            && uris[3].contains("insert_deduplication_token=tok-a"),
        "{}",
        uris[3]
    );
}

struct MtlsMaterials {
    ca_pem: String,
    client_cert_pem: String,
    client_key_pem: String,
    acceptor: tokio_rustls::TlsAcceptor,
}

fn build_mtls_materials() -> MtlsMaterials {
    let mut ca_params = rcgen::CertificateParams::default();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca_key = rcgen::KeyPair::generate().expect("ca key");
    let issuer = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("issuer");

    let mut srv_params =
        rcgen::CertificateParams::new(vec!["localhost".to_owned()]).expect("srv san");
    srv_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let srv_key = rcgen::KeyPair::generate().expect("srv key");
    let srv_cert = srv_params.signed_by(&srv_key, &issuer).expect("srv cert");

    let mut cli_params = rcgen::CertificateParams::new(vec!["client".to_owned()]).expect("cli san");
    cli_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let cli_key = rcgen::KeyPair::generate().expect("cli key");
    let cli_cert = cli_params.signed_by(&cli_key, &issuer).expect("cli cert");

    let mut roots = rustls::RootCertStore::empty();
    roots.add(issuer.der().clone()).expect("add ca");
    let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .expect("verifier");
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
        srv_key.serialize_der(),
    ));
    let mut srv_cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versions")
    .with_client_cert_verifier(verifier)
    .with_single_cert(vec![srv_cert.der().clone()], key)
    .expect("server cfg");
    srv_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];

    MtlsMaterials {
        ca_pem: issuer.pem(),
        client_cert_pem: cli_cert.pem(),
        client_key_pem: cli_key.serialize_pem(),
        acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(srv_cfg)),
    }
}

#[tokio::test]
async fn tls_handshake_failure_is_not_applied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let server_tls = TlsMaterial::new("localhost");
    let wrong_ca = TlsMaterial::new("localhost");

    let untrusted_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let untrusted_addr = untrusted_listener.local_addr().expect("addr");
    let acceptor = server_tls.acceptor();
    let untrusted_srv = tokio::spawn(async move {
        while let Ok((tcp, _)) = untrusted_listener.accept().await {
            let acc = acceptor.clone();
            tokio::spawn(async move {
                let _ = acc.accept(tcp).await;
            });
        }
    });

    let wrong_ca_path = dir.path().join("wrong_ca.pem");
    tokio::fs::write(&wrong_ca_path, &wrong_ca.root_pem)
        .await
        .expect("write ca");
    let mut bad_tls_opts = base_options(format!("https://localhost:{}", untrusted_addr.port()));
    bad_tls_opts.allow_plaintext = false;
    bad_tls_opts.root_ca_path = Some(wrong_ca_path);
    let ch_bad = ClickHouse::start(bad_tls_opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(2));
    assert_eq!(
        ch_bad
            .query("q", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Unavailable)
    );
    assert_eq!(
        ch_bad
            .insert_batch(
                &ctx,
                "ins",
                "events",
                &[SampleRow { id: 1 }],
                BatchInsertOptions::new()
            )
            .await,
        Err(InsertError::NotApplied(InsertFailure::Unavailable))
    );
    assert_eq!(
        ch_bad.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    untrusted_srv.abort();
}

async fn spawn_mtls_alert_server(
    mtls_acceptor: tokio_rustls::TlsAcceptor,
    req_index: Arc<AtomicUsize>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let mtls_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mtls");
    let mtls_addr = mtls_listener.local_addr().expect("mtls addr");
    let srv = tokio::spawn(async move {
        while let Ok((tcp, _)) = mtls_listener.accept().await {
            let acc = mtls_acceptor.clone();
            let idx_srv = Arc::clone(&req_index);
            tokio::spawn(async move {
                let Ok(mut tls_stream) = acc.accept(tcp).await else {
                    return;
                };
                let mut req_buf = vec![0_u8; 4096];
                let _ = tls_stream.read(&mut req_buf).await;
                if idx_srv.fetch_add(1, Ordering::SeqCst) == 0 {
                    let body = rbwnat_describe_id_column();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = tls_stream.write_all(resp.as_bytes()).await;
                    let _ = tls_stream.write_all(&body).await;
                    let _ = tls_stream.shutdown().await;
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
                let (mut raw_tcp, _) = tls_stream.into_inner();
                let _ = raw_tcp
                    .write_all(&[0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28])
                    .await;
                let _ = raw_tcp.shutdown().await;
            });
        }
    });
    (mtls_addr, srv)
}

#[tokio::test]
async fn response_phase_tls_fault_is_outcome_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mtls = build_mtls_materials();
    let ca_path = dir.path().join("ca.pem");
    let cert_path = dir.path().join("client.pem");
    let key_path = dir.path().join("client.key");
    tokio::fs::write(&ca_path, &mtls.ca_pem).await.expect("ca");
    tokio::fs::write(&cert_path, &mtls.client_cert_pem)
        .await
        .expect("cert");
    tokio::fs::write(&key_path, &mtls.client_key_pem)
        .await
        .expect("key");

    let req_index = Arc::new(AtomicUsize::new(0));
    let (mtls_addr, mtls_srv) =
        spawn_mtls_alert_server(mtls.acceptor, Arc::clone(&req_index)).await;

    let ctx = OperationContext::with_timeout(Duration::from_secs(2));
    let mut valid_opts = base_options(format!("https://localhost:{}", mtls_addr.port()));
    valid_opts.allow_plaintext = false;
    valid_opts.root_ca_path = Some(ca_path);
    valid_opts.client_certificate = Some(ClientCertificate {
        cert_path,
        key_path,
    });
    let ch_mtls = ClickHouse::start(valid_opts).await.expect("start mtls");
    let err = ch_mtls
        .insert_batch(
            &ctx,
            "tls_response_alert",
            "events",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new(),
        )
        .await
        .expect_err("response-phase TLS alert must fail closed");
    assert!(req_index.load(Ordering::SeqCst) >= 2);
    assert_eq!(
        err,
        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
        "response-phase TLS failure must NEVER return NotApplied"
    );
    assert_eq!(
        ch_mtls.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    mtls_srv.abort();
}

async fn spawn_stallable_drain_server() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    Arc<Notify>,
    Arc<Notify>,
    tokio::task::JoinHandle<()>,
) {
    let requests = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let reqs_srv = Arc::clone(&requests);
    let entered_srv = Arc::clone(&entered);
    let release_srv = Arc::clone(&release);
    let (addr, srv) = spawn_http_server(move |req: Request<Incoming>| {
        let reqs = Arc::clone(&reqs_srv);
        let entered = Arc::clone(&entered_srv);
        let release = Arc::clone(&release_srv);
        async move {
            reqs.fetch_add(1, Ordering::SeqCst);
            let uri = req.uri().to_string();
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if uri.contains("DESCRIBE") || body_str.contains("DESCRIBE") {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            if uri.contains("stall") || body_str.contains("stall") {
                entered.notify_one();
                release.notified().await;
            }
            let resp = if uri.contains("INSERT") || body_str.contains("INSERT") {
                Bytes::new()
            } else {
                rbwnat_u64_rows("id", &[7])
            };
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(resp))
                .expect("resp")
        }
    })
    .await;

    (addr, requests, entered, release, srv)
}

#[tokio::test]
async fn close_drains_in_flight_operations_and_times_out() {
    let (addr, requests, entered, release, srv) = spawn_stallable_drain_server().await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let ch_q = ch.clone();
    let ctx_q = ctx.clone();
    let q_entered = entered.notified();
    tokio::pin!(q_entered);
    q_entered.as_mut().enable();
    let q_task = tokio::spawn(async move {
        ch_q.query("drain_q", "SELECT stall")
            .fetch_one::<SampleRow>(&ctx_q)
            .await
    });
    q_entered.await;

    let ch_i = ch.clone();
    let ctx_i = ctx.clone();
    let i_entered = entered.notified();
    tokio::pin!(i_entered);
    i_entered.as_mut().enable();
    let i_task = tokio::spawn(async move {
        ch_i.insert_batch(
            &ctx_i,
            "drain_i",
            "events_stall",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new(),
        )
        .await
    });
    i_entered.await;

    assert_eq!(
        ch.close(Duration::from_millis(20)).await,
        CloseOutcome::TimedOut
    );
    let requests_at_close = requests.load(Ordering::SeqCst);
    assert_eq!(
        ch.query("post_close_q", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Unavailable)
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "post_close_i",
            "events",
            &[SampleRow { id: 2 }],
            BatchInsertOptions::new(),
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Unavailable))
    );
    assert_eq!(requests.load(Ordering::SeqCst), requests_at_close);

    let ch_close = ch.clone();
    let close_task = tokio::spawn(async move { ch_close.close(Duration::from_secs(5)).await });
    tokio::task::yield_now().await;
    assert!(!close_task.is_finished());

    release.notify_waiters();
    assert_eq!(q_task.await.expect("join q"), Ok(SampleRow { id: 7 }));
    assert_eq!(i_task.await.expect("join i"), Ok(InsertReport { rows: 1 }));
    assert_eq!(
        close_task.await.expect("join close"),
        CloseOutcome::Complete
    );
    srv.abort();
}

#[tokio::test]
async fn close_token_barrier_rejects_post_close_tasks_without_io() {
    let (addr, requests, _entered, _release, srv) = spawn_stallable_drain_server().await;
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start barrier");

    ch.inner.shared.tasks.close();
    assert_eq!(
        ch.query("barrier_q", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Unavailable)
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "barrier_i",
            "events",
            &[SampleRow { id: 3 }],
            BatchInsertOptions::new(),
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Unavailable))
    );
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}
