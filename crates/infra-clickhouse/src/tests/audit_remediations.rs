use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Body, Bytes, Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use operation_context::OperationContext;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify};

use super::{
    SampleRow, base_options, encode_ch_str, encode_varuint, rbwnat_describe_id_column,
    rbwnat_u64_rows, spawn_http_server,
};
use crate::{
    BatchInsertOptions, ClickHouse, CloseOutcome, Decimal32, Decimal64, Decimal128, InsertError,
    InsertFailure, Param, QueryError, QuerySettings, Row, UnknownOutcome,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
struct WideStringRow {
    payload: String,
}

fn rbwnat_describe_columns(cols: &[(&str, &str, &str)]) -> Bytes {
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
    for &(name, ty, default_type) in cols {
        encode_ch_str(name, &mut out);
        encode_ch_str(ty, &mut out);
        encode_ch_str(default_type, &mut out);
        for _ in 0..4 {
            encode_ch_str("", &mut out);
        }
    }
    Bytes::from(out)
}

#[tokio::test]
async fn multi_chunk_insert_surfaces_early_server_errors_and_invalidates_schema_cache() {
    let describe_calls = Arc::new(AtomicUsize::new(0));
    let insert_calls = Arc::new(AtomicUsize::new(0));
    let describe_srv = Arc::clone(&describe_calls);
    let insert_srv = Arc::clone(&insert_calls);

    let (addr, srv) = spawn_http_server(move |req: Request<Incoming>| {
        let describes = Arc::clone(&describe_srv);
        let inserts = Arc::clone(&insert_srv);
        async move {
            let uri = req.uri().to_string();
            let is_describe = uri.contains("-describe");
            if is_describe {
                let _ = req.into_body().collect().await;
                describes.fetch_add(1, Ordering::SeqCst);
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_columns(&[(
                        "payload", "String", "",
                    )])))
                    .expect("describe resp");
            }
            drop(req.into_body());
            let call = inserts.fetch_add(1, Ordering::SeqCst);
            let (status, body) = match call {
                0 => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Bytes::from_static(b"Code: 252. DB::Exception: Too many parts"),
                ),
                1 => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Bytes::from_static(b"Code: 53. DB::Exception: Type mismatch"),
                ),
                _ => (StatusCode::OK, Bytes::new()),
            };
            Response::builder()
                .status(status)
                .body(Full::new(body))
                .expect("insert resp")
        }
    })
    .await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let single_big = [WideStringRow {
        payload: "x".repeat(320 * 1024),
    }];
    let err_252 = ch
        .insert_batch(
            &ctx,
            "big_252",
            "events",
            &single_big,
            BatchInsertOptions::new(),
        )
        .await
        .expect_err("early 252 on >254 KiB single row");
    assert_eq!(
        err_252,
        InsertError::NotApplied(InsertFailure::TooManyParts)
    );

    let multi_big: Vec<WideStringRow> = (0..10)
        .map(|_| WideStringRow {
            payload: "y".repeat(40 * 1024),
        })
        .collect();
    let err_53 = ch
        .insert_batch(
            &ctx,
            "big_53",
            "events",
            &multi_big,
            BatchInsertOptions::new(),
        )
        .await
        .expect_err("early 53 on >254 KiB batch");
    assert_eq!(err_53, InsertError::NotApplied(InsertFailure::Rejected));
    assert_eq!(describe_calls.load(Ordering::SeqCst), 1);

    let ok_after_invalidation = ch
        .insert_batch(
            &ctx,
            "after_53",
            "events",
            &[WideStringRow {
                payload: "z".to_owned(),
            }],
            BatchInsertOptions::new(),
        )
        .await
        .expect("slot re-primed after Code: 53");
    assert_eq!(ok_after_invalidation.rows, 1);
    assert_eq!(describe_calls.load(Ordering::SeqCst), 2);

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

async fn handle_close_conn_describe(
    req: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let body = req.into_body().collect().await.expect("body").to_bytes();
    let resp_body = if String::from_utf8_lossy(&body).contains("DESCRIBE") {
        rbwnat_describe_id_column()
    } else {
        Bytes::new()
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("connection", "close")
        .body(Full::new(resp_body))
        .expect("resp"))
}

async fn spawn_close_conn_describe_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let _ = http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(stream), service_fn(handle_close_conn_describe))
                    .await;
            });
        }
    });
    (addr, srv)
}

#[tokio::test]
async fn warm_slot_connect_refused_returns_not_applied_instead_of_channel_closed() {
    let (addr, srv) = spawn_close_conn_describe_server().await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    ch.insert_batch(
        &ctx,
        "prime",
        "events",
        &[SampleRow { id: 1 }],
        BatchInsertOptions::new(),
    )
    .await
    .expect("prime warm slot");

    srv.abort();
    let _ = srv.await;

    let err = ch
        .insert_batch(
            &ctx,
            "after_down",
            "events",
            &[SampleRow { id: 2 }],
            BatchInsertOptions::new(),
        )
        .await
        .expect_err("ECONNREFUSED on warm slot must fail");
    assert_eq!(err, InsertError::NotApplied(InsertFailure::Unavailable));
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
}

#[tokio::test]
async fn multi_row_code_252_is_outcome_unknown_while_single_row_is_not_applied() {
    let (addr, srv) = spawn_http_server(|req: Request<Incoming>| async move {
        let body = req.into_body().collect().await.expect("body").to_bytes();
        if String::from_utf8_lossy(&body).contains("DESCRIBE") {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(rbwnat_describe_id_column()))
                .expect("describe");
        }
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from_static(
                b"Code: 252. DB::Exception: Too many parts",
            )))
            .expect("252")
    })
    .await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    assert_eq!(
        ch.insert_batch(
            &ctx,
            "single_252",
            "events",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new()
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::TooManyParts))
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "multi_252",
            "events",
            &[SampleRow { id: 1 }, SampleRow { id: 2 }],
            BatchInsertOptions::new()
        )
        .await,
        Err(InsertError::OutcomeUnknown(UnknownOutcome::Unavailable))
    );
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

#[tokio::test]
async fn nullable_and_low_cardinality_nullable_without_default_are_omittable() {
    let (addr, srv) = spawn_http_server(|req: Request<Incoming>| async move {
        let body = req.into_body().collect().await.expect("body").to_bytes();
        let resp_body = if String::from_utf8_lossy(&body).contains("DESCRIBE") {
            rbwnat_describe_columns(&[
                ("id", "UInt64", ""),
                ("opt", "Nullable(String)", ""),
                ("lc_opt", "LowCardinality(Nullable(String))", ""),
            ])
        } else {
            Bytes::new()
        };
        Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(resp_body))
            .expect("resp")
    })
    .await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let report = ch
        .insert_batch(
            &ctx,
            "cold_nullable",
            "events",
            &[SampleRow { id: 42 }],
            BatchInsertOptions::new(),
        )
        .await
        .expect("omitting Nullable without explicit DEFAULT must succeed");
    assert_eq!(report.rows, 1);
    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

struct ChunkedBytesBody {
    chunks: Vec<Bytes>,
    index: usize,
    sent_chunks: Arc<AtomicUsize>,
}

impl Body for ChunkedBytesBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.index >= self.chunks.len() {
            return Poll::Ready(None);
        }
        let chunk = self.chunks[self.index].clone();
        self.index += 1;
        self.sent_chunks.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Some(Ok(Frame::data(chunk))))
    }
}

fn build_oversized_rbwnat_payload(body: &str) -> Vec<u8> {
    let mut payload = Vec::new();
    encode_varuint(1, &mut payload);
    if body.contains("big_header") {
        encode_varuint(200 * 1024, &mut payload);
        payload.extend(std::iter::repeat_n(b'a', 200 * 1024));
        encode_ch_str("String", &mut payload);
    } else {
        encode_ch_str("payload", &mut payload);
        encode_ch_str("String", &mut payload);
        encode_varuint(200 * 1024, &mut payload);
        payload.extend(std::iter::repeat_n(b'b', 200 * 1024));
    }
    payload
}

async fn handle_chunked_rbwnat(
    req: Request<Incoming>,
    sent: Arc<AtomicUsize>,
) -> Result<Response<ChunkedBytesBody>, hyper::Error> {
    let body = req.into_body().collect().await.expect("body").to_bytes();
    let payload = build_oversized_rbwnat_payload(&String::from_utf8_lossy(&body));
    let chunks = payload
        .chunks(16 * 1024)
        .map(Bytes::copy_from_slice)
        .collect();
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(ChunkedBytesBody {
            chunks,
            index: 0,
            sent_chunks: sent,
        })
        .expect("stream resp"))
}

async fn spawn_chunked_rbwnat_server(
    sent_srv: Arc<AtomicUsize>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let sent = Arc::clone(&sent_srv);
            tokio::spawn(async move {
                let svc = service_fn(move |req| handle_chunked_rbwnat(req, Arc::clone(&sent)));
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, srv)
}

#[tokio::test]
async fn streaming_max_result_bytes_aborts_oversized_single_row_and_header_early() {
    let sent_chunks = Arc::new(AtomicUsize::new(0));
    let (addr, srv) = spawn_chunked_rbwnat_server(Arc::clone(&sent_chunks)).await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.query_settings = QuerySettings::Server;
    opts.max_result_bytes = 64 * 1024;
    opts.http_response_buffer_size = 64 * 1024;
    let ch = ClickHouse::start(opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    for sql in ["SELECT big_row", "SELECT big_header"] {
        sent_chunks.store(0, Ordering::SeqCst);
        let err = ch
            .query("oversized", sql)
            .max_result_bytes(NonZeroUsize::new(64 * 1024).expect("nz"))
            .fetch_one::<WideStringRow>(&ctx)
            .await
            .expect_err(sql);
        assert_eq!(err, QueryError::TooLarge, "{sql}");
    }

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

fn handle_settings_probe(uri: &str, body_str: &str) -> (StatusCode, Bytes) {
    if body_str.contains("DESCRIBE") {
        (StatusCode::OK, rbwnat_describe_id_column())
    } else if body_str.contains("code_396_rows") {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(
                b"Code: 396. DB::Exception: Limit for result exceeded, max rows: 2.00, current rows: 10.00. (TOO_MANY_ROWS_OR_BYTES)",
            ),
        )
    } else if body_str.contains("code_396_bytes") {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(
                b"Code: 396. DB::Exception: Limit for result exceeded, max bytes: 1.00 KiB, current bytes: 4.90 KiB. (TOO_MANY_ROWS_OR_BYTES)",
            ),
        )
    } else if uri.contains("INSERT") {
        (StatusCode::OK, Bytes::new())
    } else {
        (StatusCode::OK, rbwnat_u64_rows("id", &[1]))
    }
}

#[tokio::test]
async fn describe_settings_fetch_one_row_cap_and_fractional_timeouts_are_enforced() {
    let captured_uris = Arc::new(Mutex::new(Vec::new()));
    let uris_srv = Arc::clone(&captured_uris);

    let (addr, srv) = spawn_http_server(move |req: Request<Incoming>| {
        let uris = Arc::clone(&uris_srv);
        async move {
            let uri = req.uri().to_string();
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            uris.lock().await.push(uri.clone());

            let (status, resp_body) = handle_settings_probe(&uri, &body_str);
            Response::builder()
                .status(status)
                .body(Full::new(resp_body))
                .expect("resp")
        }
    })
    .await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let _ = ch
        .query("one_caps", "SELECT 1 AS id")
        .timeout(Duration::from_millis(250))
        .fetch_one::<SampleRow>(&ctx)
        .await
        .expect("fetch_one ok");

    assert_eq!(
        ch.query("one_rows_396", "SELECT code_396_rows")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::TooManyRows)
    );
    assert_eq!(
        ch.query("one_bytes_396", "SELECT code_396_bytes")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::TooLarge)
    );
    assert_eq!(
        ch.query("all_rows_396", "SELECT code_396_rows")
            .fetch_all::<SampleRow>(&ctx)
            .await,
        Err(QueryError::TooLarge)
    );

    ch.insert_batch(
        &ctx,
        "ins",
        "events",
        &[SampleRow { id: 1 }],
        BatchInsertOptions::new(),
    )
    .await
    .expect("insert ok");

    let uris = captured_uris.lock().await;
    assert!(
        uris[0].contains("max_result_rows=2")
            && uris[0].contains("readonly=2")
            && uris[0].contains("cancel_http_readonly_queries_on_client_close=1")
            && uris[0].contains("session_timezone=UTC")
            && uris[0].contains("max_execution_time=0.2"),
        "fetch_one uri missing expected settings: {}",
        uris[0]
    );
    let describe_uri = uris
        .iter()
        .find(|u| u.contains("-describe"))
        .expect("DESCRIBE must carry linked {query_id}-describe");
    assert!(
        describe_uri.contains("wait_end_of_query=1")
            && describe_uri.contains("readonly=2")
            && describe_uri.contains("cancel_http_readonly_queries_on_client_close=1")
            && describe_uri.contains("max_execution_time=")
            && describe_uri.contains("max_memory_usage=536870912"),
        "DESCRIBE uri missing expected limits: {describe_uri}"
    );
    let insert_uri = uris.last().expect("insert uri");
    assert!(
        !insert_uri.contains("readonly=2") && insert_uri.contains("max_execution_time="),
        "INSERT uri should carry timeout without readonly=2: {insert_uri}"
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

struct RwGates {
    read_entered: Arc<Notify>,
    read_release: Arc<Notify>,
    write_entered: Arc<Notify>,
    write_release: Arc<Notify>,
}

async fn spawn_gated_rw_server(
    gates: &RwGates,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let r_ent = Arc::clone(&gates.read_entered);
    let r_rel = Arc::clone(&gates.read_release);
    let w_ent = Arc::clone(&gates.write_entered);
    let w_rel = Arc::clone(&gates.write_release);

    spawn_http_server(move |req: Request<Incoming>| {
        let r_ent = Arc::clone(&r_ent);
        let r_rel = Arc::clone(&r_rel);
        let w_ent = Arc::clone(&w_ent);
        let w_rel = Arc::clone(&w_rel);
        async move {
            let uri = req.uri().to_string();
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let s = String::from_utf8_lossy(&body);
            let resp_body = if s.contains("DESCRIBE") {
                rbwnat_describe_id_column()
            } else if s.contains("hold_read") {
                r_ent.notify_waiters();
                r_rel.notified().await;
                rbwnat_u64_rows("id", &[1])
            } else if uri.contains("hold_write") {
                w_ent.notify_waiters();
                w_rel.notified().await;
                Bytes::new()
            } else if uri.contains("INSERT") {
                Bytes::new()
            } else {
                rbwnat_u64_rows("id", &[2])
            };
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(resp_body))
                .expect("resp")
        }
    })
    .await
}

#[tokio::test]
async fn separate_read_and_write_concurrency_permits_do_not_starve_each_other() {
    let gates = RwGates {
        read_entered: Arc::new(Notify::new()),
        read_release: Arc::new(Notify::new()),
        write_entered: Arc::new(Notify::new()),
        write_release: Arc::new(Notify::new()),
    };
    let (addr, srv) = spawn_gated_rw_server(&gates).await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.max_read_concurrency = 1;
    opts.max_write_concurrency = 1;
    let ch = ClickHouse::start(opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let r_wait = gates.read_entered.notified();
    tokio::pin!(r_wait);
    r_wait.as_mut().enable();
    let (ch_r, ctx_r) = (ch.clone(), ctx.clone());
    let r_holder = tokio::spawn(async move {
        ch_r.query("hold_read", "SELECT hold_read")
            .fetch_one::<SampleRow>(&ctx_r)
            .await
    });
    r_wait.await;
    assert_eq!(
        ch.query("shed_read", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Busy)
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "write_ok",
            "events",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new()
        )
        .await
        .expect("write succeeds while read permit is full")
        .rows,
        1
    );
    gates.read_release.notify_waiters();
    assert_eq!(r_holder.await.expect("join"), Ok(SampleRow { id: 1 }));

    let w_wait = gates.write_entered.notified();
    tokio::pin!(w_wait);
    w_wait.as_mut().enable();
    let (ch_w, ctx_w) = (ch.clone(), ctx.clone());
    let w_holder = tokio::spawn(async move {
        ch_w.insert_batch(
            &ctx_w,
            "hold_write",
            "hold_write",
            &[SampleRow { id: 2 }],
            BatchInsertOptions::new(),
        )
        .await
    });
    w_wait.await;
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "shed_write",
            "events",
            &[SampleRow { id: 3 }],
            BatchInsertOptions::new()
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Busy))
    );
    assert_eq!(
        ch.query("read_ok", "SELECT 2")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Ok(SampleRow { id: 2 })
    );
    gates.write_release.notify_waiters();
    assert_eq!(w_holder.await.expect("join").expect("write ok").rows, 1);

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
struct ValidTypedRow {
    d32: Decimal32<2>,
    d64: Decimal64<4>,
    d128: Decimal128<6>,
    opt_d64: Option<Decimal64<2>>,
    #[serde(with = "crate::serde::time::datetime64::millis")]
    ts: time::OffsetDateTime,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
struct WrongScaleRow {
    d64: Decimal64<2>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
struct RawIntDecimalRow {
    d64: i64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
struct WrongPrecisionTsRow {
    #[serde(with = "crate::serde::time::datetime64::micros")]
    ts: time::OffsetDateTime,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "crate::__clickhouse")]
struct RawIntTsRow {
    ts: i64,
}

fn build_typed_mock_response(body: &str) -> Bytes {
    if body.contains("DESCRIBE") {
        rbwnat_describe_columns(&[
            ("d32", "Decimal(7, 2)", ""),
            ("d64", "Decimal(12, 4)", ""),
            ("d128", "Decimal(28, 6)", ""),
            ("opt_d64", "Nullable(Decimal(12, 2))", ""),
            ("ts", "DateTime64(3, 'UTC')", ""),
        ])
    } else if body.contains("select_valid_typed") {
        let mut out = Vec::new();
        encode_varuint(5, &mut out);
        for name in ["d32", "d64", "d128", "opt_d64", "ts"] {
            encode_ch_str(name, &mut out);
        }
        for ty in [
            "Decimal(7, 2)",
            "Decimal(12, 4)",
            "Decimal(28, 6)",
            "Nullable(Decimal(12, 2))",
            "DateTime64(3, 'UTC')",
        ] {
            encode_ch_str(ty, &mut out);
        }
        out.extend_from_slice(&(-1234_i32).to_le_bytes());
        out.extend_from_slice(&567_890_i64.to_le_bytes());
        out.extend_from_slice(&(-9_999_999_i128).to_le_bytes());
        out.push(0);
        out.extend_from_slice(&(-5_i64).to_le_bytes());
        out.extend_from_slice(&1_763_820_000_123_i64.to_le_bytes());
        Bytes::from(out)
    } else if body.contains("select_decimal64_scale4") {
        let mut out = Vec::new();
        encode_varuint(1, &mut out);
        encode_ch_str("d64", &mut out);
        encode_ch_str("Decimal(12, 4)", &mut out);
        out.extend_from_slice(&1234_i64.to_le_bytes());
        Bytes::from(out)
    } else if body.contains("select_dt64_3") {
        let mut out = Vec::new();
        encode_varuint(1, &mut out);
        encode_ch_str("ts", &mut out);
        encode_ch_str("DateTime64(3, 'UTC')", &mut out);
        out.extend_from_slice(&1_763_820_000_123_i64.to_le_bytes());
        Bytes::from(out)
    } else if body.contains("param_check") {
        rbwnat_u64_rows("id", &[1])
    } else {
        Bytes::new()
    }
}

async fn spawn_typed_mock_server() -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<Mutex<Vec<String>>>,
) {
    let captured_bodies = Arc::new(Mutex::new(Vec::new()));
    let bodies_srv = Arc::clone(&captured_bodies);
    let (addr, srv) = spawn_http_server(move |req: Request<Incoming>| {
        let bodies = Arc::clone(&bodies_srv);
        async move {
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let s = String::from_utf8_lossy(&body).into_owned();
            bodies.lock().await.push(s.clone());
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(build_typed_mock_response(&s)))
                .expect("resp")
        }
    })
    .await;
    (addr, srv, captured_bodies)
}

#[tokio::test]
async fn decimal_scale_and_datetime64_precision_are_strictly_validated() {
    let (addr, srv, _) = spawn_typed_mock_server().await;
    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000)
        .expect("valid ts");
    let expected = ValidTypedRow {
        d32: Decimal32::new(-1234),
        d64: Decimal64::new(567_890),
        d128: Decimal128::new(-9_999_999),
        opt_d64: Some(Decimal64::new(-5)),
        ts,
    };

    assert_eq!(
        ch.query("valid_read", "SELECT select_valid_typed")
            .fetch_one::<ValidTypedRow>(&ctx)
            .await,
        Ok(expected)
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "valid_write",
            "typed_events",
            &[ValidTypedRow {
                d32: Decimal32::new(-1234),
                d64: Decimal64::new(567_890),
                d128: Decimal128::new(-9_999_999),
                opt_d64: None,
                ts,
            }],
            BatchInsertOptions::new(),
        )
        .await
        .expect("valid typed write")
        .rows,
        1
    );

    assert_eq!(
        ch.query("bad_scale_read", "SELECT select_decimal64_scale4")
            .fetch_one::<WrongScaleRow>(&ctx)
            .await,
        Err(QueryError::Decode)
    );
    assert_eq!(
        ch.query("raw_dec_read", "SELECT select_decimal64_scale4")
            .fetch_one::<RawIntDecimalRow>(&ctx)
            .await,
        Err(QueryError::Decode)
    );
    assert_eq!(
        ch.query("bad_prec_read", "SELECT select_dt64_3")
            .fetch_one::<WrongPrecisionTsRow>(&ctx)
            .await,
        Err(QueryError::Decode)
    );
    assert_eq!(
        ch.query("raw_ts_read", "SELECT select_dt64_3")
            .fetch_one::<RawIntTsRow>(&ctx)
            .await,
        Err(QueryError::Decode)
    );

    assert_eq!(
        ch.insert_batch(
            &ctx,
            "bad_scale_write",
            "typed_events_bad1",
            &[WrongScaleRow {
                d64: Decimal64::new(100),
            }],
            BatchInsertOptions::new(),
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Encode))
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "bad_prec_write",
            "typed_events_bad2",
            &[WrongPrecisionTsRow { ts }],
            BatchInsertOptions::new(),
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Encode))
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

#[tokio::test]
async fn datetime_and_decimal_query_param_formatting_and_validation() {
    let (addr, srv, captured_bodies) = spawn_typed_mock_server().await;
    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));

    let ts = time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000)
        .expect("valid ts");
    let pre_2001 = time::OffsetDateTime::from_unix_timestamp_nanos(-500_000_000).expect("1969");
    let _ = ch
        .query("param_fmt", "SELECT param_check")
        .param("dt_modern", ts)
        .param("dt_secs", Param::datetime_secs(ts))
        .param("dt_old", pre_2001)
        .param("dec_neg", Decimal64::<2>::new(-5))
        .param("dec_int", Param::decimal(42, 0))
        .fetch_one::<SampleRow>(&ctx)
        .await
        .expect("params ok");
    let body = captured_bodies.lock().await.last().cloned().expect("body");
    assert!(body.contains("1763820000.123000000"), "{body}");
    assert!(
        body.contains("name=\"param_dt_secs\"\r\n\r\n1763820000\r\n"),
        "{body}"
    );
    assert!(body.contains("1969-12-31 23:59:59.500000000"), "{body}");
    assert!(
        body.contains("name=\"param_dec_neg\"\r\n\r\n-0.05\r\n"),
        "{body}"
    );
    assert!(
        body.contains("name=\"param_dec_int\"\r\n\r\n42\r\n"),
        "{body}"
    );

    assert_eq!(
        ch.query("bad_dec_scale", "SELECT 1")
            .param("d", Param::decimal(1, 39))
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Rejected)
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}
