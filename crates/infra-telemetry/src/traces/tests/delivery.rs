//! The exporter against a collector that listens: what arrives there, and
//! what unusable trust material does to the exporter.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;

use futures_util::FutureExt as _;
use opentelemetry::trace::{Span as _, Tracer as _};

use tracing_subscriber::layer::SubscriberExt as _;

use super::*;

// The TLS fixture crates are in the workspace only with gRPC retained.
// template:begin grpc:telemetry-tls-delivery-tests
mod tls;
// template:end grpc:telemetry-tls-delivery-tests

const WAIT: Duration = Duration::from_secs(10);

struct Request {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn body_holds(&self, text: &str) -> bool {
        self.body
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    }
}

/// An OTLP/HTTP receiver that answers every export with an empty success.
/// Its accept thread ends with the test process.
struct Collector {
    addr: SocketAddr,
    requests: mpsc::Receiver<Request>,
}

impl Collector {
    /// `accept` turns a connection into the request it carried, if any.
    fn start(accept: impl Fn(TcpStream) -> Option<Request> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the collector");
        let addr = listener.local_addr().expect("collector address");
        let (sender, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let _ = stream.set_read_timeout(Some(WAIT));
                if let Some(request) = accept(stream)
                    && sender.send(request).is_err()
                {
                    return;
                }
            }
        });
        Self { addr, requests }
    }

    fn received(&self) -> Request {
        self.requests
            .recv_timeout(WAIT)
            .expect("the collector receives an export")
    }
}

/// Read one request and answer it; `None` when the peer never sent one.
fn exchange(mut stream: impl Read + Write) -> Option<Request> {
    let request = read_request(&mut stream)?;
    respond(&mut stream, "200 OK", &[])?;
    Some(request)
}

fn respond(stream: &mut impl Write, status: &str, body: &[u8]) -> Option<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .ok()?;
    stream.write_all(body).ok()?;
    stream.flush().ok()
}

fn read_request(stream: &mut impl Read) -> Option<Request> {
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut read_more = |received: &mut Vec<u8>| {
        let read = stream.read(&mut chunk).ok().filter(|read| *read > 0)?;
        received.extend_from_slice(&chunk[..read]);
        Some(())
    };
    let head_end = loop {
        if let Some(at) = received.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
        read_more(&mut received)?;
    };
    let head = String::from_utf8(received[..head_end].to_vec()).ok()?;
    let mut lines = head.lines();
    let path = lines.next()?.split(' ').nth(1)?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let request = Request {
        path,
        headers,
        body: Vec::new(),
    };
    let length: usize = request.header("content-length")?.parse().ok()?;
    while received.len() < head_end + length {
        read_more(&mut received)?;
    }
    Some(Request {
        body: received[head_end..].to_vec(),
        ..request
    })
}

#[test]
fn a_finished_span_reaches_the_collector_with_the_typed_headers() {
    let collector = Collector::start(exchange);
    // A collector root: the exporter must post to `/v1/traces` under it.
    let mut options = options(&format!("http://{}", collector.addr));
    options.otlp_headers = Some(SecretString::from("x-tenant=t1"));
    let handle = tracer_provider(&options, |_| false, &CollectorTrust::default())
        .expect("the provider builds");
    assert_eq!(
        handle.exporter_state,
        ExporterState::Initialized {
            endpoint_source: EndpointSource::Typed,
            certificate_file: false,
            client_certificate: false,
        }
    );

    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(handle.tracer()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("delivered span").in_scope(|| {});
    });
    assert_eq!(shutdown(handle), ProviderShutdown::Completed);

    let request = collector.received();
    assert_eq!(request.path, "/v1/traces");
    assert_eq!(
        request.header("content-type"),
        Some("application/x-protobuf")
    );
    assert_eq!(request.header("x-tenant"), Some("t1"));
    // Protobuf carries strings as their bytes.
    assert!(request.body_holds("delivered span"), "the span name");
    assert!(request.body_holds("svc"), "the service.name resource");
    assert!(request.body_holds(INSTRUMENTATION_SCOPE), "the scope");
}

#[test]
fn unusable_trust_material_degrades_the_exporter_and_names_the_variable() {
    let files = tempfile::tempdir().expect("directory for trust files");
    let file = |variable, name: &str| TrustFile {
        variable,
        path: files.path().join(name),
    };
    std::fs::write(files.path().join("text.pem"), "not PEM\n").expect("write a file");
    for (trust, expected) in [
        (
            CollectorTrust {
                certificate: Some(file(CERTIFICATE_VARS[0], "absent.pem")),
                ..CollectorTrust::default()
            },
            "read the file OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE names",
        ),
        (
            CollectorTrust {
                certificate: Some(file(CERTIFICATE_VARS[1], "text.pem")),
                ..CollectorTrust::default()
            },
            "the file OTEL_EXPORTER_OTLP_CERTIFICATE names holds no certificate",
        ),
        (
            CollectorTrust {
                client_certificate: Some(file(CLIENT_CERTIFICATE_VARS[1], "text.pem")),
                ..CollectorTrust::default()
            },
            "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE is set without OTEL_EXPORTER_OTLP_CLIENT_KEY",
        ),
        (
            CollectorTrust {
                client_key: Some(file(CLIENT_KEY_VARS[1], "text.pem")),
                ..CollectorTrust::default()
            },
            "OTEL_EXPORTER_OTLP_CLIENT_KEY is set without OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
        ),
        (
            CollectorTrust {
                client_certificate: Some(file(CLIENT_CERTIFICATE_VARS[1], "text.pem")),
                client_key: Some(file(CLIENT_KEY_VARS[1], "text.pem")),
                ..CollectorTrust::default()
            },
            "the client certificate and key files are not a usable PEM identity",
        ),
    ] {
        let error = span_exporter(Some("https://127.0.0.1:1"), HashMap::new(), &trust)
            .expect_err("unusable trust material prevents construction");
        assert!(error.to_string().starts_with(expected));
        let handle = tracer_provider(&options("https://127.0.0.1:1"), |_| false, &trust)
            .expect("unusable trust material is not a startup failure");
        match handle.exporter_state {
            ExporterState::Degraded { reason } => {
                assert_eq!(reason, "exporter_build");
            }
            state => panic!("{expected}: {state:?}"),
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
}

fn shutdown(handle: TracerProviderHandle) -> ProviderShutdown {
    runtime().block_on(handle.shutdown(tokio::time::Instant::now() + WAIT))
}

/// Each request is observed before the test permits its scripted response.
/// The finite script's thread is joined after the last response.
fn scripted_collector(
    responses: Vec<(&'static str, Vec<u8>)>,
) -> (Collector, mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted collector");
    let addr = listener.local_addr().unwrap();
    let (sender, requests) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("export connection");
            stream.set_read_timeout(Some(WAIT)).unwrap();
            stream.set_write_timeout(Some(WAIT)).unwrap();
            sender
                .send(read_request(&mut stream).expect("complete export"))
                .unwrap();
            proceed.recv_timeout(WAIT).expect("release response");
            respond(&mut stream, status, &body).expect("send scripted response");
        }
    });
    (Collector { addr, requests }, release, thread)
}

/// Configure only batch size and timer for deterministic test scheduling;
/// the real SDK batch processor and exporter wrapper own all completion.
fn batch_handle(exporter: impl SpanExporter + 'static) -> TracerProviderHandle {
    let drain = Arc::default();
    let processor = sdktrace::BatchSpanProcessor::builder(Counted {
        inner: exporter,
        drain: Arc::clone(&drain),
    })
    .with_batch_config(
        sdktrace::BatchConfigBuilder::default()
            .with_max_export_batch_size(1)
            .with_scheduled_delay(WAIT * 10)
            .build(),
    )
    .build();
    TracerProviderHandle {
        provider: SdkTracerProvider::builder()
            .with_span_processor(processor)
            .build(),
        exporter_state: ExporterState::Disabled,
        drain,
    }
}

#[test]
fn a_receiver_failure_only_poisons_the_drain_if_it_completes_after_shutdown_begins() {
    for during_drain in [false, true] {
        let (collector, release, thread) = scripted_collector(vec![
            ("400 Bad Request", b"collector_failure_secret".to_vec()),
            ("200 OK", Vec::new()),
        ]);
        let exporter = span_exporter(
            Some(&format!("http://{}/v1/traces", collector.addr)),
            HashMap::new(),
            &CollectorTrust::default(),
        )
        .unwrap();
        let handle = batch_handle(exporter);
        handle.tracer().start("first batch").end();
        assert!(collector.received().body_holds("first batch"));
        if !during_drain {
            release.send(()).unwrap();
            // FIFO control completion proves the earlier exporter call finished.
            handle
                .provider
                .force_flush()
                .expect("historical export finished");
        }
        handle.tracer().start("second batch").end();
        let outcome = runtime().block_on(async {
            let shutdown = handle.shutdown(tokio::time::Instant::now() + WAIT);
            tokio::pin!(shutdown);
            assert!(futures_util::poll!(&mut shutdown).is_pending());
            if during_drain {
                release.send(()).unwrap();
            }
            assert!(collector.received().body_holds("second batch"));
            release.send(()).unwrap();
            shutdown.await
        });
        thread.join().expect("scripted collector stops");
        if during_drain {
            let ProviderShutdown::Incomplete(reasons) = outcome else {
                panic!("later success hid the in-flight failure: {outcome:?}");
            };
            assert_ne!(
                reasons.bits() & ProviderShutdownReasons::INTERNAL_FAILURE,
                0
            );
            assert!(!format!("{outcome:?}").contains("collector_failure_secret"));
        } else {
            assert_eq!(outcome, ProviderShutdown::Completed);
        }
    }
}

#[derive(Debug)]
struct ShutdownFailure;

impl SpanExporter for ShutdownFailure {
    fn export(&self, _batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        std::future::ready(Ok(()))
    }

    fn shutdown_with_timeout(&self, _timeout: Duration) -> OTelSdkResult {
        Err(OTelSdkError::InternalFailure(
            "shutdown_error_secret".into(),
        ))
    }
}

#[test]
fn an_inner_shutdown_error_survives_the_sdk_outer_success() {
    let outcome = shutdown(batch_handle(ShutdownFailure));
    let ProviderShutdown::Incomplete(reasons) = outcome else {
        panic!("inner shutdown failure was hidden: {outcome:?}");
    };
    assert_eq!(
        reasons.bits(),
        ProviderShutdownReasons::EXPORTER_SHUTDOWN | ProviderShutdownReasons::INTERNAL_FAILURE
    );
    assert!(!format!("{outcome:?}").contains("shutdown_error_secret"));
}

#[test]
fn an_exhausted_allowance_returns_before_the_receiver_is_released() {
    for allowance in [Duration::ZERO, SHUTDOWN_JOIN_SLACK] {
        let (collector, release, thread) = scripted_collector(vec![("200 OK", Vec::new())]);
        let exporter = span_exporter(
            Some(&format!("http://{}/v1/traces", collector.addr)),
            HashMap::new(),
            &CollectorTrust::default(),
        )
        .unwrap();
        let handle = batch_handle(exporter);
        handle.tracer().start("blocked export").end();
        collector.received();
        let rt = runtime();
        let outcome = rt.block_on(async {
            let shutdown = handle.shutdown(tokio::time::Instant::now() + allowance);
            tokio::pin!(shutdown);
            let std::task::Poll::Ready(outcome) = futures_util::poll!(&mut shutdown) else {
                panic!("no SDK allowance remains after deducting the join slack");
            };
            outcome
        });
        let ProviderShutdown::Incomplete(reasons) = outcome else {
            panic!("exhausted allowance cannot complete: {outcome:?}");
        };
        assert_ne!(reasons.bits() & ProviderShutdownReasons::DEADLINE, 0);
        // The receiver is still stopped when the caller's bounded return is asserted.
        release.send(()).unwrap();
        thread.join().expect("collector stops after release");
        rt.shutdown_timeout(WAIT);
    }
}

#[derive(Clone, Debug, Default)]
struct CapturedSpans(Arc<Mutex<Vec<SpanData>>>);

impl SpanExporter for CapturedSpans {
    fn export(&self, batch: Vec<SpanData>) -> impl Future<Output = OTelSdkResult> + Send {
        self.0.lock().unwrap().extend(batch);
        std::future::ready(Ok(()))
    }
}

/// Encode one rejected span and the collector's message in an OTLP response.
fn partial_success_response(message: &str) -> Vec<u8> {
    // ExportTraceServiceResponse.partial_success (field 1) contains
    // rejected_spans=1 (field 1), error_message=message (field 2).
    let mut response = vec![
        0x0a,
        u8::try_from(message.len() + 4).unwrap(),
        0x08,
        1,
        0x12,
        u8::try_from(message.len()).unwrap(),
    ];
    response.extend_from_slice(message.as_bytes());
    response
}

#[test]
fn collector_diagnostics_cannot_escape_to_local_records_or_span_events() {
    use crate::logging::{LoggingFormat, tests::capture};

    const MESSAGE: &str = "collector_message_secret";
    const MALFORMED: &[u8] = b"malformed_response_secret";
    const STATUS_BODY: &[u8] = b"status_body_secret";
    const URL_SECRET: &str = "collector_url_secret";
    const HEADER_SECRET: &str = "collector_header_secret";
    let partial = partial_success_response(MESSAGE);
    for format in [LoggingFormat::Json, LoggingFormat::Text] {
        for level in ["debug", "trace"] {
            for (status, body, success) in [
                ("200 OK", partial.clone(), true),
                ("200 OK", MALFORMED.to_vec(), true),
                ("400 Bad Request", STATUS_BODY.to_vec(), false),
            ] {
                let (collector, release, thread) = scripted_collector(vec![(status, body)]);
                let spans = CapturedSpans::default();
                let handle = TracerProviderHandle {
                    provider: SdkTracerProvider::builder()
                        .with_simple_exporter(spans.clone())
                        .build(),
                    exporter_state: ExporterState::Disabled,
                    drain: Arc::default(),
                };
                let (dispatch, logger, records) = capture(format, level, Some(&handle));
                // Queue the response permission before the blocking export; the
                // collector still reads the real request before responding.
                release.send(()).unwrap();
                tracing::dispatcher::with_default(&dispatch, || {
                    tracing::info_span!("ordinary_span").in_scope(|| {
                        tracing::info!("ordinary_event");
                        let exporter = Counted {
                            inner: span_exporter(
                                Some(&format!(
                                    "http://{}/v1/traces?token={URL_SECRET}",
                                    collector.addr
                                )),
                                HashMap::from([(
                                    "authorization".to_owned(),
                                    HEADER_SECRET.to_owned(),
                                )]),
                                &CollectorTrust::default(),
                            )
                            .unwrap(),
                            drain: Arc::default(),
                        };
                        let result = exporter
                            .export(Vec::new())
                            .now_or_never()
                            .expect("blocking HTTP exporter finishes in one poll");
                        assert_eq!(result.is_ok(), success, "SDK result remains authoritative");
                    });
                });
                let request = collector.received();
                assert!(request.path.contains(URL_SECRET));
                assert_eq!(request.header("authorization"), Some(HEADER_SECRET));
                thread.join().expect("collector script finishes");
                assert_eq!(shutdown(handle), ProviderShutdown::Completed);
                assert!(matches!(
                    logger.shutdown(std::time::Instant::now() + WAIT),
                    crate::logging::LoggerShutdown::Completed(_)
                ));
                let records = records.records();
                assert!(
                    records.contains("ordinary_event"),
                    "positive local control: {records}"
                );
                let spans = spans.0.lock().unwrap();
                assert_eq!(spans.len(), 1, "SDK diagnostics cannot create extra spans");
                assert_eq!(spans[0].name, "ordinary_span");
                assert_eq!(
                    spans[0].events.len(),
                    1,
                    "only the application event is admitted"
                );
                assert_eq!(spans[0].events[0].name, "ordinary_event");
                let exported = format!("{spans:?}");
                for secret in [
                    MESSAGE,
                    std::str::from_utf8(MALFORMED).unwrap(),
                    std::str::from_utf8(STATUS_BODY).unwrap(),
                    URL_SECRET,
                    HEADER_SECRET,
                ] {
                    assert!(
                        !records.contains(secret),
                        "local diagnostic leaked {secret}"
                    );
                    assert!(
                        !exported.contains(secret),
                        "span diagnostic leaked {secret}"
                    );
                }
                for event in [
                    "HttpTraceClient.PartialSuccess",
                    "HttpTraceClient.ResponseParseError",
                    "HttpClient.StatusError",
                ] {
                    assert!(!records.contains(event), "raw SDK event escaped: {records}");
                }
            }
        }
    }
}
