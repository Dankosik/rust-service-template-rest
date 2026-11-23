use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use observe_test_support::EventCapture;
use operation_context::OperationContext;
use tokio::sync::{Mutex, Notify};

use super::{
    SampleRow, base_options, prometheus_recorder, rbwnat_describe_id_column, rbwnat_u64_rows,
    spawn_http_server,
};
use crate::{BatchInsertOptions, ClickHouse, CloseOutcome, InsertError, InsertFailure, QueryError};

fn metric_counter(scrape: &str, name: &str, outcome: &str) -> u64 {
    let label = format!("outcome=\"{outcome}\"");
    scrape
        .lines()
        .find(|line| line.starts_with(name) && line.contains(&label))
        .and_then(|line| line.rsplit_once(' '))
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .unwrap_or(0)
}

async fn wait_for_scrape(
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    pred: impl Fn(&str) -> bool,
) -> String {
    for _ in 0..50_000 {
        let scrape = recorder.handle().render();
        if pred(&scrape) {
            return scrape;
        }
        tokio::task::yield_now().await;
    }
    recorder.handle().render()
}

async fn spawn_auth_server(
    expected_password: Arc<Mutex<String>>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    spawn_http_server(move |req: Request<Incoming>| {
        let expected = Arc::clone(&expected_password);
        async move {
            let uri = req.uri().to_string();
            let key = req
                .headers()
                .get("X-ClickHouse-Key")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let body_str = String::from_utf8_lossy(&body);
            let wanted = expected.lock().await.clone();
            if key.as_deref() != Some(wanted.as_str()) {
                return Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Full::new(Bytes::from_static(
                        b"Code: 516. DB::Exception: Authentication failed",
                    )))
                    .expect("resp");
            }
            if body_str.contains("DESCRIBE") || uri.contains("DESCRIBE") {
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            let resp = if uri.contains("INSERT") || body_str.contains("INSERT") {
                Bytes::new()
            } else {
                rbwnat_u64_rows("id", &[1])
            };
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(resp))
                .expect("resp")
        }
    })
    .await
}

async fn write_password_atomic(staging: &std::path::Path, path: &std::path::Path, content: &str) {
    tokio::fs::write(staging, content)
        .await
        .expect("write staging password");
    tokio::fs::rename(staging, path)
        .await
        .expect("rename password file");
}

fn assert_rotation_evidence_and_redaction(events: &EventCapture, scrape: &str, forbidden: &[&str]) {
    let captured = events.snapshot();
    assert_eq!(
        captured
            .iter()
            .filter(|e| e.message() == Some("clickhouse_password_file_unreadable"))
            .count(),
        1
    );
    let dump = format!("{captured:?}\n{scrape}");
    for secret in forbidden {
        assert!(!dump.contains(secret), "leaked {secret}");
    }
}

async fn spawn_hold_first_auth_server(
    seen_keys: Arc<Mutex<Vec<Option<String>>>>,
    hold_first: Arc<Notify>,
    release_first: Arc<Notify>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let first_flag = Arc::new(AtomicUsize::new(1));
    spawn_http_server(move |req: Request<Incoming>| {
        let keys = Arc::clone(&seen_keys);
        let hold = Arc::clone(&hold_first);
        let rel = Arc::clone(&release_first);
        let flag = Arc::clone(&first_flag);
        async move {
            let key = req
                .headers()
                .get("X-ClickHouse-Key")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            keys.lock().await.push(key);
            let _ = req.into_body().collect().await;
            if flag.swap(0, Ordering::SeqCst) == 1 {
                hold.notify_one();
                rel.notified().await;
            }
            Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(rbwnat_u64_rows("id", &[9])))
                .expect("resp")
        }
    })
    .await
}

async fn advance_refresh_tick() {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::resume();
}

#[tokio::test]
async fn password_file_rotates_every_5s_and_retains_valid_password_on_ctl_error() {
    let recorder = prometheus_recorder();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);
    let events = EventCapture::new();
    let _events_guard = events.install_for(&["infra_clickhouse::credentials"]);

    let dir = tempfile::tempdir().expect("tempdir");
    let pw_path = dir.path().join("password");
    let staging = dir.path().join("password.tmp");
    tokio::fs::write(&pw_path, "pw-initial-secret\n")
        .await
        .expect("write initial");

    let expected_pw = Arc::new(Mutex::new("pw-initial-secret".to_owned()));
    let (addr, srv) = spawn_auth_server(Arc::clone(&expected_pw)).await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.password = None;
    opts.password_file = Some(pw_path.clone());
    opts.query_timeout = Duration::from_secs(30);
    opts.insert_timeout = Duration::from_secs(30);
    let ch = ClickHouse::start(opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(60));

    assert_eq!(
        ch.query("q1", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await
            .expect("initial ok")
            .id,
        1
    );

    advance_refresh_tick().await;
    let _ = wait_for_scrape(&recorder, |s| {
        metric_counter(s, "clickhouse_password_file_refreshes_total", "unchanged") == 1
    })
    .await;

    write_password_atomic(&staging, &pw_path, "pw-rotated-secret\r\n").await;
    *expected_pw.lock().await = "pw-rotated-secret".to_owned();

    advance_refresh_tick().await;
    let _ = wait_for_scrape(&recorder, |s| {
        metric_counter(s, "clickhouse_password_file_refreshes_total", "updated") == 1
    })
    .await;

    assert_eq!(
        ch.query("q2", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await
            .expect("rotated read ok")
            .id,
        1
    );
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "ins_rot",
            "events",
            &[SampleRow { id: 7 }],
            BatchInsertOptions::new()
        )
        .await
        .expect("rotated insert ok")
        .rows,
        1
    );

    write_password_atomic(&staging, &pw_path, "pw-bad\nembedded-line\n").await;
    advance_refresh_tick().await;
    let _ = wait_for_scrape(&recorder, |s| {
        metric_counter(s, "clickhouse_password_file_refreshes_total", "read_failed") == 1
    })
    .await;

    advance_refresh_tick().await;
    let scrape = wait_for_scrape(&recorder, |s| {
        metric_counter(s, "clickhouse_password_file_refreshes_total", "read_failed") == 2
    })
    .await;

    assert_eq!(
        ch.query("q3", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await
            .expect("retained password read ok")
            .id,
        1
    );
    assert_rotation_evidence_and_redaction(
        &events,
        &scrape,
        &[
            "pw-initial-secret",
            "pw-rotated-secret",
            "pw-bad",
            "embedded-line",
        ],
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}

#[tokio::test]
async fn in_flight_call_keeps_starting_credential_and_drop_cancels_refresh() {
    let recorder = prometheus_recorder();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let dir = tempfile::tempdir().expect("tempdir");
    let pw_path = dir.path().join("password");
    let staging = dir.path().join("password.tmp");
    tokio::fs::write(&pw_path, "pw-v1\n")
        .await
        .expect("write v1");

    let seen_keys = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let hold_first = Arc::new(Notify::new());
    let release_first = Arc::new(Notify::new());
    let (addr, srv) = spawn_hold_first_auth_server(
        Arc::clone(&seen_keys),
        Arc::clone(&hold_first),
        Arc::clone(&release_first),
    )
    .await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.password = None;
    opts.password_file = Some(pw_path.clone());
    opts.query_timeout = Duration::from_secs(30);
    let ch = ClickHouse::start(opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(60));

    let ch_first = ch.clone();
    let ctx_first = ctx.clone();
    let first = tokio::spawn(async move {
        ch_first
            .query("first", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx_first)
            .await
    });
    hold_first.notified().await;

    write_password_atomic(&staging, &pw_path, "pw-v2\n").await;
    advance_refresh_tick().await;
    let _ = wait_for_scrape(&recorder, |s| {
        metric_counter(s, "clickhouse_password_file_refreshes_total", "updated") == 1
    })
    .await;

    release_first.notify_one();
    assert_eq!(first.await.expect("join").expect("first ok").id, 9);
    assert_eq!(
        ch.query("second", "SELECT 2")
            .fetch_one::<SampleRow>(&ctx)
            .await
            .expect("second ok")
            .id,
        9
    );
    assert_eq!(
        seen_keys.lock().await.as_slice(),
        &[Some("pw-v1".to_owned()), Some("pw-v2".to_owned())]
    );

    let tasks_with_client = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();
    drop(ch);
    for _ in 0..500 {
        if tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks()
            < tasks_with_client
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks()
            < tasks_with_client,
        "refresh task did not exit after dropping ClickHouse without close()"
    );
    srv.abort();
}

async fn assert_updated_reaches(
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    expected: u64,
    msg: &str,
) {
    let scrape = wait_for_scrape(recorder, |s| {
        metric_counter(s, "clickhouse_password_file_refreshes_total", "updated") == expected
    })
    .await;
    assert_eq!(
        metric_counter(
            &scrape,
            "clickhouse_password_file_refreshes_total",
            "updated"
        ),
        expected,
        "{msg}"
    );
}

async fn yield_repeatedly(times: usize) {
    for _ in 0..times {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn password_file_reloads_reactively_on_query_and_insert_with_cooldown() {
    let recorder = prometheus_recorder();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let dir = tempfile::tempdir().expect("tempdir");
    let pw_path = dir.path().join("password");
    let staging = dir.path().join("password.tmp");
    tokio::fs::write(&pw_path, "pw-v1\n")
        .await
        .expect("write initial");

    let expected_pw = Arc::new(Mutex::new("pw-v1".to_owned()));
    let (addr, srv) = spawn_auth_server(Arc::clone(&expected_pw)).await;

    let mut opts = base_options(format!("http://{addr}"));
    opts.password = None;
    opts.password_file = Some(pw_path.clone());
    opts.query_timeout = Duration::from_secs(30);
    opts.insert_timeout = Duration::from_secs(30);
    let ch = ClickHouse::start(opts).await.expect("start");
    let ctx = OperationContext::with_timeout(Duration::from_secs(60));

    assert_eq!(
        ch.query("initial", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await
            .expect("initial ok")
            .id,
        1
    );

    write_password_atomic(&staging, &pw_path, "pw-v2\n").await;
    *expected_pw.lock().await = "pw-v2".to_owned();
    assert_eq!(
        ch.query("stale_query_516", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await,
        Err(QueryError::Rejected)
    );
    assert_updated_reaches(&recorder, 1, "query 516 triggers immediate reload").await;
    assert_eq!(
        ch.query("recovered_query", "SELECT 1")
            .fetch_one::<SampleRow>(&ctx)
            .await
            .expect("query recovered")
            .id,
        1
    );

    write_password_atomic(&staging, &pw_path, "pw-v3\n").await;
    *expected_pw.lock().await = "pw-v3".to_owned();
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "stale_insert_516",
            "events",
            &[SampleRow { id: 2 }],
            BatchInsertOptions::new(),
        )
        .await,
        Err(InsertError::NotApplied(InsertFailure::Rejected))
    );

    tokio::time::pause();
    yield_repeatedly(200).await;
    tokio::time::advance(Duration::from_millis(500)).await;
    yield_repeatedly(200).await;
    assert_eq!(
        metric_counter(
            &recorder.handle().render(),
            "clickhouse_password_file_refreshes_total",
            "updated"
        ),
        1,
        "reactive refresh must respect 1s cooldown throttle"
    );

    tokio::time::advance(Duration::from_millis(600)).await;
    tokio::time::resume();
    assert_updated_reaches(
        &recorder,
        2,
        "insert 516 triggers reactive reload after 1s cooldown (< 5s poll)",
    )
    .await;
    assert_eq!(
        ch.insert_batch(
            &ctx,
            "recovered_insert",
            "events",
            &[SampleRow { id: 3 }],
            BatchInsertOptions::new(),
        )
        .await
        .expect("insert recovered")
        .rows,
        1
    );

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.abort();
}
