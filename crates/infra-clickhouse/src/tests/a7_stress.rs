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
use operation_context::{OperationContext, Stopped};
use tokio::net::TcpListener;
use tokio::sync::Notify;

use super::{SampleRow, base_options, rbwnat_describe_id_column, spawn_http_server};
use crate::{ClickHouse, CloseOutcome, InsertError, InsertOptions, QueryError, UnknownOutcome};

struct OpenGuard(Arc<AtomicUsize>);

impl OpenGuard {
    fn enter(open: Arc<AtomicUsize>) -> Self {
        open.fetch_add(1, Ordering::SeqCst);
        Self(open)
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn spawn_stalling_insert_server() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    Arc<Notify>,
    Arc<Notify>,
    tokio::task::JoinHandle<()>,
) {
    let release_handlers = Arc::new(Notify::new());
    let open_inserts = Arc::new(AtomicUsize::new(0));
    let insert_received = Arc::new(Notify::new());

    let release_srv = Arc::clone(&release_handlers);
    let open_srv = Arc::clone(&open_inserts);
    let recv_srv = Arc::clone(&insert_received);
    let (addr, srv) = spawn_http_server(move |req: Request<Incoming>| {
        let release = Arc::clone(&release_srv);
        let open = Arc::clone(&open_srv);
        let recv = Arc::clone(&recv_srv);
        async move {
            let uri = req.uri().to_string();
            let Ok(collected) = req.into_body().collect().await else {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Full::new(Bytes::new()))
                    .expect("resp");
            };
            let body = collected.to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if uri.contains("DESCRIBE") || body_str.contains("DESCRIBE") {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            let _open_guard = OpenGuard::enter(open);
            recv.notify_one();
            release.notified().await;
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::new()))
                .expect("resp")
        }
    })
    .await;

    (addr, open_inserts, insert_received, release_handlers, srv)
}

#[tokio::test]
async fn a7_cancelled_insert_does_not_leak_detached_tasks_or_connections() {
    let (addr, open_inserts, insert_received, release_handlers, srv) =
        spawn_stalling_insert_server().await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.insert_timeout = Duration::from_millis(50);
    opts.max_read_concurrency = NonZeroUsize::new(4).expect("nz");
    opts.max_write_concurrency = NonZeroUsize::new(4).expect("nz");
    let ch = ClickHouse::start(opts).await.expect("start");

    let baseline_tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    for i in 0..30_u64 {
        let ctx = OperationContext::with_timeout(Duration::from_secs(2));
        let notified = insert_received.notified();
        tokio::pin!(notified);
        let row = [SampleRow { id: i }];
        let opts = InsertOptions::batch();
        let fut = ch.insert(&ctx, "a7_cancel", "events", &row, opts);
        tokio::pin!(fut);
        tokio::select! {
            res = &mut fut => panic!("unexpected completion: {res:?}"),
            () = &mut notified => {
                ctx.cancel();
                let res = fut.await;
                assert_eq!(
                    res,
                    Err(InsertError::OutcomeUnknown(UnknownOutcome::Stopped(
                        Stopped::Cancelled
                    )))
                );
            }
        }
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        ch.close(Duration::from_millis(200)).await,
        CloseOutcome::Complete
    );
    let open_after = open_inserts.load(Ordering::SeqCst);
    drop(ch);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let tasks_after = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();

    release_handlers.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;
    srv.abort();

    assert_eq!(
        open_after, 0,
        "A7 falsified: {open_after} open insert connections remained before handler release"
    );
    assert!(
        tasks_after <= baseline_tasks,
        "A7 falsified: {} detached tasks leaked (baseline={baseline_tasks}, after={tasks_after})",
        tasks_after.saturating_sub(baseline_tasks)
    );
}

#[tokio::test]
async fn a7_local_timeout_insert_does_not_leak_detached_tasks_or_connections() {
    let (addr, open_inserts, _insert_received, release_handlers, srv) =
        spawn_stalling_insert_server().await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.insert_timeout = Duration::from_millis(40);
    opts.max_read_concurrency = NonZeroUsize::new(4).expect("nz");
    opts.max_write_concurrency = NonZeroUsize::new(4).expect("nz");
    let ch = ClickHouse::start(opts).await.expect("start");

    let baseline_tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let ctx = OperationContext::unbounded();
    for i in 0..10_u64 {
        let res = ch
            .insert(
                &ctx,
                "a7_timeout",
                "events",
                &[SampleRow { id: i }],
                InsertOptions::batch(),
            )
            .await;
        assert_eq!(
            res,
            Err(InsertError::OutcomeUnknown(UnknownOutcome::Unavailable))
        );
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        ch.close(Duration::from_millis(200)).await,
        CloseOutcome::Complete
    );
    let open_after = open_inserts.load(Ordering::SeqCst);
    drop(ch);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let tasks_after = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();

    release_handlers.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;
    srv.abort();

    assert_eq!(
        open_after, 0,
        "A7 falsified on local insert_timeout: {open_after} open insert connections remained before handler release"
    );
    assert!(
        tasks_after <= baseline_tasks,
        "A7 falsified on local insert_timeout: {} detached tasks leaked (baseline={baseline_tasks}, after={tasks_after})",
        tasks_after.saturating_sub(baseline_tasks)
    );
}

struct RepeatingErrorBody {
    prefix: Bytes,
    chunk_size: usize,
    total_bytes: usize,
    offset: usize,
    pause: Option<Pin<Box<tokio::time::Sleep>>>,
    sent_counter: Arc<AtomicUsize>,
}

impl Body for RepeatingErrorBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.offset >= self.total_bytes {
            return Poll::Ready(None);
        }
        if self.offset >= 96 * 1024 {
            let sleep = self
                .pause
                .get_or_insert_with(|| Box::pin(tokio::time::sleep(Duration::from_millis(20))));
            if sleep.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.pause = None;
        }
        let data = if self.offset == 0 && !self.prefix.is_empty() {
            let prefix = self.prefix.clone();
            self.offset += prefix.len();
            self.sent_counter.fetch_add(prefix.len(), Ordering::SeqCst);
            prefix
        } else {
            let remaining = self.total_bytes - self.offset;
            let len = remaining.min(self.chunk_size);
            self.offset += len;
            self.sent_counter.fetch_add(len, Ordering::SeqCst);
            Bytes::from(vec![b'x'; len])
        };
        Poll::Ready(Some(Ok(Frame::data(data))))
    }
}

const LZ4_INDEPENDENT_64K_HEADER: [u8; 7] = [0x04, 0x22, 0x4d, 0x18, 0x60, 0x40, 0x82];
const LZ4_UNCOMPRESSED_BLOCK_FLAG: u32 = 0x8000_0000;
const LZ4_END_MARK: [u8; 4] = [0; 4];

fn lz4_compressed_oversized_158_body() -> Bytes {
    let mut frame = LZ4_INDEPENDENT_64K_HEADER.to_vec();
    let prefix = b"Code: 158. DB::Exception: limit exceeded. x";
    let mut block = Vec::new();
    block.push(0xff);
    block.push(u8::try_from(prefix.len() - 15).expect("prefix fits u8"));
    block.extend_from_slice(prefix);
    block.extend_from_slice(&1_u16.to_le_bytes());
    block.resize(block.len() + 128, 255);
    block.push(109);
    block.push(0x50);
    block.extend_from_slice(b"xxxxx");

    let block_len = u32::try_from(block.len()).expect("block len fits u32");
    for _ in 0..4 {
        frame.extend_from_slice(&block_len.to_le_bytes());
        frame.extend_from_slice(&block);
    }
    frame.extend_from_slice(&LZ4_END_MARK);
    Bytes::from(frame)
}

fn lz4_wire_oversized_158_body() -> Bytes {
    let mut frame = LZ4_INDEPENDENT_64K_HEADER.to_vec();
    let prefix = b"Code: 158. DB::Exception: wire oversized. ";
    let mut block1 = Vec::with_capacity(60 * 1024);
    block1.extend_from_slice(prefix);
    block1.resize(60 * 1024, b'x');
    let len1 = LZ4_UNCOMPRESSED_BLOCK_FLAG | u32::try_from(block1.len()).expect("block1 fits u32");
    frame.extend_from_slice(&len1.to_le_bytes());
    frame.extend_from_slice(&block1);

    let block2 = vec![b'y'; 60 * 1024];
    let len2 = LZ4_UNCOMPRESSED_BLOCK_FLAG | u32::try_from(block2.len()).expect("block2 fits u32");
    frame.extend_from_slice(&len2.to_le_bytes());
    frame.extend_from_slice(&block2);
    frame.extend_from_slice(&LZ4_END_MARK);
    Bytes::from(frame)
}

fn utf8_boundary_oversized_158_body() -> Bytes {
    let prefix = b"Code: 158. DB::Exception: utf8 ";
    let mut body = Vec::with_capacity(65_540);
    body.extend_from_slice(prefix);
    body.resize(64 * 1024 - 1, b'x');
    body.extend_from_slice("é".as_bytes());
    body.extend_from_slice(&[b'z'; 1024]);
    Bytes::from(body)
}

type BoxRespBody = http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>;

fn streaming_500_response(
    prefix: &'static [u8],
    total_bytes: usize,
    sent_counter: Arc<AtomicUsize>,
) -> Response<BoxRespBody> {
    let body = RepeatingErrorBody {
        prefix: Bytes::from_static(prefix),
        chunk_size: 32 * 1024,
        total_bytes,
        offset: 0,
        pause: None,
        sent_counter,
    };
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(body.boxed())
        .expect("resp")
}

fn full_response(
    status: StatusCode,
    content_encoding: Option<&str>,
    body: Bytes,
) -> Response<BoxRespBody> {
    let mut builder = Response::builder().status(status);
    if let Some(enc) = content_encoding {
        builder = builder.header("Content-Encoding", enc);
    }
    builder
        .body(Full::new(body).map_err(|n| match n {}).boxed())
        .expect("resp")
}

fn oversized_error_response(
    uri: &str,
    body_text: &str,
    sent_prose: Arc<AtomicUsize>,
    sent_158: Arc<AtomicUsize>,
) -> Response<BoxRespBody> {
    if uri.contains("DESCRIBE") || body_text.contains("DESCRIBE") {
        return full_response(StatusCode::OK, None, rbwnat_describe_id_column());
    }
    if body_text.contains("lz4_wire_158") {
        return full_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("lz4"),
            lz4_wire_oversized_158_body(),
        );
    }
    if body_text.contains("utf8_boundary_158") {
        return full_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            None,
            utf8_boundary_oversized_158_body(),
        );
    }
    if body_text.contains("lz4_158") {
        return full_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            Some("lz4"),
            lz4_compressed_oversized_158_body(),
        );
    }
    if body_text.contains("code_158") {
        return streaming_500_response(
            b"Code: 158. DB::Exception: limit exceeded. ",
            4 * 1024 * 1024,
            sent_158,
        );
    }
    if uri.contains("events_252") {
        return streaming_500_response(
            b"Code: 252. DB::Exception: Too many parts. ",
            2 * 1024 * 1024,
            Arc::new(AtomicUsize::new(0)),
        );
    }
    streaming_500_response(b"upstream gateway failure: ", 8 * 1024 * 1024, sent_prose)
}

async fn spawn_oversized_error_server() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let sent_prose = Arc::new(AtomicUsize::new(0));
    let sent_158 = Arc::new(AtomicUsize::new(0));
    let sent_prose_srv = Arc::clone(&sent_prose);
    let sent_158_srv = Arc::clone(&sent_158);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let sent_prose_conn = Arc::clone(&sent_prose_srv);
            let sent_158_conn = Arc::clone(&sent_158_srv);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let sent_prose_req = Arc::clone(&sent_prose_conn);
                    let sent_158_req = Arc::clone(&sent_158_conn);
                    async move {
                        let uri = req.uri().to_string();
                        let body = req.into_body().collect().await.expect("body").to_bytes();
                        let body_text = String::from_utf8_lossy(&body);
                        Ok::<_, std::convert::Infallible>(oversized_error_response(
                            &uri,
                            &body_text,
                            sent_prose_req,
                            sent_158_req,
                        ))
                    }
                });
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (addr, sent_prose, sent_158, srv)
}

#[tokio::test]
async fn a7_oversized_http_500_error_bodies_are_capped_and_classified() {
    let (addr, sent_prose, sent_158, srv) = spawn_oversized_error_server().await;

    let ch = ClickHouse::start(base_options(format!("http://{addr}")))
        .await
        .expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(2));

    for (summary, sql, expected) in [
        ("oversized_prose", "SELECT prose", QueryError::Unavailable),
        ("oversized_158", "SELECT code_158", QueryError::TooLarge),
        ("oversized_lz4_158", "SELECT lz4_158", QueryError::TooLarge),
        (
            "oversized_lz4_wire_158",
            "SELECT lz4_wire_158",
            QueryError::TooLarge,
        ),
        (
            "oversized_utf8_158",
            "SELECT utf8_boundary_158",
            QueryError::TooLarge,
        ),
    ] {
        assert_eq!(
            ch.query(summary, sql).fetch_all::<SampleRow>(&ctx).await,
            Err(expected),
            "{sql}"
        );
    }
    assert_eq!(
        ch.insert(
            &ctx,
            "oversized_insert_252",
            "events_252",
            &[SampleRow { id: 1 }],
            InsertOptions::batch()
        )
        .await,
        Err(InsertError::NotApplied(crate::InsertFailure::TooManyParts))
    );
    assert_eq!(
        ch.insert(
            &ctx,
            "oversized_insert_prose",
            "events_prose",
            &[SampleRow { id: 1 }],
            InsertOptions::batch()
        )
        .await,
        Err(InsertError::OutcomeUnknown(UnknownOutcome::Unavailable))
    );

    let raw = clickhouse::Client::default().with_url(format!("http://{addr}"));
    for (sql, expected_prefix) in [
        ("SELECT prose", "upstream gateway failure:"),
        ("SELECT code_158", "Code: 158. DB::Exception:"),
        ("SELECT lz4_158", "Code: 158. DB::Exception:"),
        ("SELECT lz4_wire_158", "Code: 158. DB::Exception:"),
        ("SELECT utf8_boundary_158", "Code: 158. DB::Exception:"),
    ] {
        let err = raw
            .query(sql)
            .fetch_one::<u64>()
            .await
            .expect_err("must fail with bounded BadResponse");
        let clickhouse::error::Error::BadResponse(msg) = err else {
            panic!("expected BadResponse for {sql}, got {err:?}");
        };
        assert!(
            msg.len() <= 64 * 1024,
            "{sql}: BadResponse exceeded 64 KiB ({} bytes)",
            msg.len()
        );
        let preview: String = msg.chars().take(64).collect();
        assert!(
            msg.starts_with(expected_prefix),
            "{sql}: unexpected BadResponse prefix: {preview:?}"
        );
    }

    assert!(
        sent_prose.load(Ordering::SeqCst) < 512 * 1024,
        "server sent excessive oversized prose body ({} bytes)",
        sent_prose.load(Ordering::SeqCst)
    );
    assert!(
        sent_158.load(Ordering::SeqCst) < 512 * 1024,
        "server sent excessive oversized 158 body ({} bytes)",
        sent_158.load(Ordering::SeqCst)
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}
