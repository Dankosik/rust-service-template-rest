//! Black-box proof that the gRPC example shares the shipped process lifecycle.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use grpc_contracts::generated::{
    BidiStreamRequest, BidiStreamResponse, UnaryRequest, echo_service_client::EchoServiceClient,
};
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
        let mut command = Command::new(example_binary());
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
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| panic!("no {message:?} record before deadline"));
            let record = serde_json::from_str::<serde_json::Value>(&line)
                .unwrap_or_else(|_| panic!("stdout must be JSON, got {line:?}"));
            if record["message"] == message {
                return record;
            }
        }
    }

    fn terminate(&self) {
        kill(
            Pid::from_raw(self.child.id().cast_signed()),
            Signal::SIGTERM,
        )
        .expect("send SIGTERM");
    }

    fn wait_within(mut self, within: Duration) -> (Option<i32>, String) {
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
        (status.code(), stderr)
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

fn health_check(address: &str, certificate: String) -> Result<ServingStatus, tonic::Status> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| tonic::Status::internal("test runtime failed"))?;
    runtime.block_on(async move {
        let mut health = HealthClient::new(channel(address, certificate).await?);
        let request = Request::new(HealthCheckRequest {
            service: String::new(),
        });
        health
            .check(request)
            .await
            .map(|response| response.into_inner().status())
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

async fn watch_and_hold(
    address: &str,
    certificate: String,
    token: String,
) -> Result<
    (
        tonic::Streaming<HealthCheckResponse>,
        tonic::Streaming<BidiStreamResponse>,
        tokio::sync::mpsc::Sender<BidiStreamRequest>,
    ),
    tonic::Status,
> {
    let mut health = HealthClient::new(channel(address, certificate.clone()).await?);
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
    if initial.status() != ServingStatus::Serving {
        return Err(tonic::Status::unavailable(
            "health watch did not start serving",
        ));
    }

    let mut echo = EchoServiceClient::new(channel(address, certificate).await?);
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let mut held = Request::new(tokio_stream::wrappers::ReceiverStream::new(receiver));
    held.metadata_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("bearer metadata"),
    );
    let held = echo.bidi_stream(held).await?.into_inner();
    Ok((watch, held, sender))
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

fn listener_addresses(example: &Example) -> (String, String) {
    let http = example.await_record("http listener bound")["addr"]
        .as_str()
        .expect("HTTP address")
        .to_owned();
    let grpc = example.await_record("grpc listener bound")["addr"]
        .as_str()
        .expect("gRPC address")
        .to_owned();
    example.await_record("service_ready");
    (http, grpc)
}

#[test]
#[cfg(target_os = "linux")]
fn tls_health_and_http_share_the_example_sigterm_lifecycle() {
    let oidc = OidcFixture::new(2);
    let (certificate, private_key) = tls_material();

    let clean = Example::spawn(&oidc, &certificate, &private_key);
    let (clean_http, clean_grpc) = listener_addresses(&clean);
    let clean_ready = format!("http://{clean_http}/health/ready");
    assert!(poll_status(&clean_ready, 200, Duration::from_secs(5)));
    assert_eq!(
        health_check(&clean_grpc, certificate.clone()).expect("clean TLS health check"),
        ServingStatus::Serving
    );
    clean.terminate();
    assert!(poll_status(&clean_ready, 503, Duration::from_millis(250)));
    let (code, stderr) = clean.wait_within(Duration::from_secs(5));
    assert_eq!(code, Some(0), "stderr: {stderr}");

    let example = Example::spawn(&oidc, &certificate, &private_key);
    let (http, grpc) = listener_addresses(&example);

    let ready = format!("http://{http}/health/ready");
    assert!(
        poll_status(&ready, 200, Duration::from_secs(5)),
        "HTTP readiness never became ready"
    );

    assert_eq!(
        health_check(&grpc, certificate.clone()).expect("TLS health check"),
        ServingStatus::Serving
    );
    assert_eq!(
        unary(&grpc, certificate.clone(), &oidc.token).expect("authenticated unary"),
        "process echo"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build gRPC observer runtime");
    let (mut watch, _held, _bidi_sender) = runtime
        .block_on(watch_and_hold(
            &grpc,
            certificate.clone(),
            oidc.token.clone(),
        ))
        .expect("open health watch and held RPC");

    let started = Instant::now();
    example.terminate();
    assert!(
        poll_status(&ready, 503, Duration::from_millis(250)),
        "HTTP readiness did not enter drain"
    );
    let watched = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(2), watch.message()).await })
        .expect("health watch did not publish drain")
        .expect("health watch failed")
        .expect("health watch closed before drain state");
    assert_eq!(watched.status(), ServingStatus::NotServing);
    assert_eq!(
        health_check(&grpc, certificate).expect("TLS health during drain"),
        ServingStatus::NotServing
    );
    let (code, stderr) = example.wait_within(Duration::from_secs(15));
    assert_eq!(code, Some(3), "stderr: {stderr}");
    assert!(
        started.elapsed() >= Duration::from_secs(8),
        "the held RPC did not consume the shared gRPC drain budget"
    );
    oidc.finish();
}
