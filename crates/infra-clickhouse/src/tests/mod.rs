mod a7_stress;
mod admission;
mod audit_remediations;
mod faults;
mod metadata;
mod rotation;
mod telemetry;

use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusRecorder};
use operation_context::OperationContext;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tls_test_support::TlsMaterial;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};
use tokio::task::JoinHandle;

use crate::{
    AsyncInsertOptions, BatchInsertOptions, ClickHouse, ClickHouseOptions, CloseOutcome,
    HISTOGRAMS, InsertError, InsertFailure, QueryError, QuerySettings, Row, UnknownOutcome, error,
};

pub(super) fn base_options(url: String) -> ClickHouseOptions {
    ClickHouseOptions {
        url,
        database: "app".to_owned(),
        username: Some("app".to_owned()),
        password: Some(SecretString::from("app")),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
        allow_plaintext: true,
        allow_unauthenticated: false,
        query_settings: QuerySettings::Client,
        query_timeout: Duration::from_secs(5),
        insert_timeout: Duration::from_secs(5),
        max_read_concurrency: 32,
        max_write_concurrency: 32,
        max_result_rows: 100_000,
        max_result_bytes: 16 * 1024 * 1024,
        http_response_buffer_size: 2 * 1024 * 1024,
        max_memory_usage: Some(512 * 1024 * 1024),
        propagate_trace_context: true,
    }
}

pub(super) fn prometheus_recorder() -> PrometheusRecorder {
    let mut builder = PrometheusBuilder::new();
    for &(name, buckets) in HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
            .expect("valid clickhouse histogram buckets");
    }
    builder.build_recorder()
}

pub(super) fn encode_varuint(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = u8::try_from(value & 0x7f).expect("masked to 7 bits");
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

pub(super) fn encode_ch_str(s: &str, out: &mut Vec<u8>) {
    encode_varuint(s.len(), out);
    out.extend_from_slice(s.as_bytes());
}

pub(super) fn rbwnat_u64_rows(col_name: &str, values: &[u64]) -> Bytes {
    let mut out = Vec::new();
    encode_varuint(1, &mut out);
    encode_ch_str(col_name, &mut out);
    encode_ch_str("UInt64", &mut out);
    for &v in values {
        out.extend_from_slice(&v.to_le_bytes());
    }
    Bytes::from(out)
}

pub(super) fn rbwnat_describe_id_column() -> Bytes {
    let mut out = Vec::new();
    encode_varuint(7, &mut out);
    for col in [
        "name",
        "type",
        "default_type",
        "default_expression",
        "comment",
        "codec_expression",
        "ttl_expression",
    ] {
        encode_ch_str(col, &mut out);
    }
    for _ in 0..7 {
        encode_ch_str("String", &mut out);
    }
    encode_ch_str("id", &mut out);
    encode_ch_str("UInt64", &mut out);
    for _ in 0..5 {
        encode_ch_str("", &mut out);
    }
    Bytes::from(out)
}

#[derive(Clone, Debug)]
pub(super) struct CapturedRequest {
    pub(super) method: Method,
    pub(super) uri: String,
    pub(super) content_type: Option<String>,
    pub(super) body: Bytes,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
pub(super) struct SampleRow {
    pub(super) id: u64,
}

pub(super) async fn spawn_http_server<F, Fut>(handler: F) -> (SocketAddr, JoinHandle<()>)
where
    F: Fn(Request<Incoming>) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<Full<Bytes>>> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let h = handler.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let h = h.clone();
                    async move { Ok::<_, hyper::Error>(h(req).await) }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, handle)
}

async fn spawn_http_capture_server(
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let captured_conn = Arc::clone(&captured);
            tokio::spawn(serve_capture_conn(stream, captured_conn));
        }
    });
    (addr, handle)
}

async fn serve_capture_conn(stream: TcpStream, captured: Arc<Mutex<Vec<CapturedRequest>>>) {
    let svc = service_fn(move |req: Request<Incoming>| {
        let captured_req = Arc::clone(&captured);
        async move {
            let method = req.method().clone();
            let uri = req.uri().to_string();
            let content_type = req
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = req
                .into_body()
                .collect()
                .await
                .expect("collect body")
                .to_bytes();
            let body_text = String::from_utf8_lossy(&body);
            let resp_body = if body_text.contains("two_rows") {
                rbwnat_u64_rows("id", &[10, 20])
            } else {
                rbwnat_u64_rows("id", &[42])
            };
            captured_req.lock().await.push(CapturedRequest {
                method,
                uri,
                content_type,
                body,
            });
            Ok::<_, hyper::Error>(
                Response::builder()
                    .status(StatusCode::OK)
                    .header(
                        "X-ClickHouse-Summary",
                        r#"{"read_rows":"1","read_bytes":"8"}"#,
                    )
                    .body(Full::new(resp_body))
                    .expect("build response"),
            )
        }
    });
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), svc)
        .await;
}

#[tokio::test]
async fn a4_1_and_a4_3_compile_and_send_futures() {
    fn assert_send<F: Future + Send>(_: &F) {}

    let client = ClickHouse::start(base_options("http://127.0.0.1:8123".to_owned()))
        .await
        .expect("start plaintext dev client");
    assert_eq!(HISTOGRAMS[0].0, "clickhouse_operation_duration_seconds");
    assert_eq!(HISTOGRAMS[0].1.len(), 12);

    let ctx = OperationContext::unbounded();
    let rows = [SampleRow { id: 1 }];
    let batch_fut = client.insert_batch(
        &ctx,
        "batch_smoke",
        "events",
        &rows,
        BatchInsertOptions::new(),
    );
    assert_send(&batch_fut);
    let async_fut = client.insert_async(
        &ctx,
        "async_smoke",
        "events",
        &rows,
        AsyncInsertOptions::with_payload_hash(),
    );
    assert_send(&async_fut);
    let read_fut = client
        .query("read_smoke", "SELECT 1")
        .fetch_one::<SampleRow>(&ctx);
    assert_send(&read_fut);

    assert_eq!(
        client.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn a4_4_and_a4_6_cursor_metrics_bounds_and_multipart_params() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let (addr, server) = spawn_http_capture_server(Arc::clone(&captured)).await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(2));

    let secret_str = "super-secret-param-value";
    let uid = uuid::Uuid::new_v4();
    let date = time::Date::from_calendar_date(2026, time::Month::March, 15).expect("date");
    let dt = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("dt");
    let slice_items: &[&str] = &["alpha", "beta"];

    let row = ch
        .query("param_probe", "SELECT id FROM t WHERE s = {s:String}")
        .param("s", secret_str)
        .param("flag", true)
        .param("num", 7_i64)
        .param("flt", 1.5_f64)
        .param("uid", uid)
        .param("d", date)
        .param("dt", dt)
        .param("opt", Option::<u32>::None)
        .param("arr", slice_items)
        .param("vec", vec![1_i32, 2_i32])
        .fetch_one::<SampleRow>(&ctx)
        .await
        .expect("fetch_one");
    assert_eq!(row.id, 42);

    let reqs = captured.lock().await;
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.method, Method::POST);
    assert!(
        !req.uri.contains("param_")
            && !req.uri.contains(secret_str)
            && !req.uri.contains(&uid.to_string()),
        "URI leaked bound parameters: {}",
        req.uri
    );
    assert!(
        req.uri.contains("wait_end_of_query=1"),
        "missing wait_end_of_query: {}",
        req.uri
    );
    assert!(
        req.uri.contains("buffer_size="),
        "missing buffer_size: {}",
        req.uri
    );
    assert!(
        req.uri.contains("query_id="),
        "missing query_id: {}",
        req.uri
    );
    let ct = req.content_type.as_deref().unwrap_or("");
    assert!(
        ct.starts_with("multipart/form-data; boundary="),
        "unexpected content-type: {ct}"
    );
    let body_text = String::from_utf8_lossy(&req.body);
    assert!(body_text.contains("name=\"param_s\""));
    assert!(body_text.contains(secret_str));
    assert!(body_text.contains(&uid.to_string()));
    assert!(body_text.contains("2026-03-15"));
    assert!(body_text.contains("['alpha','beta']"));
    drop(reqs);

    let err = ch
        .query("row_limit_probe", "SELECT two_rows")
        .max_result_rows(NonZeroU64::new(1).expect("nz"))
        .fetch_all::<SampleRow>(&ctx)
        .await
        .expect_err("2 rows with limit 1 must fail");
    assert_eq!(err, QueryError::TooLarge);

    let err_bytes = ch
        .query("byte_limit_probe", "SELECT two_rows")
        .max_result_bytes(NonZeroUsize::new(10).expect("nz"))
        .fetch_all::<SampleRow>(&ctx)
        .await
        .expect_err("byte limit must fail");
    assert_eq!(err_bytes, QueryError::TooLarge);

    let raw_client = clickhouse::Client::default().with_url(format!("http://{addr}"));
    let mut cursor = raw_client
        .query("SELECT id")
        .fetch::<SampleRow>()
        .expect("cursor");
    assert!(cursor.next().await.expect("row").is_some());
    assert_eq!(cursor.returned_rows(), 1);
    assert!(cursor.decoded_bytes() > 0);
    let summary = cursor.summary().expect("summary header");
    assert_eq!(summary.read_rows(), Some(1));

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    server.abort();
}

async fn serve_metadata_conn(stream: TcpStream, describe_calls: Arc<Mutex<Vec<String>>>) {
    let svc = service_fn(move |req: Request<Incoming>| {
        let describe_req = Arc::clone(&describe_calls);
        async move {
            let uri = req.uri().to_string();
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if uri.contains("DESCRIBE") || body_str.contains("DESCRIBE") {
                describe_req.lock().await.push(uri);
                return Ok::<_, hyper::Error>(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Full::new(rbwnat_describe_id_column()))
                        .expect("resp"),
                );
            }
            Ok::<_, hyper::Error>(
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::new()))
                    .expect("resp"),
            )
        }
    });
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), svc)
        .await;
}

#[tokio::test]
async fn a4_5_with_database_isolates_metadata_while_with_password_and_header_preserve_cache_and_pool()
 {
    let tcp_accepts = Arc::new(AtomicUsize::new(0));
    let describe_calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let tcp_accepts_srv = Arc::clone(&tcp_accepts);
    let describe_srv = Arc::clone(&describe_calls);

    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tcp_accepts_srv.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(serve_metadata_conn(stream, Arc::clone(&describe_srv)));
        }
    });

    let hyper_client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .pool_timer(TokioTimer::new())
        .pool_idle_timeout(Duration::from_secs(30))
        .build(HttpConnector::new());
    let base =
        clickhouse::Client::with_http_client(hyper_client).with_url(format!("http://{addr}"));
    let client_a = base.clone().with_database("db_a");
    let client_b = base.clone().with_database("db_b");

    let mut ins = client_a
        .insert::<SampleRow>("events")
        .await
        .expect("ins a1");
    ins.write(&SampleRow { id: 1 }).await.expect("w");
    ins.end().await.expect("e");

    let client_a_rotated = client_a
        .clone()
        .with_password("new_pw")
        .with_header("X-Rotation", "2");
    let mut ins2 = client_a_rotated
        .insert::<SampleRow>("events")
        .await
        .expect("ins a2");
    ins2.write(&SampleRow { id: 2 }).await.expect("w");
    ins2.end().await.expect("e");

    let mut ins3 = client_b
        .insert::<SampleRow>("events")
        .await
        .expect("ins b1");
    ins3.write(&SampleRow { id: 3 }).await.expect("w");
    ins3.end().await.expect("e");

    let describes = describe_calls.lock().await;
    assert_eq!(
        describes.len(),
        2,
        "expected 1 DESCRIBE per db, got: {describes:?}"
    );
    assert!(describes[0].contains("database=db_a"));
    assert!(describes[1].contains("database=db_b"));
    assert_eq!(
        tcp_accepts.load(Ordering::SeqCst),
        1,
        "expected connection pool reuse across with_password/with_header/with_database"
    );

    server.abort();
}

fn bad_response_for_query(body_str: &str) -> (StatusCode, &'static str) {
    if body_str.contains("ch_500_158") {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Code: 158. DB::Exception: limit exceeded",
        );
    }
    if body_str.contains("empty_400") {
        return (StatusCode::BAD_REQUEST, "");
    }
    if body_str.contains("prose_400") {
        return (StatusCode::BAD_REQUEST, "400 rows written");
    }
    if body_str.contains("empty_401") {
        return (StatusCode::UNAUTHORIZED, "");
    }
    if body_str.contains("prose_401") {
        return (StatusCode::UNAUTHORIZED, "401 custom text");
    }
    if body_str.contains("body_401") {
        return (StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if body_str.contains("empty_403") {
        return (StatusCode::FORBIDDEN, "");
    }
    if body_str.contains("empty_413") {
        return (StatusCode::PAYLOAD_TOO_LARGE, "");
    }
    if body_str.contains("body_413") {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request entity too large");
    }
    (StatusCode::INTERNAL_SERVER_ERROR, "")
}

async fn spawn_bad_response_server() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind bad");
    let addr = listener.local_addr().expect("bad addr");
    let handle = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let svc = service_fn(|req: Request<Incoming>| async move {
                    let body = req.into_body().collect().await.expect("body").to_bytes();
                    let body_str = String::from_utf8_lossy(&body);
                    let (status, resp_body) = bad_response_for_query(&body_str);
                    Ok::<_, hyper::Error>(
                        Response::builder()
                            .status(status)
                            .body(Full::new(Bytes::from_static(resp_body.as_bytes())))
                            .expect("resp"),
                    )
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, handle)
}

#[tokio::test]
async fn a4_7_error_network_is_connect_and_bad_response_probes() {
    let closed_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let closed_addr = closed_listener.local_addr().expect("addr");
    drop(closed_listener);

    let raw_closed = clickhouse::Client::default().with_url(format!("http://{closed_addr}"));
    let err = raw_closed
        .query("SELECT 1")
        .fetch_one::<u64>()
        .await
        .expect_err("connect refused");
    assert!(matches!(err, clickhouse::error::Error::Network(_)));
    assert!(error::is_hyper_connect_error(&err));

    let tls_material = TlsMaterial::new("localhost");
    let tls_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind tls");
    let tls_addr = tls_listener.local_addr().expect("tls addr");
    let acceptor = tls_material.acceptor();
    let tls_srv = tokio::spawn(async move {
        if let Ok((tcp, _)) = tls_listener.accept().await {
            let _ = acceptor.accept(tcp).await;
        }
    });

    let mut http = HttpConnector::new();
    http.enforce_http(false);
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let tls_cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_cfg)
        .https_only()
        .enable_http1()
        .wrap_connector(http);
    let hyper_tls =
        hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(connector);
    let raw_tls = clickhouse::Client::with_http_client(hyper_tls)
        .with_url(format!("https://127.0.0.1:{}", tls_addr.port()));
    let tls_err = raw_tls
        .query("SELECT 1")
        .fetch_one::<u64>()
        .await
        .expect_err("untrusted cert handshake fail");
    assert!(matches!(tls_err, clickhouse::error::Error::Network(_)));
    assert!(error::is_hyper_connect_error(&tls_err));
    tls_srv.abort();
}

const A4_7_LOOPBACK_CASES: &[(&str, &str, QueryError, InsertFailure, InsertError)] = &[
    (
        "SELECT empty_400",
        "400 Bad Request",
        QueryError::Rejected,
        InsertFailure::Rejected,
        InsertError::NotApplied(InsertFailure::Rejected),
    ),
    (
        "SELECT empty_401",
        "401 Unauthorized",
        QueryError::Rejected,
        InsertFailure::Rejected,
        InsertError::NotApplied(InsertFailure::Rejected),
    ),
    (
        "SELECT empty_403",
        "403 Forbidden",
        QueryError::Rejected,
        InsertFailure::Rejected,
        InsertError::NotApplied(InsertFailure::Rejected),
    ),
    (
        "SELECT empty_413",
        "413 Payload Too Large",
        QueryError::Unavailable,
        InsertFailure::Unavailable,
        InsertError::NotApplied(InsertFailure::TooLarge),
    ),
    (
        "SELECT prose_400",
        "400 rows written",
        QueryError::Unavailable,
        InsertFailure::Unavailable,
        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
    ),
    (
        "SELECT prose_401",
        "401 custom text",
        QueryError::Unavailable,
        InsertFailure::Unavailable,
        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
    ),
    (
        "SELECT body_401",
        "unauthorized",
        QueryError::Unavailable,
        InsertFailure::Unavailable,
        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
    ),
    (
        "SELECT body_413",
        "request entity too large",
        QueryError::Unavailable,
        InsertFailure::Unavailable,
        InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
    ),
];

#[tokio::test]
async fn a4_7_bad_response_loopback_probes() {
    let (bad_addr, bad_srv) = spawn_bad_response_server().await;
    let bad_client = clickhouse::Client::default().with_url(format!("http://{bad_addr}"));

    let err_158 = bad_client
        .query("SELECT ch_500_158")
        .fetch_one::<u64>()
        .await
        .expect_err("158");
    assert!(
        matches!(&err_158, clickhouse::error::Error::BadResponse(s) if s.starts_with("Code: 158"))
    );
    let classified_158 = error::classify_query_error(&err_158, false);
    assert_eq!(classified_158.error, QueryError::TooLarge);
    assert_eq!(
        (
            classified_158.detail.reason,
            classified_158.detail.status_code
        ),
        (
            error::FailureReason::ResultLimit,
            Some(error::ResponseStatusCode::ClickHouse(158)),
        )
    );

    for &(sql, expected_reason, expected_query, expected_metadata, expected_insert) in
        A4_7_LOOPBACK_CASES
    {
        let err = bad_client
            .query(sql)
            .fetch_one::<u64>()
            .await
            .expect_err(sql);
        assert!(
            matches!(&err, clickhouse::error::Error::BadResponse(s) if s == expected_reason),
            "sql={sql}: expected BadResponse({expected_reason:?}), got {err:?}"
        );
        assert_eq!(
            (
                error::classify_query_error(&err, false).error,
                error::classify_metadata_error(&err).error,
                error::classify_post_dispatch_insert_error(&err, true).error,
            ),
            (
                expected_query,
                InsertError::NotApplied(expected_metadata),
                expected_insert,
            ),
            "{sql}"
        );
    }

    bad_srv.abort();
}

#[test]
fn clickhouse_permanent_db_and_query_param_codes_classify_as_rejected() {
    for (code, message, expected_reason) in [
        (
            16_u32,
            "Code: 16. DB::Exception: No such column in table",
            error::FailureReason::Schema,
        ),
        (
            36_u32,
            "Code: 36. DB::Exception: Bad arguments",
            error::FailureReason::QueryInvalid,
        ),
        (
            43_u32,
            "Code: 43. DB::Exception: Illegal type of argument",
            error::FailureReason::QueryInvalid,
        ),
        (
            46_u32,
            "Code: 46. DB::Exception: Unknown function",
            error::FailureReason::QueryInvalid,
        ),
        (
            70_u32,
            "Code: 70. DB::Exception: Cannot convert type",
            error::FailureReason::QueryInvalid,
        ),
        (
            81_u32,
            "Code: 81. DB::Exception: Database missing does not exist",
            error::FailureReason::Schema,
        ),
        (
            117_u32,
            "Code: 117. DB::Exception: Incorrect data",
            error::FailureReason::QueryInvalid,
        ),
        (
            153_u32,
            "Code: 153. DB::Exception: Illegal division",
            error::FailureReason::QueryInvalid,
        ),
        (
            215_u32,
            "Code: 215. DB::Exception: Not an aggregate",
            error::FailureReason::QueryInvalid,
        ),
        (
            386_u32,
            "Code: 386. DB::Exception: No common type",
            error::FailureReason::QueryInvalid,
        ),
        (
            456_u32,
            "Code: 456. DB::Exception: Query parameter `id` was not set",
            error::FailureReason::QueryInvalid,
        ),
        (
            457_u32,
            "Code: 457. DB::Exception: Value for query parameter `id` is invalid",
            error::FailureReason::QueryInvalid,
        ),
    ] {
        let ch_err = clickhouse::error::Error::BadResponse(message.to_owned());
        let q = error::classify_query_error(&ch_err, false);
        assert_eq!(q.error, QueryError::Rejected, "{code}");
        assert!(!q.error.is_retryable(), "{code}");
        assert_eq!(q.detail.reason, expected_reason, "{code}");
        assert_eq!(
            q.detail.status_code,
            Some(error::ResponseStatusCode::ClickHouse(code)),
            "{code}"
        );
    }
    let db_err = clickhouse::error::Error::BadResponse(
        "Code: 81. DB::Exception: Database missing does not exist".to_owned(),
    );
    assert_eq!(
        error::classify_metadata_error(&db_err).error,
        InsertError::NotApplied(InsertFailure::Rejected)
    );
    assert_eq!(
        error::classify_post_dispatch_insert_error(&db_err, true).error,
        InsertError::NotApplied(InsertFailure::Rejected)
    );
}

#[test]
fn a4_7_canonical_http_status_fallback_and_prose_fail_closed() {
    let canonical_cases = [
        (
            "400 Bad Request",
            Some(400),
            QueryError::Rejected,
            InsertFailure::Rejected,
            InsertError::NotApplied(InsertFailure::Rejected),
        ),
        (
            "401 Unauthorized",
            Some(401),
            QueryError::Rejected,
            InsertFailure::Rejected,
            InsertError::NotApplied(InsertFailure::Rejected),
        ),
        (
            "403 Forbidden",
            Some(403),
            QueryError::Rejected,
            InsertFailure::Rejected,
            InsertError::NotApplied(InsertFailure::Rejected),
        ),
        (
            "413 Payload Too Large",
            Some(413),
            QueryError::Unavailable,
            InsertFailure::Unavailable,
            InsertError::NotApplied(InsertFailure::TooLarge),
        ),
    ];

    for (reason, expected_status, expected_query, expected_metadata, expected_insert) in
        canonical_cases
    {
        assert_eq!(
            error::extract_http_status_from_bad_response(reason),
            expected_status,
            "{reason}"
        );
        let ch_err = clickhouse::error::Error::BadResponse(reason.to_owned());
        let q = error::classify_query_error(&ch_err, false);
        assert_eq!(q.error, expected_query, "{reason}");
        assert_eq!(
            q.detail.status_code,
            expected_status.map(error::ResponseStatusCode::Http),
            "{reason}"
        );
        assert_eq!(
            error::classify_metadata_error(&ch_err).error,
            InsertError::NotApplied(expected_metadata),
            "{reason}"
        );
        assert_eq!(
            error::classify_post_dispatch_insert_error(&ch_err, true).error,
            expected_insert,
            "{reason}"
        );
    }

    for prose in [
        "400 rows written",
        "401 custom text",
        "403 proxy denied",
        "413 request entity too large",
        "400 Bad Request extra",
        " 400 Bad Request",
        "404 Not Found",
        "500 Internal Server Error",
        "unauthorized",
        "request entity too large",
    ] {
        assert_eq!(
            error::extract_http_status_from_bad_response(prose),
            None,
            "{prose}"
        );
        let ch_err = clickhouse::error::Error::BadResponse(prose.to_owned());
        assert_eq!(
            error::classify_query_error(&ch_err, false).error,
            QueryError::Unavailable,
            "{prose}"
        );
        assert_eq!(
            error::classify_metadata_error(&ch_err).error,
            InsertError::NotApplied(InsertFailure::Unavailable),
            "{prose}"
        );
        assert_eq!(
            error::classify_post_dispatch_insert_error(&ch_err, true).error,
            InsertError::OutcomeUnknown(UnknownOutcome::Unavailable),
            "{prose}"
        );
    }
}

struct CaptureServerState {
    describe_calls: AtomicUsize,
    open_conns: AtomicUsize,
    conns_closed: Notify,
    received_inserts: Mutex<Vec<Vec<u8>>>,
}

struct CaptureConnGuard(Arc<CaptureServerState>);

impl Drop for CaptureConnGuard {
    fn drop(&mut self) {
        if self.0.open_conns.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.conns_closed.notify_waiters();
        }
    }
}

async fn handle_raw_insert_capture_conn(mut stream: TcpStream, state: Arc<CaptureServerState>) {
    let _conn_guard = CaptureConnGuard(Arc::clone(&state));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut describe_handled = false;
    while let Ok(n) = stream.read(&mut tmp).await {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if !describe_handled
            && let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n")
            && String::from_utf8_lossy(&buf).contains("DESCRIBE TABLE")
        {
            let head_str = String::from_utf8_lossy(&buf[..pos]);
            let content_len = head_str
                .lines()
                .find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buf.len() >= pos + 4 + content_len {
                state.describe_calls.fetch_add(1, Ordering::SeqCst);
                let resp_body = rbwnat_describe_id_column();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    resp_body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&resp_body).await;
                let _ = stream.flush().await;
                buf.drain(..pos + 4 + content_len);
                describe_handled = true;
            }
        }
    }
    if !buf.is_empty() {
        state.received_inserts.lock().await.push(buf);
    }
}

#[tokio::test]
async fn a4_8_idx_zero_serialization_error_wire_capture() {
    #[derive(Row)]
    #[clickhouse(crate = "crate::__clickhouse")]
    struct FailRow {
        id: u64,
    }

    impl Serialize for FailRow {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom(format!("boom {}", self.id)))
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let state = Arc::new(CaptureServerState {
        describe_calls: AtomicUsize::new(0),
        open_conns: AtomicUsize::new(0),
        conns_closed: Notify::new(),
        received_inserts: Mutex::new(Vec::new()),
    });
    let srv_state = Arc::clone(&state);

    let srv = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            srv_state.open_conns.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(handle_raw_insert_capture_conn(
                stream,
                Arc::clone(&srv_state),
            ));
        }
    });

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::unbounded();
    let err = ch
        .insert_batch(
            &ctx,
            "fail_idx0",
            "events",
            &[FailRow { id: 1 }],
            BatchInsertOptions::new(),
        )
        .await
        .expect_err("idx 0 serialize must fail");
    assert_eq!(err, InsertError::NotApplied(InsertFailure::Encode));
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    drop(ch);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let closed = state.conns_closed.notified();
            tokio::pin!(closed);
            closed.as_mut().enable();
            if state.open_conns.load(Ordering::SeqCst) == 0 {
                break;
            }
            closed.await;
        }
    })
    .await
    .expect("capture connections closed after dropping ClickHouse");

    assert_eq!(state.describe_calls.load(Ordering::SeqCst), 1);
    let inserts = state.received_inserts.lock().await;
    assert!(
        inserts.len() <= 1,
        "expected at most 1 INSERT HTTP connection, got {}",
        inserts.len()
    );
    for raw in inserts.iter() {
        let text = String::from_utf8_lossy(raw);
        let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
            panic!("expected complete HTTP headers, got: {text}");
        };
        let head = String::from_utf8_lossy(&raw[..pos]);
        assert!(
            head.contains("INSERT"),
            "expected INSERT request headers, got: {head}"
        );
        let body = &raw[pos + 4..];
        assert!(
            body.is_empty(),
            "expected 0 HTTP body bytes on idx==0 serialize failure, got {} bytes: {:?}",
            body.len(),
            body
        );
    }
    srv.abort();
}
