#![allow(
    clippy::expect_used,
    reason = "test-only JSON fixtures fail closed with precise local setup context"
)]

use super::*;
use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use std::io;
use std::sync::{Arc, Mutex};
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

#[derive(Clone, Default)]
pub(crate) struct Buffer(Arc<Mutex<Vec<u8>>>);

#[test]
fn invalid_filter_is_reported_without_its_value() {
    let options = LoggingOptions {
        level: "app=private-marker=invalid",
        format: LoggingFormat::Json,
        tracer_provider: None,
    };
    let Err(error) = install_subscriber(&options) else {
        panic!("expected malformed filter to be refused");
    };
    assert!(matches!(error, LoggingError::Directive));
    assert!(error.to_string().contains("log.level"));
    assert!(!format!("{options:?} {error} {error:?}").contains("private-marker"));
}

impl Buffer {
    pub(crate) fn records(&self) -> String {
        String::from_utf8(self.0.lock().expect("test writer mutex").clone())
            .expect("json subscriber only writes UTF-8")
    }
}

impl io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut buffer = self
            .0
            .lock()
            .map_err(|_| io::Error::other("test writer mutex"))?;
        io::Write::write(&mut *buffer, bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    fn layer(
        &self,
        format: LoggingFormat,
        trace_context: bool,
    ) -> (format::FormatLayer, LoggerGuard) {
        let (output, guard) = output::start(self.clone()).expect("start the test writer");
        (
            format::FormatLayer::new(output, format, trace_context),
            guard,
        )
    }
}

pub(crate) fn capture(
    format: LoggingFormat,
    level: &str,
    provider: Option<&TracerProviderHandle>,
) -> (tracing::Dispatch, LoggerGuard, Buffer) {
    let buffer = Buffer::default();
    let filter = admission_filter(level).expect("valid capture filter");
    let otel = provider.map(|handle| tracing_opentelemetry::layer().with_tracer(handle.tracer()));
    let (layer, guard) = buffer.layer(format, otel.is_some());
    let registry = Registry::default().with(filter);
    (
        tracing::Dispatch::new(registry.with(otel).with(layer)),
        guard,
        buffer,
    )
}

fn drain(guard: LoggerGuard) {
    assert_eq!(
        guard.shutdown(std::time::Instant::now() + std::time::Duration::from_secs(2)),
        LoggerShutdown::Completed
    );
}

#[derive(Clone, Copy)]
enum EventParent {
    Current,
    Explicit,
}

#[derive(Clone, Copy)]
enum Sampling {
    Sampled,
    NotSampled,
}

fn emit_correlated_event(
    sampling: Sampling,
    event_parent: EventParent,
) -> (String, String, String, bool) {
    let buffer = Buffer::default();
    let (layer, guard) = buffer.layer(LoggingFormat::Json, true);
    let provider = SdkTracerProvider::builder()
        .with_sampler(match sampling {
            Sampling::Sampled => Sampler::AlwaysOn,
            Sampling::NotSampled => Sampler::AlwaysOff,
        })
        .build();
    let subscriber = Registry::default()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")))
        .with(layer);
    let dispatch = tracing::Dispatch::new(subscriber);
    assert!(
        !dispatch
            .downcast_ref::<format::FormatLayer>()
            .expect("format layer present")
            .has_pinned_global_dispatch()
    );

    let (trace_id, span_id, sampled) = tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!("correlation_parent");
        let context = span.context();
        let otel_span = context.span();
        let span_context = otel_span.span_context();
        let expected = (
            span_context.trace_id().to_string(),
            span_context.span_id().to_string(),
            span_context.is_sampled(),
        );

        match event_parent {
            EventParent::Current => span.in_scope(|| tracing::info!("correlation probe")),
            EventParent::Explicit => tracing::info!(parent: &span, "correlation probe"),
        }

        expected
    });

    drain(guard);
    (buffer.records(), trace_id, span_id, sampled)
}

fn emit_event_without_otel_context(with_span: bool) -> String {
    let buffer = Buffer::default();
    let (layer, guard) = buffer.layer(LoggingFormat::Json, false);
    let dispatch = tracing::Dispatch::new(Registry::default().with(layer));

    tracing::dispatcher::with_default(&dispatch, || {
        if with_span {
            tracing::info_span!("plain_parent").in_scope(|| tracing::info!("plain probe"));
        } else {
            tracing::info!("plain probe");
        }
    });

    drain(guard);
    buffer.records()
}

#[test]
fn json_logs_keep_otel_correlation_for_supported_event_parents_and_sampling() {
    for (name, sampling, event_parent, expected_sampled) in [
        (
            "active sampled parent",
            Sampling::Sampled,
            EventParent::Current,
            true,
        ),
        (
            "explicit sampled parent",
            Sampling::Sampled,
            EventParent::Explicit,
            true,
        ),
        (
            "active non-sampled parent",
            Sampling::NotSampled,
            EventParent::Current,
            false,
        ),
    ] {
        let (record, trace_id, span_id, sampled) = emit_correlated_event(sampling, event_parent);
        assert_eq!(sampled, expected_sampled, "{name}");
        assert!(
            record.contains(&format!(
                r#""trace_id":"{trace_id}","span_id":"{span_id}","trace_flags":"{flags}""#,
                flags = if sampled { "01" } else { "00" }
            )),
            "{name} must carry the trace context: {record}"
        );
    }
}

#[test]
fn json_logs_omit_otel_correlation_without_a_span_or_provider() {
    for (name, with_span) in [("no span", false), ("no provider", true)] {
        let record = emit_event_without_otel_context(with_span);
        assert!(
            !record.contains("\"trace_id\"") && !record.contains("\"span_id\""),
            "{name} must not emit empty or invalid trace ids: {record}"
        );
    }
}

#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "global subscriber pinning test isolates its global default in an owned child process"
)]
fn global_subscriber_pins_dispatch_and_correlates_otel_context() {
    const CHILD: &str = "TELEMETRY_GLOBAL_PIN_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "logging::tests::global_subscriber_pins_dispatch_and_correlates_otel_context",
                "--no-capture",
                "--quiet",
            ])
            .env(CHILD, "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn isolated global subscriber child");
        let output = super::wait_for_child(child, std::time::Duration::from_secs(20));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stdout={stdout} stderr={stderr}");
        let records: Vec<serde_json::Value> = stdout
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assert_eq!(records.len(), 2, "stdout={stdout}");
        for (record, expected_message) in records.iter().zip(["current_parent", "explicit_parent"])
        {
            assert_eq!(record["message"], expected_message, "{record}");
            assert_eq!(record["trace_id"], record["expected_trace"], "{record}");
            assert_eq!(record["span_id"], record["expected_span"], "{record}");
            assert_eq!(record["trace_flags"], "01", "{record}");
        }
        return;
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child runtime");
    let provider = runtime
        .block_on(crate::install_tracer_provider(&crate::TracingOptions {
            service_name: "svc",
            service_version: "1.0.0",
            vcs_revision: "abc",
            instance_id: Some("instance-1"),
            deployment_environment: "test",
            sampler: crate::ResolvedSampler::AlwaysOn,
            otlp_endpoint: None,
            otlp_headers: None,
        }))
        .expect("tracer provider installs");
    let options = LoggingOptions {
        level: "info",
        format: LoggingFormat::Json,
        tracer_provider: Some(&provider),
    };
    let guard = install_subscriber(&options).expect("global subscriber installs");
    assert!(tracing::dispatcher::get_default(|dispatch| {
        dispatch
            .downcast_ref::<format::FormatLayer>()
            .is_some_and(format::FormatLayer::has_pinned_global_dispatch)
    }));
    std::assert_matches!(
        install_subscriber(&options),
        Err(LoggingError::AlreadyInstalled)
    );

    let span = tracing::info_span!("global_parent");
    let context = span.context();
    let otel_span = context.span();
    let span_context = otel_span.span_context();
    assert!(span_context.is_valid() && span_context.is_sampled());
    let expected_trace = span_context.trace_id().to_string();
    let expected_span = span_context.span_id().to_string();

    span.in_scope(|| {
        tracing::info!(
            expected_trace = expected_trace.as_str(),
            expected_span = expected_span.as_str(),
            "current_parent"
        );
    });
    std::thread::spawn(move || {
        tracing::info!(
            parent: &span,
            expected_trace = expected_trace.as_str(),
            expected_span = expected_span.as_str(),
            "explicit_parent"
        );
    })
    .join()
    .expect("explicit-parent thread completes");

    drain(guard);
    let _ = runtime.block_on(
        provider.shutdown(tokio::time::Instant::now() + std::time::Duration::from_secs(2)),
    );
}

#[test]
fn json_line_sorts_event_fields_then_merged_span_fields() {
    let buffer = Buffer::default();
    let (layer, guard) = buffer.layer(LoggingFormat::Json, false);
    let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
    tracing::dispatcher::with_default(&dispatch, || {
        let outer = tracing::info_span!(
            "outer",
            request_id = "r-1",
            shared = "outer",
            name = "overwritten by the span name",
            later = tracing::field::Empty,
            log.target = ?"dropped like a log bridge field",
            r#kind = ?"k",
        );
        let _outer = outer.enter();
        outer.record("later", 7_u64);
        tracing::info_span!("inner", shared = "inner").in_scope(|| {
            tracing::info!(
                target: "a\"b\n",
                zeta = 1,
                alpha = true,
                ratio = f64::NAN,
                text = "q\"\n",
                "done"
            );
        });
    });
    drain(guard);
    let record = buffer.records();
    let (head, rest) = record
        .split_once(r#""timestamp":""#)
        .expect("the line has a timestamp");
    let (timestamp, tail) = rest.split_once('"').expect("the timestamp is quoted");
    assert_eq!(
        head, r#"{"level":"INFO","target":"a\"b\n","#,
        "level and target come first, the target escaped like any string: {record}"
    );
    assert_eq!(
        tail,
        concat!(
            r##","alpha":true,"message":"done","ratio":null,"text":"q\"\n","zeta":1,"##,
            r##""kind":"\"k\"","later":7,"name":"inner","request_id":"r-1","shared":"inner"}"##,
            "\n"
        ),
        "event fields, then span fields with the nearest value of a repeated key: {record}"
    );
    let shape: String = timestamp
        .chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect();
    assert_eq!(shape, "0000-00-00T00:00:00.000000Z", "{timestamp}");
}

#[test]
fn json_line_writes_each_key_once() {
    let buffer = Buffer::default();
    let (layer, guard) = buffer.layer(LoggingFormat::Json, false);
    let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
    tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!(
            "HTTP request",
            request_id = "from-span",
            route = "/items",
            level = "span field named like the line's own key",
        );
        span.in_scope(|| {
            tracing::info!(
                request_id = "from-event",
                name = "from-event",
                timestamp = 1,
                trace_id = "not a trace id",
                "http_request"
            );
        });
    });
    drain(guard);
    let record = buffer.records();
    let line: serde_json::Value = serde_json::from_str(&record).expect("one JSON object");
    assert_eq!(line["request_id"], "from-event", "{record}");
    assert_eq!(line["name"], "from-event", "{record}");
    assert_eq!(line["route"], "/items", "{record}");
    assert_eq!(line["level"], "INFO", "{record}");
    for key in ["level", "timestamp", "trace_id", "request_id", "name"] {
        assert_eq!(
            record.matches(&format!("\"{key}\":")).count(),
            usize::from(key != "trace_id"),
            "{key}: {record}"
        );
    }
}

#[test]
fn json_line_names_the_real_target_of_a_log_crate_record() {
    let buffer = Buffer::default();
    let (layer, guard) = buffer.layer(LoggingFormat::Json, false);
    let dispatch = tracing::Dispatch::new(Registry::default().with(layer));
    tracing::dispatcher::with_default(&dispatch, || {
        tracing_log::format_trace(
            &tracing_log::log::Record::builder()
                .level(tracing_log::log::Level::Warn)
                .target("rustls::client")
                .module_path(Some("rustls::client::hs"))
                .file(Some("hs.rs"))
                .line(Some(7))
                .args(format_args!("bridged"))
                .build(),
        )
        .expect("the record is dispatched");
    });
    drain(guard);
    let record = buffer.records();
    assert!(
        record.starts_with(r#"{"level":"WARN","target":"rustls::client","timestamp":""#),
        "{record}"
    );
    assert!(
        record.ends_with("Z\",\"message\":\"bridged\"}\n"),
        "{record}"
    );
}

#[test]
fn info_spans_survive_a_quieter_directive() {
    for directive in ["warn", "off", "warn,[never]=trace"] {
        let buffer = Buffer::default();
        let (layer, guard) = buffer.layer(LoggingFormat::Json, false);
        let filter = admission_filter(directive).expect("valid directive");
        let dispatch = tracing::Dispatch::new(Registry::default().with(filter).with(layer));
        tracing::dispatcher::with_default(&dispatch, || {
            let request = tracing::info_span!("request", request_id = "r-1");
            assert!(!request.is_disabled(), "{directive}");
            assert!(tracing::debug_span!("detail").is_disabled(), "{directive}");
            request.in_scope(|| {
                tracing::info!("quiet");
                tracing::error!("loud");
            });
        });
        drain(guard);
        let record = buffer.records();
        assert!(!record.contains("quiet"), "{directive}: {record}");
        assert_eq!(
            record.contains(r#""message":"loud","name":"request","request_id":"r-1"}"#),
            directive != "off",
            "{directive}: {record}"
        );
    }
}

#[test]
fn a_verbose_directive_still_enables_verbose_spans() {
    let filter = admission_filter("info,app=debug").expect("valid directive");
    let dispatch = tracing::Dispatch::new(Registry::default().with(filter));
    tracing::dispatcher::with_default(&dispatch, || {
        assert!(!tracing::debug_span!(target: "app", "detail").is_disabled());
        assert!(tracing::debug_span!(target: "other", "detail").is_disabled());
    });
}

#[test]
fn only_directives_without_span_filters_or_empty_parts_become_targets() {
    for directive in [
        "info",
        "info,hyper=warn",
        "warn,app=debug",
        "off",
        "app",
        "3",
    ] {
        assert!(static_targets(directive).is_some(), "{directive}");
    }
    for directive in [
        "info,",
        ",",
        "info,,hyper=warn",
        ",info",
        "info,[request]=debug",
        "app=",
        "info,app=",
    ] {
        assert!(static_targets(directive).is_none(), "{directive}");
    }
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "cumulative SDK diagnostic test runs in an isolated owned child process"
)]
fn in_diagnostic_child() -> bool {
    const CHILD: &str = "TELEMETRY_DIAGNOSTIC_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "logging::tests::sdk_diagnostics_keep_numeric_facts_without_raw_output",
                "--no-capture",
            ])
            .env(CHILD, "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run the isolated diagnostic observation");
        let result = super::wait_for_child(child, std::time::Duration::from_secs(20));
        assert!(
            result.status.success(),
            "child output: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return false;
    }
    true
}

struct NeverFormat;
impl std::fmt::Debug for NeverFormat {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        panic!("denied diagnostic Debug was invoked")
    }
}

fn emit_denied_diagnostics() {
    tracing::error!("ordinary_record");
    tracing::warn!(target: "infra_messaging::consumer::delivery", "messaging_delivery_failed");
    tracing::debug!(target: "async_nats", "received MESSAGE: sid=1, subject=events.PRIVATE_NATS");
    tracing::trace!(target: "async_nats::connection", "server op: events.PRIVATE_NATS");
    tracing::debug!(name: "HttpTraceClient.ResponseParseError", target: "opentelemetry-otlp", error = ?NeverFormat);
    tracing::trace!(target: "reqwest::connect", "SDK_SECRET_SENTINEL");
    tracing::warn!(target: "opentelemetry_sdk", "SDK_SECRET_SENTINEL");
    assert!(
        tracing::info_span!(target: "tracing_opentelemetry", "SDK_SECRET_SENTINEL").is_disabled()
    );
    for target in [
        "rustls::client",
        "hyper_util::client",
        "opentelemetry-http",
        "h2::codec",
        "rustls_platform_verifier",
    ] {
        tracing_log::format_trace(
            &tracing_log::log::Record::builder()
                .target(target)
                .level(tracing_log::log::Level::Error)
                .args(format_args!("SDK_SECRET_SENTINEL"))
                .build(),
        )
        .expect("bridge diagnostic");
    }
}

#[test]
fn sdk_diagnostics_keep_numeric_facts_without_raw_output() {
    use metrics::Key;
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    if !in_diagnostic_child() {
        return;
    }
    for format in [LoggingFormat::Json, LoggingFormat::Text] {
        for level in ["debug", "trace", "off", "off,[request]=trace"] {
            let (dispatch, guard, buffer) = capture(format, level, None);
            tracing::dispatcher::with_default(&dispatch, || {
                let span = tracing::info_span!("request", request_id = "safe-request");
                assert!(!span.is_disabled());
                span.in_scope(emit_denied_diagnostics);
            });
            drain(guard);
            let records = buffer.records();
            assert!(!records.contains("SDK_SECRET_SENTINEL"), "{records}");
            assert!(!records.contains("ResponseParseError"), "{records}");
            assert!(!records.contains("PRIVATE_NATS"), "{records}");
            if level != "off" && !level.starts_with("off,") {
                assert!(
                    records.contains("ordinary_record")
                        && records.contains("safe-request")
                        && records.contains("messaging_delivery_failed"),
                    "{records}"
                );
            }
        }
    }

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        let (dispatch, guard, _) = capture(LoggingFormat::Json, "off", None);
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::warn!(name: "HttpTraceClient.PartialSuccess", target: "opentelemetry-otlp", error_message = "SDK_SECRET_SENTINEL");
            tracing::warn!(name: "BatchSpanProcessor.SpansDropped", target: "opentelemetry_sdk", dropped_span_count = ?NeverFormat);
        });
        publish_observations();
        let absent = snapshotter.snapshot().into_vec();
        assert!(
            absent.iter().all(
                |(composite, ..)| composite.key().name() != "telemetry_sdk_queue_dropped_spans"
            )
        );
        assert!(absent.iter().all(|(composite, ..)| {
            composite.key().name() != "telemetry_sdk_reported_rejected_spans_total"
        }));
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::debug!(name: "BatchSpanProcessor.SpanDroppingStarted", target: "opentelemetry_sdk", message = "SDK_SECRET_SENTINEL");
            for _ in 0..2 {
                tracing::warn!(name: "BatchSpanProcessor.SpansDropped", target: "opentelemetry_sdk", dropped_span_count = 19_u64);
            }
            tracing::warn!(name: "HttpTraceClient.PartialSuccess", target: "opentelemetry-otlp", rejected_spans = 3_i64, error_message = "SDK_SECRET_SENTINEL");
            tracing::warn!(name: "HttpTraceClient.PartialSuccess", target: "opentelemetry-otlp", rejected_spans = -1_i64);
            tracing::debug!(name: "HttpClient.StatusError", target: "opentelemetry-otlp", status_code = 400_u64, url = ?NeverFormat);
        });
        publish_observations();
        publish_observations();
        let observed = snapshotter.snapshot().into_vec();
        for (target, expected) in [
            (
                Key::from_name("telemetry_sdk_queue_dropped_spans"),
                DebugValue::Gauge(19.0.into()),
            ),
            (
                Key::from_name("telemetry_sdk_reported_rejected_spans_total"),
                DebugValue::Counter(3),
            ),
            (
                Key::from_parts(
                    "telemetry_sdk_diagnostics_total",
                    &[("event", "response_parse_error")],
                ),
                DebugValue::Counter(8),
            ),
            (
                Key::from_parts(
                    "telemetry_sdk_diagnostics_total",
                    &[("event", "queue_spans_dropped")],
                ),
                DebugValue::Counter(3),
            ),
        ] {
            let actual = observed
                .iter()
                .find_map(|(composite, _, _, value)| (composite.key() == &target).then_some(value));
            assert_eq!(actual, Some(&expected), "{target}: {observed:?}");
        }
        assert!(!format!("{observed:?}").contains("SDK_SECRET_SENTINEL"));
        drain(guard);
    });
}

// template:begin object-storage:telemetry-sdk-log-cap-test
#[test]
fn sdk_debug_records_stay_out_unless_the_directive_names_them() {
    let emit = |level: &str| {
        let buffer = Buffer::default();
        let (layer, guard) = buffer.layer(LoggingFormat::Text, false);
        let filter = EnvFilter::try_new(level).expect("valid directive");
        let dispatch = tracing::Dispatch::new(
            Registry::default()
                .with(filter)
                .with(aws_sdk_log_cap(level))
                .with(layer),
        );
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::debug!(target: "aws_smithy_runtime::client::orchestrator::endpoints", "sdk debug with key");
            tracing::info!(target: "aws_smithy_runtime::client", "sdk info");
            tracing::debug!(target: "service::feature", "service debug");
        });
        drain(guard);
        buffer.records()
    };
    let capped = emit("debug");
    assert!(!capped.contains("sdk debug with key"), "{capped}");
    assert!(capped.contains("sdk info"), "{capped}");
    assert!(capped.contains("service debug"), "{capped}");
    let named = emit("debug,aws_smithy_runtime=debug");
    assert!(named.contains("sdk debug with key"), "{named}");
    let sibling = emit("debug,aws_config=warn");
    assert!(!sibling.contains("sdk debug with key"), "{sibling}");
    let quiet = emit("warn");
    assert!(!quiet.contains("sdk info"), "{quiet}");
}
// template:end object-storage:telemetry-sdk-log-cap-test

#[test]
fn temporal_sdk_debug_and_trace_records_stay_capped_unless_explicitly_named() {
    let emit = |level: &str| {
        let buffer = Buffer::default();
        let (layer, guard) = buffer.layer(LoggingFormat::Text, false);
        let filter = admission_filter(level).expect("valid directive");
        let dispatch = tracing::Dispatch::new(
            Registry::default()
                .with(filter)
                .with(temporal_sdk_log_cap(level))
                .with(layer),
        );
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::debug!(target: "temporalio_sdk_core::worker::workflow", "core debug");
            tracing::trace!(target: "temporalio_sdk::worker", "sdk trace");
            tracing::debug!(target: "temporal_client", "client root debug");
            tracing::debug!(target: "temporal_client::retry", "client submodule debug");
            tracing::info!(target: "temporalio_sdk_core::worker", "core info");
            tracing::info!(target: "temporal_client", "client info");
            tracing::debug!(target: "temporal_client_app::handler", "unrelated prefix debug");
            tracing::debug!(target: "infra_temporal::client", "adapter debug");
        });
        drain(guard);
        buffer.records()
    };

    let capped = emit("trace");
    assert!(!capped.contains("core debug"), "{capped}");
    assert!(!capped.contains("sdk trace"), "{capped}");
    assert!(!capped.contains("client root debug"), "{capped}");
    assert!(!capped.contains("client submodule debug"), "{capped}");
    assert!(capped.contains("core info"), "{capped}");
    assert!(capped.contains("client info"), "{capped}");
    assert!(capped.contains("unrelated prefix debug"), "{capped}");
    assert!(capped.contains("adapter debug"), "{capped}");

    let named_core = emit("debug,temporalio_sdk_core=debug");
    assert!(named_core.contains("core debug"), "{named_core}");
    assert!(!named_core.contains("client root debug"), "{named_core}");
    assert!(
        !named_core.contains("client submodule debug"),
        "{named_core}"
    );

    let named_client = emit("debug,temporal_client=debug");
    assert!(!named_client.contains("core debug"), "{named_client}");
    assert!(named_client.contains("client root debug"), "{named_client}");
    assert!(
        named_client.contains("client submodule debug"),
        "{named_client}"
    );

    let sibling = emit("debug,temporalio_client=warn");
    assert!(!sibling.contains("core debug"), "{sibling}");
    assert!(!sibling.contains("client root debug"), "{sibling}");

    let stem_only = emit("debug,temporalio_=debug");
    assert!(!stem_only.contains("core debug"), "{stem_only}");

    let quiet = emit("warn");
    assert!(!quiet.contains("core info"), "{quiet}");
    assert!(!quiet.contains("client info"), "{quiet}");
}
