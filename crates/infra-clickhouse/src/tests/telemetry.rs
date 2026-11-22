use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use observe_test_support::{EventCapture, SpanCapture};
use opentelemetry::trace::{SpanKind, TracerProvider as _};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use operation_context::OperationContext;
use tokio::sync::{Mutex, Notify};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt as _;

use super::{
    SampleRow, base_options, prometheus_recorder, rbwnat_describe_id_column, rbwnat_u64_rows,
    spawn_http_server,
};
use crate::{
    AsyncInsertOptions, BatchInsertOptions, ClickHouse, CloseOutcome, InsertError, InsertFailure,
    QueryError,
};

fn count_lines_with(scrape: &str, prefix: &str, labels: &[&str]) -> u64 {
    scrape
        .lines()
        .filter(|line| line.starts_with(prefix) && labels.iter().all(|l| line.contains(l)))
        .map(|line| {
            line.rsplit_once(' ')
                .and_then(|(_, v)| v.parse::<u64>().ok())
                .unwrap_or(0)
        })
        .sum()
}

fn gauge_value(scrape: &str, name: &str) -> Option<f64> {
    scrape
        .lines()
        .find(|line| line.starts_with(name) && !line.starts_with('#'))
        .and_then(|line| line.rsplit_once(' '))
        .and_then(|(_, v)| v.parse::<f64>().ok())
}

struct TelemetryServer {
    addr: std::net::SocketAddr,
    traceparents: Arc<Mutex<Vec<Option<String>>>>,
    stall_describe: Arc<AtomicUsize>,
    stall_insert: Arc<AtomicUsize>,
    stalled_notify: Arc<Notify>,
    release_stall: Arc<Notify>,
    handle: tokio::task::JoinHandle<()>,
}

async fn spawn_telemetry_server() -> TelemetryServer {
    let traceparents = Arc::new(Mutex::new(Vec::new()));
    let stall_describe = Arc::new(AtomicUsize::new(0));
    let stall_insert = Arc::new(AtomicUsize::new(0));
    let stalled_notify = Arc::new(Notify::new());
    let release_stall = Arc::new(Notify::new());

    let traceparents_srv = Arc::clone(&traceparents);
    let describe_stall = Arc::clone(&stall_describe);
    let insert_stall = Arc::clone(&stall_insert);
    let stalled_sig = Arc::clone(&stalled_notify);
    let release_sig = Arc::clone(&release_stall);
    let (addr, handle) = spawn_http_server(move |req: Request<Incoming>| {
        let tp = Arc::clone(&traceparents_srv);
        let sd = Arc::clone(&describe_stall);
        let si = Arc::clone(&insert_stall);
        let sn = Arc::clone(&stalled_sig);
        let rs = Arc::clone(&release_sig);
        async move {
            let uri = req.uri().to_string();
            let header = req
                .headers()
                .get("traceparent")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            tp.lock().await.push(header);
            let body = req.into_body().collect().await.expect("body").to_bytes();
            let s = String::from_utf8_lossy(&body);
            if s.contains("DESCRIBE") || uri.contains("DESCRIBE") {
                if sd.load(Ordering::SeqCst) == 1 {
                    sn.notify_one();
                    rs.notified().await;
                }
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(rbwnat_describe_id_column()))
                    .expect("resp");
            }
            if uri.contains("INSERT") || s.contains("INSERT") {
                if si.load(Ordering::SeqCst) == 1 {
                    sn.notify_one();
                    rs.notified().await;
                }
                return Response::builder()
                    .status(StatusCode::OK)
                    .body(Full::new(Bytes::new()))
                    .expect("resp");
            }
            if s.contains("read_stall") {
                sn.notify_one();
                rs.notified().await;
            }
            let (status, resp) = match () {
                () if s.contains("too_many") => (StatusCode::OK, rbwnat_u64_rows("id", &[1, 2])),
                () if s.contains("raw_db_err") => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Bytes::from_static(b"Code: 47. DB::Exception: SENSITIVE_RAW_SQL_LEAK_MARKER"),
                ),
                () => (StatusCode::OK, rbwnat_u64_rows("id", &[1])),
            };
            Response::builder()
                .status(status)
                .body(Full::new(resp))
                .expect("resp")
        }
    })
    .await;

    TelemetryServer {
        addr,
        traceparents,
        stall_describe,
        stall_insert,
        stalled_notify,
        release_stall,
        handle,
    }
}

async fn exercise_drop_and_busy_paths(
    ch: &ClickHouse,
    ctx: &OperationContext,
    srv: &TelemetryServer,
) {
    let mut fut = Box::pin(
        ch.query("drop_read", "SELECT read_stall")
            .fetch_one::<SampleRow>(ctx),
    );
    tokio::select! {
        res = &mut fut => panic!("unexpected read completion: {res:?}"),
        () = srv.stalled_notify.notified() => {
            assert_eq!(
                ch.query("busy_shed", "SELECT 1").fetch_one::<SampleRow>(ctx).await,
                Err(QueryError::Busy)
            );
            assert_eq!(
                ch.insert_batch(ctx, "busy_ins", "events_table", &[SampleRow { id: 1 }], BatchInsertOptions::new()).await,
                Err(InsertError::NotApplied(InsertFailure::Busy))
            );
            drop(fut);
        }
    }
    srv.release_stall.notify_waiters();

    srv.stall_describe.store(1, Ordering::SeqCst);
    let mut pre_fut = Box::pin(ch.insert_batch(
        ctx,
        "drop_pre_dispatch",
        "cold_table",
        &[SampleRow { id: 1 }],
        BatchInsertOptions::new(),
    ));
    tokio::select! {
        res = &mut pre_fut => panic!("unexpected pre-dispatch completion: {res:?}"),
        () = srv.stalled_notify.notified() => drop(pre_fut),
    }
    srv.stall_describe.store(0, Ordering::SeqCst);
    srv.release_stall.notify_waiters();

    srv.stall_insert.store(1, Ordering::SeqCst);
    let mut post_fut = Box::pin(ch.insert_batch(
        ctx,
        "drop_post_dispatch",
        "events_table",
        &[SampleRow { id: 1 }],
        BatchInsertOptions::new(),
    ));
    tokio::select! {
        res = &mut post_fut => panic!("unexpected post-dispatch completion: {res:?}"),
        () = srv.stalled_notify.notified() => drop(post_fut),
    }
    srv.stall_insert.store(0, Ordering::SeqCst);
    srv.release_stall.notify_waiters();
}

async fn assert_pre_admission_silent(
    ch: &ClickHouse,
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    spans: &SpanCapture,
) {
    let pre_cancelled = OperationContext::unbounded();
    pre_cancelled.cancel();
    let _ = ch
        .query("pre_cancel", "SELECT 1")
        .fetch_one::<SampleRow>(&pre_cancelled)
        .await;
    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    let _ = ch
        .query("pre_invalid", "   ")
        .fetch_one::<SampleRow>(&ctx)
        .await;
    let _ = ch
        .insert_batch(
            &ctx,
            "pre_bad",
            "db.t",
            &[SampleRow { id: 1 }],
            BatchInsertOptions::new(),
        )
        .await;
    let _ = ch
        .insert_async(
            &ctx,
            "zero",
            "events",
            &[] as &[SampleRow],
            AsyncInsertOptions::new(),
        )
        .await;
    assert_eq!(spans.snapshot(), []);
    assert_eq!(
        count_lines_with(
            &recorder.handle().render(),
            "clickhouse_operation_duration_seconds_count",
            &[]
        ),
        0
    );
}

async fn exercise_read_telemetry(
    ch: &ClickHouse,
    ctx: &OperationContext,
    srv: &TelemetryServer,
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    exporter: &InMemorySpanExporter,
) {
    let ok_row = ch
        .query(
            "read_ok_summary",
            "SELECT id FROM secret_table WHERE k = {k:String}",
        )
        .param("k", "SENSITIVE_PARAM_SENTINEL")
        .fetch_one::<SampleRow>(ctx)
        .await
        .expect("read ok");
    assert_eq!(ok_row.id, 1);

    let tp = srv
        .traceparents
        .lock()
        .await
        .last()
        .cloned()
        .flatten()
        .expect("traceparent");
    let finished = exporter.get_finished_spans().expect("spans");
    let ok_span = finished.last().expect("ok span");
    assert_eq!(ok_span.span_kind, SpanKind::Client);
    assert_eq!(
        tp,
        format!(
            "00-{}-{}-01",
            ok_span.span_context.trace_id(),
            ok_span.span_context.span_id()
        )
    );
    assert!(
        !ok_span
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "error.type")
    );
    assert!(ok_span.attributes.iter().any(|kv| {
        kv.key.as_str() == "server.port" && kv.value.as_str() == srv.addr.port().to_string()
    }));

    let bytes_after_ok = count_lines_with(
        &recorder.handle().render(),
        "clickhouse_read_bytes_total",
        &[],
    );
    assert!(bytes_after_ok > 0);
    assert_eq!(
        ch.query("too_many_summary", "SELECT too_many")
            .fetch_one::<SampleRow>(ctx)
            .await,
        Err(QueryError::TooManyRows)
    );
    assert_eq!(
        ch.query("raw_err_summary", "SELECT raw_db_err")
            .max_result_rows(NonZeroU64::new(10).expect("nz"))
            .fetch_one::<SampleRow>(ctx)
            .await,
        Err(QueryError::Rejected)
    );
    assert_eq!(
        count_lines_with(
            &recorder.handle().render(),
            "clickhouse_read_rows_total",
            &[]
        ),
        1
    );
    assert_eq!(
        count_lines_with(
            &recorder.handle().render(),
            "clickhouse_read_bytes_total",
            &[]
        ),
        bytes_after_ok
    );
}

fn assert_scrape_and_redaction(
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    exporter: &InMemorySpanExporter,
    spans: &SpanCapture,
    events: &EventCapture,
) {
    let scrape = recorder.handle().render();
    assert_eq!(
        gauge_value(&scrape, "clickhouse_operations_in_flight"),
        Some(0.0)
    );
    assert_eq!(
        count_lines_with(&scrape, "clickhouse_insert_rows_total", &["mode=\"batch\""]),
        2
    );
    assert_eq!(
        count_lines_with(
            &scrape,
            "clickhouse_operation_duration_seconds_count",
            &["operation=\"insert_batch\"", "outcome=\"cancelled\""]
        ),
        1
    );
    assert_eq!(
        count_lines_with(
            &scrape,
            "clickhouse_operation_duration_seconds_count",
            &["operation=\"insert_batch\"", "outcome=\"outcome_unknown\""]
        ),
        1
    );
    assert_eq!(
        count_lines_with(
            &scrape,
            "clickhouse_admission_refused_total",
            &["operation=\"fetch_one\""]
        ),
        1
    );

    let dump = format!(
        "{scrape}\n{:?}\n{:?}\n{:?}",
        exporter.get_finished_spans().expect("all spans"),
        spans.snapshot(),
        events.snapshot()
    );
    for forbidden in [
        "SENSITIVE_RAW_SQL_LEAK_MARKER",
        "SUPER_SECRET_PASSWORD_SENTINEL",
        "SENSITIVE_PARAM_SENTINEL",
        "secret_table",
        "events_table",
        "cold_table",
    ] {
        assert!(!dump.contains(forbidden), "leaked {forbidden}");
    }
}

#[tokio::test]
async fn telemetry_contract_spans_w3c_metrics_and_redaction() {
    let recorder = prometheus_recorder();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let spans = SpanCapture::new();
    let events = EventCapture::new();
    let subscriber = tracing_subscriber::registry()
        .with(filter_fn(|meta| !meta.target().starts_with("clickhouse")))
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
        .with(spans.clone())
        .with(events.clone());
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let srv = spawn_telemetry_server().await;
    let mut opts = base_options(format!("http://{}", srv.addr));
    opts.max_concurrency = 1;
    opts.password = Some(secrecy::SecretString::from(
        "SUPER_SECRET_PASSWORD_SENTINEL",
    ));
    let ch = ClickHouse::start(opts).await.expect("start");

    assert_eq!(
        gauge_value(
            &recorder.handle().render(),
            "clickhouse_operations_in_flight"
        ),
        Some(0.0)
    );
    assert_pre_admission_silent(&ch, &recorder, &spans).await;

    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    exercise_read_telemetry(&ch, &ctx, &srv, &recorder, &exporter).await;

    ch.insert_batch(
        &ctx,
        "batch_ok",
        "events_table",
        &[SampleRow { id: 1 }, SampleRow { id: 2 }],
        BatchInsertOptions::new(),
    )
    .await
    .expect("batch ok");
    exercise_drop_and_busy_paths(&ch, &ctx, &srv).await;

    let mut no_tp_opts = base_options(format!("http://{}", srv.addr));
    no_tp_opts.propagate_trace_context = false;
    let ch_no_tp = ClickHouse::start(no_tp_opts).await.expect("start no tp");
    let before_tp = srv.traceparents.lock().await.len();
    ch_no_tp
        .query("no_tp_q", "SELECT 1")
        .fetch_one::<SampleRow>(&ctx)
        .await
        .expect("no tp ok");
    assert_eq!(srv.traceparents.lock().await[before_tp], None);

    assert_scrape_and_redaction(&recorder, &exporter, &spans, &events);

    assert_eq!(
        ch.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    assert_eq!(
        ch_no_tp.close(Duration::from_secs(1)).await,
        CloseOutcome::Complete
    );
    srv.handle.abort();
}
