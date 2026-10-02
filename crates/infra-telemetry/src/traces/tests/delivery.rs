//! The exporter against a collector that listens: what arrives there, and
//! what unusable trust material does to the exporter.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc;

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
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        .ok()?;
    stream.flush().ok()?;
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
    handle
        .provider
        .shutdown_with_timeout(WAIT)
        .expect("the provider flushes");

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
        let handle = tracer_provider(&options("https://127.0.0.1:1"), |_| false, &trust)
            .expect("unusable trust material is not a startup failure");
        match handle.exporter_state {
            ExporterState::Degraded { reason } => {
                assert!(reason.starts_with(expected), "{expected}: {reason}");
            }
            state => panic!("{expected}: {state:?}"),
        }
    }
}
