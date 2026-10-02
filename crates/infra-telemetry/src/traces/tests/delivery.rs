//! The exporter against a collector that listens: what arrives there, and
//! whom the exporter trusts.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, mpsc};

use futures_util::FutureExt as _;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use tracing_subscriber::layer::SubscriberExt as _;

use super::*;

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
    fn start(tls: Option<Arc<ServerConfig>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the collector");
        let addr = listener.local_addr().expect("collector address");
        let (sender, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let _ = stream.set_read_timeout(Some(WAIT));
                // A refused handshake is a connection without a request.
                let request = match &tls {
                    Some(config) => ServerConnection::new(Arc::clone(config))
                        .ok()
                        .and_then(|tls| exchange(StreamOwned::new(tls, stream))),
                    None => exchange(stream),
                };
                if let Some(request) = request
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

/// A private certificate authority, the collector certificate it signed for
/// `127.0.0.1`, and a client certificate, with the PEM files the standard
/// variables would name.
struct Pki {
    issuer: CertifiedIssuer<'static, KeyPair>,
    files: tempfile::TempDir,
}

impl Pki {
    fn new() -> Self {
        let mut ca = CertificateParams::default();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        Self {
            issuer: CertifiedIssuer::self_signed(ca, KeyPair::generate().expect("CA key"))
                .expect("self-signed CA"),
            files: tempfile::tempdir().expect("directory for PEM files"),
        }
    }

    fn file(&self, variable: &'static str, name: &str, pem: &str) -> TrustFile {
        let path = self.files.path().join(name);
        std::fs::write(&path, pem).expect("write a PEM file");
        TrustFile { variable, path }
    }

    fn certificate_file(&self) -> TrustFile {
        self.file(CERTIFICATE_VARS[1], "ca.pem", &self.issuer.pem())
    }

    fn client_files(&self) -> (TrustFile, TrustFile) {
        let key = KeyPair::generate().expect("client key");
        let mut params = CertificateParams::new(vec!["exporter".to_owned()]).expect("client names");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let certificate = params.signed_by(&key, &self.issuer).expect("client leaf");
        (
            self.file(CLIENT_CERTIFICATE_VARS[1], "client.pem", &certificate.pem()),
            self.file(CLIENT_KEY_VARS[1], "client.key", &key.serialize_pem()),
        )
    }

    fn collector(&self, require_client_certificate: bool) -> Arc<ServerConfig> {
        let key = KeyPair::generate().expect("collector key");
        let mut params =
            CertificateParams::new(vec!["127.0.0.1".to_owned()]).expect("collector names");
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params
            .signed_by(&key, &self.issuer)
            .expect("collector leaf");
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("protocol versions");
        let builder = if require_client_certificate {
            let mut roots = RootCertStore::empty();
            roots.add(self.issuer.der().clone()).expect("CA root");
            builder.with_client_cert_verifier(
                WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                    .build()
                    .expect("client verifier"),
            )
        } else {
            builder.with_no_client_auth()
        };
        Arc::new(
            builder
                .with_single_cert(
                    vec![certificate.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .expect("collector certificate"),
        )
    }
}

/// One export through the exporter the trust material builds. The batch
/// processor runs the blocking client on its own thread; here it finishes
/// in one poll.
fn export(collector: &Collector, trust: &CollectorTrust) -> OTelSdkResult {
    span_exporter(
        Some(&format!("https://{}/v1/traces", collector.addr)),
        HashMap::new(),
        trust,
    )
    .expect("the exporter builds")
    .export(Vec::new())
    .now_or_never()
    .expect("the blocking client finishes in one poll")
}

#[test]
fn a_finished_span_reaches_the_collector_with_the_typed_headers() {
    let collector = Collector::start(None);
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
fn a_certificate_file_is_what_makes_a_private_collector_trusted() {
    let pki = Pki::new();
    let collector = Collector::start(Some(pki.collector(false)));

    export(&collector, &CollectorTrust::default())
        .expect_err("the platform trust store does not know the private CA");

    let trust = CollectorTrust {
        certificate: Some(pki.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &trust).expect("the certificate file's CA signed the collector");
    assert_eq!(collector.received().path, "/v1/traces");

    // Only the file's certificates are trusted: another CA's file does not
    // fall back to anything that would accept this collector.
    let other = Pki::new();
    let stranger = CollectorTrust {
        certificate: Some(other.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &stranger).expect_err("another CA did not sign the collector");
}

#[test]
fn a_client_certificate_is_presented_to_a_collector_that_requires_one() {
    let pki = Pki::new();
    let collector = Collector::start(Some(pki.collector(true)));
    let anonymous = CollectorTrust {
        certificate: Some(pki.certificate_file()),
        ..CollectorTrust::default()
    };
    export(&collector, &anonymous).expect_err("the collector requires a client certificate");

    let (client_certificate, client_key) = pki.client_files();
    let identified = CollectorTrust {
        client_certificate: Some(client_certificate),
        client_key: Some(client_key),
        ..anonymous
    };
    export(&collector, &identified).expect("the client certificate is accepted");
    assert_eq!(collector.received().path, "/v1/traces");
}

#[test]
fn unusable_trust_material_degrades_the_exporter_and_names_the_variable() {
    let pki = Pki::new();
    let (client_certificate, client_key) = pki.client_files();
    let missing = TrustFile {
        variable: CERTIFICATE_VARS[0],
        path: pki.files.path().join("absent.pem"),
    };
    let not_pem = pki.file(CERTIFICATE_VARS[1], "empty.pem", "not a certificate\n");
    for (trust, expected) in [
        (
            CollectorTrust {
                certificate: Some(missing),
                ..CollectorTrust::default()
            },
            "read the file OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE names",
        ),
        (
            CollectorTrust {
                certificate: Some(not_pem),
                ..CollectorTrust::default()
            },
            "the file OTEL_EXPORTER_OTLP_CERTIFICATE names holds no certificate",
        ),
        (
            CollectorTrust {
                client_certificate: Some(client_certificate.clone()),
                ..CollectorTrust::default()
            },
            "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE is set without OTEL_EXPORTER_OTLP_CLIENT_KEY",
        ),
        (
            CollectorTrust {
                client_key: Some(client_key.clone()),
                ..CollectorTrust::default()
            },
            "OTEL_EXPORTER_OTLP_CLIENT_KEY is set without OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
        ),
        (
            // Two keys and no certificate.
            CollectorTrust {
                client_certificate: Some(client_key.clone()),
                client_key: Some(client_key),
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
