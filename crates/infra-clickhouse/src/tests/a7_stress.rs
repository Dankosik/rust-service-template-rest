use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use operation_context::{OperationContext, Stopped};
use tokio::sync::Notify;

use super::{SampleRow, base_options, rbwnat_describe_id_column, spawn_http_server};
use crate::{BatchInsertOptions, ClickHouse, CloseOutcome, InsertError, UnknownOutcome};

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
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            if uri.contains("DESCRIBE") || body_str.contains("DESCRIBE") {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            open.fetch_add(1, Ordering::SeqCst);
            recv.notify_one();
            release.notified().await;
            open.fetch_sub(1, Ordering::SeqCst);
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
    opts.max_concurrency = 4;
    let ch = ClickHouse::start(opts).await.expect("start");

    let baseline_tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    for i in 0..30_u64 {
        let ctx = OperationContext::with_timeout(Duration::from_secs(2));
        let notified = insert_received.notified();
        tokio::pin!(notified);
        let row = [SampleRow { id: i }];
        let opts = BatchInsertOptions::new();
        let fut = ch.insert_batch(&ctx, "a7_cancel", "events", &row, opts);
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
    let tasks_after = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let open_after = open_inserts.load(Ordering::SeqCst);

    release_handlers.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;
    srv.abort();

    assert_eq!(
        open_after,
        0,
        "A7 falsified: {open_after} open insert connections and {} detached tasks remained after 4x insert_timeout and close()",
        tasks_after.saturating_sub(baseline_tasks)
    );
}

#[tokio::test]
async fn a7_local_timeout_insert_does_not_leak_detached_tasks_or_connections() {
    let (addr, open_inserts, _insert_received, release_handlers, srv) =
        spawn_stalling_insert_server().await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.insert_timeout = Duration::from_millis(40);
    opts.max_concurrency = 4;
    let ch = ClickHouse::start(opts).await.expect("start");

    let baseline_tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let ctx = OperationContext::unbounded();
    for i in 0..10_u64 {
        let res = ch
            .insert_batch(
                &ctx,
                "a7_timeout",
                "events",
                &[SampleRow { id: i }],
                BatchInsertOptions::new(),
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
    let tasks_after = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    let open_after = open_inserts.load(Ordering::SeqCst);

    release_handlers.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;
    srv.abort();

    assert_eq!(
        open_after,
        0,
        "A7 falsified on local insert_timeout: {open_after} open insert connections and {} detached tasks remained after 5x insert_timeout and close()",
        tasks_after.saturating_sub(baseline_tasks)
    );
}
