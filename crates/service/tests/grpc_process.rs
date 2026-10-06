//! Black-box proof that the gRPC example shares the shipped process lifecycle.

#![cfg(target_os = "linux")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use grpc_contracts::example::v1::{UnaryRequest, echo_service_client::EchoServiceClient};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    },
};
use tonic::{
    Request,
    transport::{Certificate, Channel, ClientTlsConfig, Endpoint},
};
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::{HealthCheckRequest, HealthCheckResponse};

const JWT_SIGNING_DER: &[u8] =
    include_bytes!("../../infra-bearerauthn/tests/fixtures/authn-jwt-signing-key.der");

struct Example {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Example {
    fn spawn(fixture: &OidcFixture, certificate: &str, private_key: &str) -> Self {
        Self::spawn_with(fixture, certificate, private_key, &[])
    }

    fn spawn_with(
        fixture: &OidcFixture,
        certificate: &str,
        private_key: &str,
        environment: &[(&str, &str)],
    ) -> Self {
        Self::spawn_command(
            Command::new(example_binary()),
            fixture,
            certificate,
            private_key,
            environment,
        )
    }

    fn spawn_command(
        mut command: Command,
        fixture: &OidcFixture,
        certificate: &str,
        private_key: &str,
        environment: &[(&str, &str)],
    ) -> Self {
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("APP__HTTP__ADDR", "127.0.0.1:0")
            .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
            .env("APP__HTTP__READINESS_PROPAGATION_DELAY", "300ms")
            .env("APP__HTTP__DRAIN_TIMEOUT", "10s")
            .env("APP__HTTP__REQUEST_TIMEOUT", "2s")
            .env("APP__GRPC__ENABLED", "true")
            .env("APP__GRPC__ADDR", "127.0.0.1:0")
            .env("APP__GRPC__SECURITY", "tls")
            .env("APP__GRPC__CERTIFICATE", certificate)
            .env("APP__GRPC__PRIVATE_KEY", private_key)
            .env("APP__AUTHN__MODE", "oidc-jwt")
            .env("APP__AUTHN__ISSUER", &fixture.issuer)
            .env("APP__AUTHN__AUDIENCE", "grpc-example")
            .env("SSL_CERT_FILE", &fixture.root_path)
            .env("APP__LOG__FORMAT", "json")
            .envs(environment.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn gRPC example");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    fn await_record(&self, message: &str) -> serde_json::Value {
        serde_json::from_str(&self.await_line(message)).expect("stdout must be JSON")
    }

    fn await_line(&self, message: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("no {message:?} record before deadline"));
            let expected = serde_json::to_string(message).expect("encode message");
            if line.contains(&format!("\"message\":{expected}"))
                || line.contains(&format!("\"message\"={expected}"))
            {
                return line;
            }
        }
    }

    fn await_address(&self, message: &str) -> String {
        let line = self.await_line(message);
        if let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) {
            return record["addr"]
                .as_str()
                .expect("listener address")
                .to_owned();
        }
        let (_, value) = line.split_once("\"addr\"=").expect("text listener address");
        serde_json::Deserializer::from_str(value)
            .into_iter::<String>()
            .next()
            .expect("address value")
            .expect("quoted text address")
    }

    fn terminate(&self) {
        kill(
            Pid::from_raw(self.child.id().cast_signed()),
            Signal::SIGTERM,
        )
        .expect("send SIGTERM");
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
    )]
    fn wait_within(mut self, within: Duration) -> (Option<i32>, String, Vec<String>) {
        let deadline = Instant::now() + within;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll gRPC example") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("gRPC example exceeded shutdown bound");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
        }
        let mut logs = Vec::new();
        let log_deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < log_deadline {
            match self.lines.recv_timeout(Duration::from_millis(50)) {
                Ok(line) => logs.push(line),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        (status.code(), stderr, logs)
    }
}

impl Drop for Example {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct OidcFixture {
    runtime: tokio::runtime::Runtime,
    issuer: String,
    root_path: PathBuf,
    token: String,
    task: JoinHandle<()>,
}

impl OidcFixture {
    #[allow(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    fn new(clients: usize) -> Self {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
        let root = new_issuer();
        let leaf_key = KeyPair::generate().expect("generate OIDC leaf key");
        let mut leaf = CertificateParams::new(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])
            .expect("OIDC leaf names");
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = leaf.signed_by(&leaf_key, &root).expect("sign OIDC leaf");

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("build OIDC fixture runtime");
        let listener = runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("bind OIDC fixture");
        let address = listener.local_addr().expect("OIDC fixture address");
        let issuer = format!("https://127.0.0.1:{}", address.port());

        let signing = EncodingKey::from_rsa_der(JWT_SIGNING_DER);
        let mut jwk = Jwk::from_encoding_key(&signing, Algorithm::RS256).expect("derive JWK");
        jwk.common.key_id = Some("grpc-process".to_owned());
        let jwks = serde_json::json!({"keys": [jwk]}).to_string();
        let token = encode(
            &Header {
                kid: Some("grpc-process".to_owned()),
                ..Header::new(Algorithm::RS256)
            },
            &serde_json::json!({
                "iss": issuer,
                "aud": "grpc-example",
                "exp": jsonwebtoken::get_current_timestamp() + 60,
                "sub": "grpc-process"
            }),
            &signing,
        )
        .expect("sign process token");
        let discovery = serde_json::json!({
            "issuer": issuer,
            "jwks_uri": format!("{issuer}/jwks")
        })
        .to_string();

        let config = ServerConfig::builder_with_provider(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("OIDC TLS versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certificate.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
        )
        .expect("OIDC TLS certificate");
        let task = runtime.spawn(serve_oidc(
            listener,
            TlsAcceptor::from(std::sync::Arc::new(config)),
            discovery,
            jwks,
            clients,
        ));

        let root_path = std::env::temp_dir().join(format!(
            "grpc-oidc-root-{}-{}.pem",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ));
        std::fs::write(&root_path, root.pem()).expect("write OIDC root");
        Self {
            runtime,
            issuer,
            root_path,
            token,
            task,
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    fn finish(self) {
        let Self {
            runtime,
            root_path,
            task,
            ..
        } = self;
        runtime.block_on(task).expect("OIDC fixture task");
        let _ = std::fs::remove_file(root_path);
    }
}

fn new_issuer() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    CertifiedIssuer::self_signed(params, KeyPair::generate().expect("generate OIDC root key"))
        .expect("create OIDC root")
}

async fn serve_oidc(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    discovery: String,
    jwks: String,
    clients: usize,
) {
    for _ in 0..clients {
        for body in [&discovery, &jwks] {
            let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .expect("OIDC client never connected")
                .expect("accept OIDC connection");
            let mut stream = acceptor.accept(stream).await.expect("OIDC TLS handshake");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).await.expect("read OIDC request");
                assert!(read > 0, "OIDC request ended before headers");
                request.extend_from_slice(&chunk[..read]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write OIDC response");
        }
    }
}

fn example_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
            let status = Command::new(env!("CARGO"))
                .current_dir(&root)
                .args([
                    "build",
                    "--locked",
                    "--package",
                    "service",
                    "--example",
                    "grpc",
                ])
                .status()
                .expect("build gRPC example");
            assert!(status.success(), "gRPC example build failed: {status}");
            let target = std::env::var_os("CARGO_TARGET_DIR")
                .map_or_else(|| root.join("target"), PathBuf::from);
            target.join("debug/examples/grpc")
        })
        .clone()
}

fn tls_material() -> (String, String) {
    let key = rcgen::KeyPair::generate().expect("generate TLS key");
    let params =
        rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).expect("certificate names");
    let certificate = params.self_signed(&key).expect("self-sign certificate");
    (certificate.pem(), key.serialize_pem())
}

fn get_status(url: &str) -> Result<u16, ureq::Error> {
    match ureq::get(url).call() {
        Ok(response) => Ok(response.status().as_u16()),
        Err(ureq::Error::StatusCode(status)) => Ok(status),
        Err(error) => Err(error),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
fn poll_status(url: &str, expected: u16, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if get_status(url).is_ok_and(|status| status == expected) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn health_check(address: &str, certificate: String) -> Result<i32, tonic::Status> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| tonic::Status::internal("test runtime failed"))?;
    runtime.block_on(async move {
        let mut health = HealthClient::new(channel(address, certificate).await?);
        health
            .check(Request::new(HealthCheckRequest {
                service: String::new(),
            }))
            .await
            .map(|response| response.into_inner().status)
    })
}

fn unary(address: &str, certificate: String, token: &str) -> Result<String, tonic::Status> {
    let token = token.to_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| tonic::Status::internal("test runtime failed"))?;
    runtime.block_on(async move {
        let mut echo = EchoServiceClient::new(channel(address, certificate).await?);
        let mut request = Request::new(UnaryRequest {
            message: "process echo".to_owned(),
        });
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("bearer metadata"),
        );
        echo.unary(request)
            .await
            .map(|response| response.into_inner().message)
    })
}

async fn hold_watch(
    address: &str,
    certificate: String,
    token: String,
) -> Result<tonic::Streaming<HealthCheckResponse>, tonic::Status> {
    let mut health = HealthClient::new(channel(address, certificate).await?);
    let mut watch = Request::new(HealthCheckRequest {
        service: String::new(),
    });
    watch.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("bearer metadata"),
    );
    let mut watch = health.watch(watch).await?.into_inner();
    let initial = watch
        .message()
        .await?
        .ok_or_else(|| tonic::Status::internal("missing health watch state"))?;
    if initial.status != ServingStatus::Serving as i32 {
        return Err(tonic::Status::unavailable(
            "health watch did not start serving",
        ));
    }
    Ok(watch)
}

async fn channel(address: &str, certificate: String) -> Result<Channel, tonic::Status> {
    let endpoint = Endpoint::from_shared(format!("https://{address}"))
        .map_err(|_| tonic::Status::unavailable("client endpoint failed"))?
        .tls_config(
            ClientTlsConfig::new().ca_certificate(Certificate::from_pem(certificate.into_bytes())),
        )
        .map_err(|_| tonic::Status::unavailable("client TLS configuration failed"))?;
    tokio::time::timeout(Duration::from_secs(5), endpoint.connect())
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("client connect timed out"))?
        .map_err(|_| tonic::Status::unavailable("client connect failed"))
}

fn listener_addresses(example: &Example) -> (String, String, String) {
    let http = example.await_record("http listener bound")["addr"]
        .as_str()
        .expect("HTTP address")
        .to_owned();
    let diagnostics = example.await_record("diagnostics listener bound")["addr"]
        .as_str()
        .expect("diagnostics address")
        .to_owned();
    let grpc = example.await_record("grpc listener bound")["addr"]
        .as_str()
        .expect("gRPC address")
        .to_owned();
    example.await_record("service_ready");
    (http, diagnostics, grpc)
}

fn logged_messages(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|record| record["message"].as_str().map(str::to_owned))
        .collect()
}

/// Captures complete, uncompressed requests from the production HTTP exporter.
/// The existing OIDC runtime owns the receiver and its bounded shutdown.
struct Collector {
    endpoint: String,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: tokio_util::sync::CancellationToken,
    task: JoinHandle<()>,
}

impl Collector {
    fn start(runtime: &tokio::runtime::Runtime) -> Self {
        let listener = runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("bind OTLP receiver");
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&bodies);
        let handler_1 = move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
            let captured = Arc::clone(&captured);
            async move {
                assert_eq!(headers["content-type"], "application/x-protobuf");
                assert!(!headers.contains_key("content-encoding"));
                captured.lock().unwrap().push(body.to_vec());
                (
                    [("content-type", "application/x-protobuf")],
                    Vec::<u8>::new(),
                )
            }
        };
        #[allow(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new().route("/v1/traces", axum::routing::post(handler_1));
        let stop = tokio_util::sync::CancellationToken::new();
        let cancelled = stop.clone();
        let task = runtime.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(cancelled.cancelled_owned())
                .await
                .expect("OTLP receiver");
        });
        Self {
            endpoint,
            bodies,
            stop,
            task,
        }
    }

    fn finish(self, runtime: &tokio::runtime::Runtime) -> Vec<Vec<u8>> {
        self.stop.cancel();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), self.task)
                .await
                .expect("OTLP receiver shutdown bound")
                .expect("OTLP receiver task");
        });
        self.bodies.lock().unwrap().clone()
    }
}

fn exported_contains(bodies: &[Vec<u8>], value: &[u8]) -> bool {
    bodies
        .iter()
        .any(|body| body.windows(value.len()).any(|bytes| bytes == value))
}

const AUTHORITY: &str = "grpc-inbound-authority-private.example";
const USER_AGENT: &str = "grpc-inbound-agent-private";
const TRACE_ID: &str = "11111111111111111111111111111111";
const TRACEPARENT: &str = "00-11111111111111111111111111111111-2222222222222222-01";
const CLIENT_DESTINATION: &str = "grpc-configured-destination.example";
const CLIENT_AGENT: &str = "grpc-configured-agent";

async fn exercise_server_privacy(address: &str, certificate: &str, token: &str) {
    let channel = Endpoint::from_shared(format!("https://{address}"))
        .unwrap()
        .origin(format!("https://{AUTHORITY}").parse().unwrap())
        .user_agent(USER_AGENT)
        .unwrap()
        .tls_config(
            ClientTlsConfig::new()
                .domain_name("127.0.0.1")
                .ca_certificate(Certificate::from_pem(certificate)),
        )
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("privacy TLS channel");
    let mut client = EchoServiceClient::new(channel);
    for (token, accepted) in [(token, true), ("invalid-token", false)] {
        let mut request = Request::new(UnaryRequest {
            message: "privacy echo".to_owned(),
        });
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
            .metadata_mut()
            .insert("traceparent", TRACEPARENT.parse().unwrap());
        let result = tokio::time::timeout(Duration::from_secs(5), client.unary(request))
            .await
            .expect("privacy RPC bound");
        if accepted {
            assert_eq!(
                result.expect("authenticated request").into_inner().message,
                "privacy echo"
            );
        } else {
            assert_eq!(result.unwrap_err().code(), tonic::Code::Unauthenticated);
        }
    }
}

async fn exercise_client_destination(address: &str, certificate: String, token: &str) {
    let authority = format!(
        "{CLIENT_DESTINATION}:{}",
        address.parse::<std::net::SocketAddr>().unwrap().port()
    );
    let destination = format!("https://{authority}");
    let transport = infra_grpc::Client::new(
        &format!("https://{address}"),
        infra_grpc::ClientSecurity::Tls(infra_grpc::ClientTlsMaterial {
            ca_certificate_pem: Some(certificate),
            identity: None,
        }),
        Duration::from_secs(5),
    )
    .expect("observed client");
    let mut client = EchoServiceClient::with_origin(transport, destination.parse().unwrap());
    let mut request = Request::new(UnaryRequest {
        message: "client identity echo".to_owned(),
    });
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    request
        .metadata_mut()
        .insert("user-agent", CLIENT_AGENT.parse().unwrap());
    // The current observer reads Host before falling back to URI.host(),
    // whose value excludes the port. Supply the configured authority to
    // exercise preservation of both the destination name and its port.
    request
        .metadata_mut()
        .insert("host", authority.parse().unwrap());
    assert_eq!(
        client
            .unary(request)
            .await
            .expect("observed client response")
            .into_inner()
            .message,
        "client identity echo"
    );
}

fn assert_server_privacy_outputs(
    format: &str,
    stderr: &str,
    logs: &[String],
    exported: &[Vec<u8>],
) {
    let local = logs.join("\n");
    let authn = logs
        .iter()
        .find(|line| line.contains("authn_verification_failed"))
        .expect("a real request event must reach the local formatter");
    assert!(authn.contains(TRACE_ID), "request correlation: {authn}");
    assert!(authn.contains("span_id"), "span correlation: {authn}");
    assert!(
        authn.contains("example.v1.EchoService/Unary"),
        "RPC identity: {authn}"
    );
    if format == "json" {
        for line in logs {
            serde_json::from_str::<serde_json::Value>(line).expect("complete JSON records");
        }
    }
    for witness in [
        b"example.v1.EchoService/Unary".as_slice(),
        b"rpc.grpc.status_code",
        b"authn_verification_failed",
        &[0x11; 16],
        &[0x22; 8],
    ] {
        assert!(
            exported_contains(exported, witness),
            "missing exported request/correlation witness: {witness:?}"
        );
    }
    // OTLP KeyValue("rpc.grpc.status_code", AnyValue.int_value):
    // the actual successful and refused RPCs retain their numeric status.
    for code in [0_u8, 16] {
        let attribute = [
            b"\x0a\x14rpc.grpc.status_code\x12\x02\x18".as_slice(),
            &[code],
        ]
        .concat();
        assert!(
            exported_contains(exported, &attribute),
            "missing exported gRPC status {code}"
        );
    }
    // Protobuf string values are uncompressed UTF-8 at this boundary. Scan
    // every captured byte, including names, attributes and event fields.
    for excluded in [AUTHORITY, USER_AGENT] {
        assert!(!local.contains(excluded), "{format} disclosed {excluded}");
        assert!(!stderr.contains(excluded), "stderr disclosed {excluded}");
        assert!(
            !exported_contains(exported, excluded.as_bytes()),
            "OTLP disclosed {excluded}"
        );
    }
}

fn exercise_server_format(oidc: &OidcFixture, format: &str, level: &str) {
    let collector = Collector::start(&oidc.runtime);
    let (certificate, private_key) = tls_material();
    let example = Example::spawn_with(
        oidc,
        &certificate,
        &private_key,
        &[
            ("APP__LOG__FORMAT", format),
            ("APP__LOG__LEVEL", level),
            (
                "APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_ENDPOINT",
                &collector.endpoint,
            ),
            ("APP__OBSERVABILITY__OTEL__TRACES_SAMPLER", "always_on"),
        ],
    );
    let _ = example.await_address("http listener bound");
    let _ = example.await_address("diagnostics listener bound");
    let address = example.await_address("grpc listener bound");
    example.await_line("service_ready");
    oidc.runtime.block_on(async {
        exercise_server_privacy(&address, &certificate, &oidc.token).await;
        exercise_client_destination(&address, certificate, &oidc.token).await;
    });
    example.terminate();
    let (code, stderr, logs) = example.wait_within(Duration::from_secs(15));
    assert_eq!(code, Some(0), "{format}: {stderr}; {logs:?}");
    let exported = collector.finish(&oidc.runtime);
    assert_server_privacy_outputs(format, &stderr, &logs, &exported);
}

#[test]
fn grpc_server_outputs_withhold_caller_identity_and_client_keeps_destination() {
    let _ = example_binary();
    let oidc = OidcFixture::new(2);
    let client_collector = Collector::start(&oidc.runtime);
    // This integration-test executable has no other subscriber. Use the public
    // provider/subscriber so the real client adapter reaches an actual export.
    let provider = infra_telemetry::install_tracer_provider(&infra_telemetry::TracingOptions {
        service_name: "grpc-process-client".to_owned(),
        service_version: "test".to_owned(),
        vcs_revision: "test".to_owned(),
        instance_id: "grpc-client-fixture".to_owned(),
        deployment_environment: "test".to_owned(),
        sampler: infra_telemetry::ResolvedSampler::AlwaysOn,
        otlp_endpoint: Some(client_collector.endpoint.clone()),
        otlp_headers: None,
    })
    .expect("client provider");
    let logger = infra_telemetry::install_subscriber(&infra_telemetry::LoggingOptions {
        level: "off",
        format: infra_telemetry::LoggingFormat::Json,
        tracer_provider: Some(&provider),
    })
    .expect("client subscriber");

    for (format, level) in [("json", "debug"), ("text", "trace")] {
        exercise_server_format(&oidc, format, level);
    }

    assert_eq!(
        oidc.runtime
            .block_on(provider.shutdown(tokio::time::Instant::now() + Duration::from_secs(5))),
        infra_telemetry::ProviderShutdown::Completed
    );
    assert!(matches!(
        logger.shutdown(Instant::now() + Duration::from_secs(2)),
        infra_telemetry::LoggerShutdown::Completed(_)
    ));
    let client_exported = client_collector.finish(&oidc.runtime);
    for witness in [
        "example.v1.EchoService/Unary",
        "server.address",
        CLIENT_DESTINATION,
        "server.port",
        "user_agent.original",
        CLIENT_AGENT,
        "rpc.grpc.status_code",
    ] {
        assert!(
            exported_contains(&client_exported, witness.as_bytes()),
            "client destination/status missing: {witness}"
        );
    }
    oidc.finish();
}

#[test]
fn tls_health_and_http_share_the_example_sigterm_lifecycle() {
    let _ = example_binary();
    let oidc = OidcFixture::new(1);
    let (certificate, private_key) = tls_material();
    let example = Example::spawn(&oidc, &certificate, &private_key);
    let (http, diagnostics, grpc) = listener_addresses(&example);
    let ready = format!("http://{http}/health/ready");
    assert!(
        poll_status(&ready, 200, Duration::from_secs(5)),
        "HTTP readiness never became ready"
    );
    assert_eq!(
        health_check(&grpc, certificate.clone()).expect("TLS health check"),
        ServingStatus::Serving as i32
    );
    assert_eq!(
        unary(&grpc, certificate.clone(), &oidc.token).expect("authenticated unary"),
        "process echo"
    );
    // The handling time is a histogram, as grpc-ecosystem dashboards query it.
    let metrics = ureq::get(&format!("http://{diagnostics}/metrics"))
        .call()
        .expect("metrics scrape")
        .into_body()
        .read_to_string()
        .expect("metrics text");
    assert!(
        metrics.contains(
            r#"grpc_server_handling_seconds_bucket{grpc_service="example.v1.EchoService",grpc_method="Unary",le="0.005"}"#
        ),
        "{metrics}"
    );

    // A worker thread keeps serving the client connection while the test
    // waits, as a live client would; hyper's graceful drain needs its PING ack.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("build gRPC observer runtime");
    let mut watch = runtime
        .block_on(hold_watch(&grpc, certificate, oidc.token.clone()))
        .expect("open health watch");

    example.terminate();
    let watched = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(2), watch.message()).await })
        .expect("health watch did not publish drain")
        .expect("health watch failed")
        .expect("health watch closed before drain state");
    assert_eq!(watched.status, ServingStatus::NotServing as i32);
    let ended = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(2), watch.message()).await })
        .expect("health watch did not end")
        .expect("health watch end failed");
    assert!(ended.is_none(), "health watch stayed open after drain");

    let (code, stderr, logs) = example.wait_within(Duration::from_secs(15));
    let messages = logged_messages(&logs);
    assert_eq!(code, Some(0), "stderr: {stderr}; logs: {messages:?}");
    for message in [
        "readiness_disabled",
        "drain_started",
        "drain_completed",
        "grpc_drain_completed",
        "shutdown_finishing",
    ] {
        assert!(
            messages.iter().any(|logged| logged == message),
            "missing {message} in {messages:?}"
        );
    }
    oidc.finish();
}

const BACKGROUND_SCENARIO: &str = "SERVICE_BACKGROUND_SCENARIO";
const BACKGROUND_CONTROL: &str = "SERVICE_BACKGROUND_CONTROL";
const PRIVATE_BACKGROUND_ERROR: &str = "withheld-feature-failure-fixture";

fn register_background_fixture(
    services: &mut infra_grpc::Services,
    _state: &service::AppState,
    background: &mut service::BackgroundRegistration<'_>,
) -> Result<(), infra_grpc::Error> {
    let scenario = std::env::var(BACKGROUND_SCENARIO).expect("fixture scenario");
    let control = std::env::var(BACKGROUND_CONTROL).expect("fixture control address");
    let startup_refusal = scenario == "startup_refusal";
    background.spawn("feature_manager", move |stop, reporter| async move {
        let mut control = tokio::net::TcpStream::connect(control)
            .await
            .expect("connect parent control");
        if scenario == "live_error" {
            let mut fail = [0_u8; 1];
            control.read_exact(&mut fail).await.unwrap();
            assert_eq!(fail, [b'f']);
            reporter.report();
            reporter.report();
        }
        stop.cancelled().await;
        control.write_all(b"c").await.unwrap();
        // The root must keep joining this manager after a report or stop.
        let mut release = [0_u8; 1];
        control.read_exact(&mut release).await.unwrap();
        assert_eq!(release, [b'r']);
        tracing::info!("feature_work_retired");
        if matches!(scenario.as_str(), "live_error" | "cleanup_error") {
            Err(PRIVATE_BACKGROUND_ERROR)
        } else {
            Ok(())
        }
    });
    if startup_refusal {
        // The registered manager already belongs to cleanup when a later
        // registration step rejects its actual descriptor input.
        services.describe(b"invalid protobuf descriptor")?;
    }
    Ok(())
}

#[test]
fn registered_background_fixture() {
    if std::env::var_os(BACKGROUND_SCENARIO).is_none() {
        return;
    }
    let code = service::run_with_grpc(
        [std::ffi::OsString::from("background-fixture")],
        register_background_fixture,
    );
    // Production run has completed its runtime and telemetry cleanup. Avoid
    // adding test-harness output to the child's process result.
    let code = if code == std::process::ExitCode::SUCCESS {
        0
    } else if code == std::process::ExitCode::from(3) {
        3
    } else {
        1
    };
    std::process::exit(code);
}

#[test]
fn registered_feature_work_joins_and_reports_primary_or_cleanup_failure() {
    let scenarios = [
        ("complete", 0),
        ("cleanup_error", 3),
        ("live_error", 1),
        ("startup_refusal", 1),
    ];
    let oidc = OidcFixture::new(scenarios.len());
    let (certificate, private_key) = tls_material();
    for (scenario, expected_code) in scenarios {
        let listener = oidc
            .runtime
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("fixture control listener");
        let address = listener.local_addr().unwrap().to_string();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "registered_background_fixture",
            "--nocapture",
            "--test-threads=1",
        ]);
        let example = Example::spawn_command(
            command,
            &oidc,
            &certificate,
            &private_key,
            &[
                (BACKGROUND_SCENARIO, scenario),
                (BACKGROUND_CONTROL, address.as_str()),
            ],
        );
        let (mut control, _) = oidc
            .runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(10), listener.accept()).await
            })
            .expect("registered manager connects")
            .unwrap();
        if scenario == "startup_refusal" {
            example.await_record("service failed");
        } else {
            example.await_record("service_ready");
            if scenario == "live_error" {
                oidc.runtime.block_on(control.write_all(b"f")).unwrap();
                let failure = example.await_record("service failed");
                assert!(
                    failure["error"]
                        .as_str()
                        .unwrap()
                        .contains("feature_manager")
                );
                assert!(!failure.to_string().contains(PRIVATE_BACKGROUND_ERROR));
            } else {
                example.terminate();
            }
        }
        oidc.runtime.block_on(async {
            let mut cancelled = [0_u8; 1];
            tokio::time::timeout(Duration::from_secs(10), control.read_exact(&mut cancelled))
                .await
                .expect("root cancellation reaches the registered manager")
                .unwrap();
            assert_eq!(cancelled, [b'c']);
            control.write_all(b"r").await.unwrap();
        });
        let (code, stderr, logs) = example.wait_within(Duration::from_secs(10));
        assert_eq!(code, Some(expected_code), "{scenario}: {stderr}; {logs:?}");
        assert!(stderr.is_empty(), "{scenario}: {stderr}");
        assert!(
            !logs
                .iter()
                .any(|line| line.contains(PRIVATE_BACKGROUND_ERROR))
        );
        let messages = logged_messages(&logs);
        let retired = messages
            .iter()
            .position(|message| message == "feature_work_retired")
            .expect("registered work retired before process completion");
        let flushed = messages
            .iter()
            .position(|message| message == "trace_shutdown_completed")
            .expect("trace cleanup follows background completion");
        assert!(retired < flushed, "{scenario}: {logs:?}");
    }
    oidc.finish();
}
