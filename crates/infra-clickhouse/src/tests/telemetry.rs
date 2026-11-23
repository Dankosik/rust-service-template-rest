use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Bytes, Incoming};
use observe_test_support::{EventCapture, SpanCapture};
use opentelemetry::trace::{SpanKind, Status, TracerProvider as _};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
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
    query_ids: Arc<Mutex<Vec<String>>>,
    stall_describe: Arc<AtomicUsize>,
    stall_insert: Arc<AtomicUsize>,
    stalled_notify: Arc<Notify>,
    release_stall: Arc<Notify>,
    handle: tokio::task::JoinHandle<()>,
}

fn extract_wire_query_id(uri: &http::Uri) -> Option<String> {
    uri.query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("query_id="))
        .map(str::to_owned)
}

fn telemetry_read_response(body: &str) -> (StatusCode, Bytes) {
    match () {
        () if body.contains("too_many") => (StatusCode::OK, rbwnat_u64_rows("id", &[1, 2])),
        () if body.contains("raw_db_err") => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"Code: 47. DB::Exception: SENSITIVE_RAW_SQL_LEAK_MARKER"),
        ),
        () if body.contains("auth_516") => (
            StatusCode::FORBIDDEN,
            Bytes::from_static(b"Code: 516. DB::Exception: Authentication failed"),
        ),
        () if body.contains("access_497") => (
            StatusCode::FORBIDDEN,
            Bytes::from_static(b"Code: 497. DB::Exception: Not enough privileges"),
        ),
        () if body.contains("readonly_164") => (
            StatusCode::FORBIDDEN,
            Bytes::from_static(b"Code: 164. DB::Exception: Cannot modify setting"),
        ),
        () if body.contains("timeout_159") => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"Code: 159. DB::Exception: Timeout exceeded"),
        ),
        () if body.contains("oom_241") => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(b"Code: 241. DB::Exception: Memory limit exceeded"),
        ),
        () => (StatusCode::OK, rbwnat_u64_rows("id", &[1])),
    }
}

async fn spawn_telemetry_server() -> TelemetryServer {
    let traceparents = Arc::new(Mutex::new(Vec::new()));
    let query_ids = Arc::new(Mutex::new(Vec::new()));
    let stall_describe = Arc::new(AtomicUsize::new(0));
    let stall_insert = Arc::new(AtomicUsize::new(0));
    let stalled_notify = Arc::new(Notify::new());
    let release_stall = Arc::new(Notify::new());

    let traceparents_srv = Arc::clone(&traceparents);
    let query_ids_srv = Arc::clone(&query_ids);
    let describe_stall = Arc::clone(&stall_describe);
    let insert_stall = Arc::clone(&stall_insert);
    let stalled_sig = Arc::clone(&stalled_notify);
    let release_sig = Arc::clone(&release_stall);
    let (addr, handle) = spawn_http_server(move |req: Request<Incoming>| {
        let tp = Arc::clone(&traceparents_srv);
        let qids = Arc::clone(&query_ids_srv);
        let sd = Arc::clone(&describe_stall);
        let si = Arc::clone(&insert_stall);
        let sn = Arc::clone(&stalled_sig);
        let rs = Arc::clone(&release_sig);
        async move {
            let wire_qid = extract_wire_query_id(req.uri());
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
            if let Some(qid) = wire_qid {
                qids.lock().await.push(qid);
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
            if s.contains("hang_forever") {
                sn.notify_one();
                std::future::pending::<()>().await;
            }
            let (status, resp) = telemetry_read_response(&s);
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
        query_ids,
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

async fn exercise_read_failure_diagnostics(
    ch: &ClickHouse,
    ctx: &OperationContext,
    srv: &TelemetryServer,
) {
    for (summary, sql, expected) in [
        ("auth_err", "SELECT auth_516", QueryError::Rejected),
        ("access_err", "SELECT access_497", QueryError::Rejected),
        ("readonly_err", "SELECT readonly_164", QueryError::Rejected),
        (
            "server_timeout_err",
            "SELECT timeout_159",
            QueryError::Unavailable,
        ),
        ("oom_err", "SELECT oom_241", QueryError::Unavailable),
    ] {
        assert_eq!(
            ch.query(summary, sql).fetch_one::<SampleRow>(ctx).await,
            Err(expected),
            "{summary}"
        );
    }

    let notified = srv.stalled_notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let mut client_to = Box::pin(
        ch.query("client_timeout_err", "SELECT hang_forever")
            .timeout(Duration::from_millis(500))
            .fetch_one::<SampleRow>(ctx),
    );
    tokio::select! {
        res = &mut client_to => panic!("unexpected client_timeout_err completion: {res:?}"),
        () = notified => {}
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(client_to.await, Err(QueryError::Unavailable));
    tokio::time::resume();

    let notified = srv.stalled_notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let short_parent = OperationContext::with_timeout(Duration::from_millis(500));
    let mut parent_to = Box::pin(
        ch.query("parent_deadline_err", "SELECT hang_forever")
            .fetch_one::<SampleRow>(&short_parent),
    );
    tokio::select! {
        res = &mut parent_to => panic!("unexpected parent_deadline_err completion: {res:?}"),
        () = notified => {}
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        parent_to.await,
        Err(QueryError::Stopped(operation_context::Stopped::Deadline))
    );
    tokio::time::resume();
}

fn otel_attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.as_str().into_owned())
}

struct ExpectedSpan {
    db_op: &'static str,
    summary: &'static str,
    error_type: Option<&'static str>,
    reason: Option<&'static str>,
    status_code: Option<&'static str>,
    expect_query_id: bool,
    event_level: Option<tracing::Level>,
}

impl ExpectedSpan {
    const fn ok(db_op: &'static str, summary: &'static str) -> Self {
        Self {
            db_op,
            summary,
            error_type: None,
            reason: None,
            status_code: None,
            expect_query_id: true,
            event_level: None,
        }
    }

    const fn err(
        db_op: &'static str,
        summary: &'static str,
        error_type: &'static str,
        reason: &'static str,
        status_code: Option<&'static str>,
        expect_query_id: bool,
        event_level: tracing::Level,
    ) -> Self {
        Self {
            db_op,
            summary,
            error_type: Some(error_type),
            reason: Some(reason),
            status_code,
            expect_query_id,
            event_level: Some(event_level),
        }
    }
}

const EXPECTED_SPANS: &[ExpectedSpan] = &[
    ExpectedSpan::ok("SELECT", "read_ok_summary"),
    ExpectedSpan::err(
        "SELECT",
        "too_many_summary",
        "too_many_rows",
        "too_many_rows",
        None,
        true,
        tracing::Level::DEBUG,
    ),
    ExpectedSpan::err(
        "SELECT",
        "raw_err_summary",
        "rejected",
        "schema",
        Some("47"),
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "auth_err",
        "rejected",
        "auth",
        Some("516"),
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "access_err",
        "rejected",
        "access",
        Some("497"),
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "readonly_err",
        "rejected",
        "readonly",
        Some("164"),
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "server_timeout_err",
        "unavailable",
        "server_timeout",
        Some("159"),
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "oom_err",
        "unavailable",
        "memory_limit",
        Some("241"),
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "client_timeout_err",
        "unavailable",
        "client_timeout",
        None,
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::err(
        "SELECT",
        "parent_deadline_err",
        "timeout",
        "parent_deadline",
        None,
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::ok("INSERT", "batch_ok"),
    ExpectedSpan::err(
        "SELECT",
        "drop_read",
        "cancelled",
        "future_dropped",
        None,
        true,
        tracing::Level::DEBUG,
    ),
    ExpectedSpan::err(
        "INSERT",
        "drop_pre_dispatch",
        "cancelled",
        "future_dropped",
        None,
        false,
        tracing::Level::DEBUG,
    ),
    ExpectedSpan::err(
        "INSERT",
        "drop_post_dispatch",
        "outcome_unknown",
        "future_dropped",
        None,
        true,
        tracing::Level::WARN,
    ),
    ExpectedSpan::ok("SELECT", "no_tp_q"),
];

fn assert_failure_event(
    spec: &ExpectedSpan,
    cap: &observe_test_support::CapturedSpan,
    captured_events: &[observe_test_support::CapturedEvent],
    error_type: &str,
) {
    let summary = spec.summary;
    let event = captured_events
        .iter()
        .find(|ev| ev.unquoted("db.query.summary") == Some(summary))
        .unwrap_or_else(|| panic!("missing failure event for {summary}"));
    assert_eq!(
        event.message(),
        Some("clickhouse_operation_failed"),
        "{summary}"
    );
    assert_eq!(Some(event.level), spec.event_level, "{summary}");
    assert_eq!(event.unquoted("error.type"), Some(error_type), "{summary}");
    assert_eq!(
        event.unquoted("clickhouse.error.reason"),
        spec.reason,
        "{summary}"
    );
    assert_eq!(
        event.unquoted("db.response.status_code"),
        spec.status_code,
        "{summary}"
    );
    assert_eq!(
        event.unquoted("clickhouse.query_id"),
        cap.unquoted("clickhouse.query_id"),
        "{summary}"
    );
}

fn assert_single_span_and_event(
    spec: &ExpectedSpan,
    otel: &SpanData,
    cap: &observe_test_support::CapturedSpan,
    captured_events: &[observe_test_support::CapturedEvent],
    wire_query_ids: &[String],
    expected_address: &str,
    expected_port: &str,
) {
    let summary = spec.summary;
    assert_eq!(otel.span_kind, SpanKind::Client, "{summary}");
    assert_eq!(otel.name.as_ref(), format!("{} {summary}", spec.db_op));
    for (key, value) in [
        ("db.system.name", "clickhouse"),
        ("db.operation.name", spec.db_op),
        ("db.query.summary", summary),
        ("db.namespace", "app"),
        ("server.address", expected_address),
        ("server.port", expected_port),
    ] {
        assert_eq!(
            otel_attr(otel, key).as_deref(),
            Some(value),
            "{summary}:{key}"
        );
        let cap_val = if key == "server.port" {
            cap.field(key)
        } else {
            cap.unquoted(key)
        };
        assert_eq!(cap_val, Some(value), "{summary}:{key}");
    }
    let otel_qid = otel_attr(otel, "clickhouse.query_id");
    let cap_qid = cap.unquoted("clickhouse.query_id");
    if spec.expect_query_id {
        let qid = otel_qid
            .as_deref()
            .unwrap_or_else(|| panic!("missing OTel clickhouse.query_id for {summary}"));
        assert_eq!(cap_qid, Some(qid), "{summary}");
        assert!(uuid::Uuid::parse_str(qid).is_ok(), "{summary}: {qid}");
        assert!(
            wire_query_ids.iter().any(|wire| wire == qid),
            "{summary}: query_id {qid} missing from wire requests {wire_query_ids:?}"
        );
    } else {
        assert_eq!(otel_qid, None, "{summary}");
        assert_eq!(cap_qid, None, "{summary}");
    }
    assert_eq!(
        otel_attr(otel, "db.response.status_code").as_deref(),
        spec.status_code,
        "{summary}"
    );
    assert_eq!(
        cap.unquoted("db.response.status_code"),
        spec.status_code,
        "{summary}"
    );
    assert_eq!(
        otel_attr(otel, "clickhouse.error.reason").as_deref(),
        spec.reason,
        "{summary}"
    );
    assert_eq!(
        cap.unquoted("clickhouse.error.reason"),
        spec.reason,
        "{summary}"
    );

    if let Some(error_type) = spec.error_type {
        assert_eq!(
            otel_attr(otel, "error.type").as_deref(),
            Some(error_type),
            "{summary}"
        );
        assert!(matches!(otel.status, Status::Error { .. }), "{summary}");
        assert_eq!(cap.unquoted("error.type"), Some(error_type), "{summary}");
        assert_eq!(cap.unquoted("otel.status_code"), Some("ERROR"), "{summary}");
        assert_failure_event(spec, cap, captured_events, error_type);
    } else {
        assert_eq!(otel_attr(otel, "error.type"), None, "{summary}");
        assert_eq!(otel.status, Status::Unset, "{summary}");
        assert_eq!(cap.unquoted("error.type"), None, "{summary}");
        assert_eq!(cap.unquoted("otel.status_code"), None, "{summary}");
        assert!(
            !captured_events
                .iter()
                .any(|ev| ev.unquoted("db.query.summary") == Some(summary)),
            "unexpected failure event for {summary}"
        );
    }
}

fn assert_span_contracts(
    finished: &[SpanData],
    captured: &[observe_test_support::CapturedSpan],
    captured_events: &[observe_test_support::CapturedEvent],
    wire_query_ids: &[String],
    expected_address: &str,
    expected_port: &str,
) {
    assert_eq!(finished.len(), EXPECTED_SPANS.len());
    assert_eq!(captured.len(), EXPECTED_SPANS.len());
    let clickhouse_events: Vec<_> = captured_events
        .iter()
        .filter(|ev| ev.target.starts_with("infra_clickhouse"))
        .cloned()
        .collect();
    assert_eq!(
        clickhouse_events.len(),
        EXPECTED_SPANS
            .iter()
            .filter(|spec| spec.event_level.is_some())
            .count()
    );

    for spec in EXPECTED_SPANS {
        let summary = spec.summary;
        let mut otel_matches = finished
            .iter()
            .filter(|s| otel_attr(s, "db.query.summary").as_deref() == Some(summary));
        let otel = otel_matches
            .next()
            .unwrap_or_else(|| panic!("missing exported OTel span for summary {summary}"));
        assert!(
            otel_matches.next().is_none(),
            "duplicate OTel span for {summary}"
        );

        let mut cap_matches = captured
            .iter()
            .filter(|s| s.unquoted("db.query.summary") == Some(summary));
        let cap = cap_matches
            .next()
            .unwrap_or_else(|| panic!("missing captured tracing span for summary {summary}"));
        assert!(
            cap_matches.next().is_none(),
            "duplicate captured span for {summary}"
        );

        assert_single_span_and_event(
            spec,
            otel,
            cap,
            &clickhouse_events,
            wire_query_ids,
            expected_address,
            expected_port,
        );
    }
}

fn assert_scrape_and_redaction(
    recorder: &metrics_exporter_prometheus::PrometheusRecorder,
    exporter: &InMemorySpanExporter,
    spans: &SpanCapture,
    events: &EventCapture,
    wire_query_ids: &[String],
    expected_address: &str,
    expected_port: &str,
) {
    let finished = exporter.get_finished_spans().expect("all spans");
    let captured_spans = spans.snapshot();
    let captured_events = events.snapshot();
    assert_span_contracts(
        &finished,
        &captured_spans,
        &captured_events,
        wire_query_ids,
        expected_address,
        expected_port,
    );

    let scrape = recorder.handle().render();
    assert_eq!(
        gauge_value(&scrape, "clickhouse_operations_in_flight"),
        Some(0.0)
    );
    assert_eq!(
        count_lines_with(&scrape, "clickhouse_insert_rows_total", &["mode=\"batch\""]),
        2
    );
    for (operation, outcome, expected_count) in [
        ("fetch_one", "ok", 2),
        ("fetch_one", "too_many_rows", 1),
        ("fetch_one", "rejected", 4),
        ("fetch_one", "unavailable", 3),
        ("fetch_one", "timeout", 1),
        ("fetch_one", "cancelled", 1),
        ("insert_batch", "ok", 1),
        ("insert_batch", "cancelled", 1),
        ("insert_batch", "outcome_unknown", 1),
    ] {
        let op_label = format!("operation=\"{operation}\"");
        let outcome_label = format!("outcome=\"{outcome}\"");
        assert_eq!(
            count_lines_with(
                &scrape,
                "clickhouse_operation_duration_seconds_count",
                &[&op_label, &outcome_label]
            ),
            expected_count,
            "duration count mismatch for ({operation}, {outcome}) in:\n{scrape}"
        );
    }
    assert_eq!(
        count_lines_with(
            &scrape,
            "clickhouse_admission_refused_total",
            &["operation=\"fetch_one\""]
        ),
        1
    );
    assert_eq!(
        count_lines_with(
            &scrape,
            "clickhouse_admission_refused_total",
            &["operation=\"insert_batch\""]
        ),
        1
    );

    let dump = format!(
        "{scrape}\n{finished:?}\n{captured_spans:?}\n{:?}",
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

    let ctx = OperationContext::with_timeout(Duration::from_secs(30));
    exercise_read_telemetry(&ch, &ctx, &srv, &recorder, &exporter).await;
    exercise_read_failure_diagnostics(&ch, &ctx, &srv).await;

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

    let expected_address = srv.addr.ip().to_string();
    let expected_port = srv.addr.port().to_string();
    let wire_query_ids = srv.query_ids.lock().await.clone();
    assert_scrape_and_redaction(
        &recorder,
        &exporter,
        &spans,
        &events,
        &wire_query_ids,
        &expected_address,
        &expected_port,
    );

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
