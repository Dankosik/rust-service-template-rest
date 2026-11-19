use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use infra_http_transport::{ConnectionLimits, Drained, Http1Options, Server};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing_subscriber::layer::SubscriberExt as _;

fn loopback() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn options() -> Http1Options {
    Http1Options {
        header_read_timeout: Duration::from_millis(300),
        max_read_buffer_bytes: 8 * 1024,
        connections: ConnectionLimits {
            max_connections: NonZeroU32::new(4),
            max_connection_age: None,
        },
    }
}

fn harden(app: Router) -> Router {
    crate::harden::harden(
        app,
        &crate::HardenOptions {
            max_body_bytes: 64,
            request_timeout: Duration::from_secs(30),
            max_in_flight: None,
            admission_wait: Duration::from_secs(30),
            log_health_probes: false,
            trust_trace_context: false,
            draining: None,
        },
    )
}

fn capture_request_spans() -> (
    tracing::subscriber::DefaultGuard,
    SdkTracerProvider,
    InMemorySpanExporter,
) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    (
        tracing::subscriber::set_default(subscriber),
        provider,
        exporter,
    )
}

#[tokio::test]
async fn drain_budget_expiry_forces_an_in_flight_connection() {
    let (_subscriber, _provider, exporter) = capture_request_spans();
    let entered = Arc::new(tokio::sync::Notify::new());
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixture registers a raw route"
    )]
    let routes = Router::new().route(
        "/pending",
        get({
            let entered = entered.clone();
            move || {
                entered.notify_one();
                std::future::pending::<&'static str>()
            }
        }),
    );
    let server = Server::bind(loopback(), harden(routes), options()).unwrap();
    let mut peer = TcpStream::connect(server.local_addr()).await.unwrap();
    peer.write_all(b"GET /pending HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let drained = server.drain(Duration::from_millis(50)).await.unwrap();
    std::assert_matches!(
        drained,
        Drained::TimedOut {
            remaining_at_deadline: 1,
            ..
        }
    );
    let mut body = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(2), peer.read_to_end(&mut body)).await;
    std::assert_matches!(closed, Ok(Ok(0) | Err(_)));
    assert_eq!(body.as_slice(), b"");
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans
        .iter()
        .find(|span| span.name == "GET /pending")
        .expect("the interrupted request exports a span");
    assert!(
        !span
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "http.response.status_code")
    );
    assert!(span.attributes.contains(&opentelemetry::KeyValue::new(
        "error.type",
        "server_shutdown"
    )));
    std::assert_matches!(span.status, opentelemetry::trace::Status::Error { .. });
    let access = span
        .events
        .iter()
        .find(|event| event.name == "http_request")
        .expect("the interrupted request has an access event");
    assert!(
        access
            .attributes
            .contains(&opentelemetry::KeyValue::new("status", "503"))
    );
    assert!(access.attributes.contains(&opentelemetry::KeyValue::new(
        "termination",
        "server_shutdown"
    )));
}

// template:begin grpc:http-server-span-tls-test
use infra_http_transport::{BoundListener, TlsOptions};
use rustls::pki_types::ServerName;
use tls_test_support::localhost_tls;
use tokio_rustls::TlsConnector;

#[tokio::test]
async fn tls_http1_stream_completes_at_content_length_and_reports_https() {
    use axum::body::Body;
    use axum::http::header::CONTENT_LENGTH;
    use futures_util::stream;

    let (_subscriber, _provider, exporter) = capture_request_spans();
    let (server_config, client_config) = localhost_tls(b"http/1.1");
    let opts = Http1Options {
        header_read_timeout: Duration::from_secs(2),
        ..options()
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "test fixture registers a raw route"
    )]
    let route = Router::new().route(
        "/ok",
        get(|| async {
            let body = Body::from_stream(stream::iter([Ok::<_, std::convert::Infallible>("ok")]));
            ([(CONTENT_LENGTH, "2")], body)
        }),
    );
    let server = BoundListener::bind(loopback()).unwrap().serve_tls(
        harden(route),
        opts,
        TlsOptions {
            config: Arc::new(server_config),
            handshake_timeout: Duration::from_secs(2),
        },
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        let stream = TcpStream::connect(server.local_addr()).await.unwrap();
        let mut tls = TlsConnector::from(Arc::new(client_config))
            .connect(ServerName::try_from("localhost").unwrap(), stream)
            .await
            .unwrap();
        tls.write_all(b"GET /ok HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert!(response.ends_with(b"\r\n\r\nok"));
    })
    .await
    .unwrap();
    assert_eq!(
        server.drain(Duration::from_secs(1)).await.unwrap(),
        Drained::Complete
    );
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans.iter().find(|span| span.name == "GET /ok").unwrap();
    assert!(
        span.attributes
            .contains(&opentelemetry::KeyValue::new("url.scheme", "https"))
    );
    assert!(
        !span
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "error.type")
    );
}

// template:end grpc:http-server-span-tls-test
