//! Explicit, external R5 quota proof; never runs in the ordinary test suite.
//!
//! Supply `RUNTIME_PROGRESS_IMAGE` (one Linux release image containing
//! `/proof/runtime_progress`, `/bin/sh` and `sha256sum`), `RUNTIME_PROGRESS_SOURCE`
//! (its immutable source identity), and a new `RUNTIME_PROGRESS_RESULTS` directory. Run with
//! `cargo test --locked -p service --test runtime_progress -- --ignored`.
//! Docker must support cgroup v2 and host.docker.internal:host-gateway. The
//! driver and its finite OIDC/S3 peers stay outside the service's CPU quota.
//! The image's org.opencontainers.image.revision label must equal the supplied
//! source identity. Every process uses the resolved image ID, never a moving tag.
//! Numeric failures retain every run and reopen the frozen design; they never
//! cause a retry with a different rate, quota, deadline, or acceptance threshold.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::FutureExt as _;
use grpc_contracts::example::v1::{UnaryRequest, echo_service_client::EchoServiceClient};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
};
use tokio_util::sync::CancellationToken;
use tonic::{Request, transport::Channel};

type Result<T> = std::result::Result<T, String>;

const REQUEST_BUDGET: Duration = Duration::from_secs(8);
const JWT_KEY: &[u8] =
    include_bytes!("../../infra-bearerauthn/tests/fixtures/authn-jwt-signing-key.der");
const CHEAP_BODY: &str = const_str_1k();

const fn const_str_1k() -> &'static str {
    // A fixed independent oracle, not a digest or a probe substituted for Echo.
    concat!(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    )
}

fn failure(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[derive(Clone)]
struct Evidence {
    zero: Instant,
    directory: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
    task_errors: Arc<Mutex<Vec<String>>>,
}

impl Evidence {
    fn record(&self, mut event: Value) {
        event["observed_s"] = json!(self.zero.elapsed().as_secs_f64());
        self.events.lock().unwrap().push(event);
    }

    fn task_error(&self, error: impl std::fmt::Display) {
        let error = error.to_string();
        self.record(json!({"event": "driver_task_failed", "error": error}));
        self.task_errors.lock().unwrap().push(error);
    }

    async fn save(&self, name: &str) -> Result<()> {
        let records = std::mem::take(&mut *self.events.lock().unwrap());
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.directory.join(name))
            .await
            .map_err(failure)?;
        for record in records {
            let mut line = serde_json::to_vec(&record).map_err(failure)?;
            line.push(b'\n');
            file.write_all(&line).await.map_err(failure)?;
        }
        file.sync_all().await.map_err(failure)
    }
}

/// Every CLI child is killed and waited on its bound; output readers are joined.
async fn docker(arguments: &[String], within: Duration) -> Result<String> {
    let mut child = Command::new("docker")
        .args(arguments)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(failure)?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |pipe: Box<dyn AsyncRead + Send + Unpin>| async move {
        let mut bytes = Vec::new();
        pipe.take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(failure)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Docker command output exceeded 8 MiB; use file capture".to_owned());
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    let out = tokio::spawn(read(Box::new(stdout)));
    let err = tokio::spawn(read(Box::new(stderr)));
    let status = if let Ok(status) = timeout(within, child.wait()).await {
        status.map_err(failure)
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        Err(format!("Docker command exceeded {}s", within.as_secs()))
    };
    let out = out.await.map_err(failure)?;
    let err = err.await.map_err(failure)?;
    let status = status?;
    let out = out?;
    let err = err?;
    if !status.success() {
        return Err(format!("Docker failed: {status}; {err}; {out}"));
    }
    Ok(out)
}

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

struct Peers {
    issuer: String,
    upload: String,
    token: String,
    root: PathBuf,
    stop: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl Peers {
    async fn start(evidence: &Evidence) -> Result<Self> {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
        let mut root_params = CertificateParams::default();
        root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let root = CertifiedIssuer::self_signed(root_params, KeyPair::generate().map_err(failure)?)
            .map_err(failure)?;
        let key = KeyPair::generate().map_err(failure)?;
        let cert = CertificateParams::new(vec!["host.docker.internal".to_owned()])
            .map_err(failure)?
            .signed_by(&key, &root)
            .map_err(failure)?;
        let tls = ServerConfig::builder_with_provider(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .map_err(failure)?
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .map_err(failure)?;
        let oidc = TcpListener::bind("0.0.0.0:0").await.map_err(failure)?;
        let s3 = TcpListener::bind("0.0.0.0:0").await.map_err(failure)?;
        let issuer = format!(
            "https://host.docker.internal:{}",
            oidc.local_addr().map_err(failure)?.port()
        );
        let upload = format!(
            "http://host.docker.internal:{}",
            s3.local_addr().map_err(failure)?.port()
        );
        let signing = EncodingKey::from_rsa_der(JWT_KEY);
        let mut jwk = Jwk::from_encoding_key(&signing, Algorithm::RS256).map_err(failure)?;
        jwk.common.key_id = Some("runtime-progress".to_owned());
        let jwks = json!({"keys": [jwk]}).to_string();
        let discovery = json!({"issuer": issuer, "jwks_uri": format!("{issuer}/jwks")}).to_string();
        let token = encode(
            &Header { kid: Some("runtime-progress".to_owned()), ..Header::new(Algorithm::RS256) },
            &json!({"iss": issuer, "aud": "runtime-progress", "sub": "quota-driver", "exp": jsonwebtoken::get_current_timestamp() + 3600}),
            &signing,
        ).map_err(failure)?;
        let root_path = evidence.directory.join("oidc-root.pem");
        tokio::fs::write(&root_path, root.pem())
            .await
            .map_err(failure)?;
        let stop = CancellationToken::new();
        let tasks = vec![
            tokio::spawn(serve_peer(
                oidc,
                Some(TlsAcceptor::from(Arc::new(tls))),
                discovery,
                jwks,
                stop.clone(),
                evidence.clone(),
            )),
            tokio::spawn(serve_peer(
                s3,
                None,
                String::new(),
                String::new(),
                stop.clone(),
                evidence.clone(),
            )),
        ];
        Ok(Self {
            issuer,
            upload,
            token,
            root: root_path,
            stop,
            tasks,
        })
    }

    async fn finish(self) {
        self.stop.cancel();
        for mut task in self.tasks {
            if timeout(Duration::from_secs(3), &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
    }
}

async fn serve_peer(
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    discovery: String,
    jwks: String,
    stop: CancellationToken,
    evidence: Evidence,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept(), if connections.len() < 16 => {
                let Ok((stream, _)) = accepted else { break };
                let tls = tls.clone();
                let discovery = discovery.clone();
                let jwks = jwks.clone();
                let evidence = evidence.clone();
                connections.spawn(async move {
                    let result = timeout(Duration::from_secs(10), async {
                        if let Some(tls) = tls {
                            let stream = tls.accept(stream).await.map_err(failure)?;
                            answer_peer(stream, &discovery, &jwks, &evidence).await
                        } else {
                            answer_peer(stream, "", "", &evidence).await
                        }
                    }).await;
                    evidence.record(json!({"event": "peer_completion", "result": format!("{result:?}")}));
                });
            }
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

/// HTTP/1 requests are finite. Handles transport chunks separately from S3's
/// aws-chunked content encoding, including the final checksum trailer.
async fn read_http<S: AsyncRead + Unpin>(stream: &mut S) -> Result<(String, Vec<u8>)> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= 16 * 1024 {
            return Err("peer request headers exceeded bound".into());
        }
        head.push(stream.read_u8().await.map_err(failure)?);
    }
    let head = String::from_utf8(head).map_err(failure)?;
    let lower = head.to_ascii_lowercase();
    let length = lower
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .map(|value| value.trim().parse::<usize>().map_err(failure))
        .transpose()?;
    let mut body = Vec::new();
    if lower.contains("transfer-encoding: chunked") {
        loop {
            let mut line = Vec::new();
            while !line.ends_with(b"\r\n") {
                if line.len() >= 4096 {
                    return Err("chunk header exceeded bound".into());
                }
                line.push(stream.read_u8().await.map_err(failure)?);
            }
            let line = String::from_utf8(line).map_err(failure)?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap(), 16)
                .map_err(failure)?;
            if size == 0 {
                // Trailer section: either CRLF or bounded fields ending CRLFCRLF.
                let mut trailers = Vec::new();
                loop {
                    if trailers.len() >= 16 * 1024 {
                        return Err("trailers exceeded bound".into());
                    }
                    trailers.push(stream.read_u8().await.map_err(failure)?);
                    if trailers == b"\r\n" || trailers.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                break;
            }
            if body.len() + size > 512 * 1024 {
                return Err("peer body exceeded bound".into());
            }
            let offset = body.len();
            body.resize(offset + size, 0);
            stream
                .read_exact(&mut body[offset..])
                .await
                .map_err(failure)?;
            let mut ending = [0; 2];
            stream.read_exact(&mut ending).await.map_err(failure)?;
            if ending != *b"\r\n" {
                return Err("invalid chunk ending".into());
            }
        }
    } else if let Some(length) = length {
        if length > 512 * 1024 {
            return Err("peer body exceeded bound".into());
        }
        body.resize(length, 0);
        stream.read_exact(&mut body).await.map_err(failure)?;
    }
    Ok((head, body))
}

async fn answer_peer<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    discovery: &str,
    jwks: &str,
    evidence: &Evidence,
) -> Result<()> {
    let (head, body) = read_http(&mut stream).await?;
    let request = head.lines().next().ok_or("missing request line")?;
    let response = if discovery.is_empty() {
        if !request.starts_with("PUT ") {
            return Err(format!("unexpected S3 route: {request}"));
        }
        let lower = head.to_ascii_lowercase();
        let decoded = if lower.contains("content-encoding: aws-chunked") {
            let declared = lower
                .lines()
                .find_map(|line| line.strip_prefix("x-amz-decoded-content-length:"))
                .ok_or("missing decoded S3 size")?
                .trim()
                .parse::<usize>()
                .map_err(failure)?;
            let actual = decode_chunks(&body)?.len();
            if actual != declared {
                return Err("S3 declared/received decoded sizes differ".into());
            }
            actual
        } else {
            body.len()
        };
        if decoded != 128 * 1024 || body.len() < decoded || body.len() > decoded + 16 * 1024 {
            return Err(format!(
                "unexpected S3 body sizes: decoded={decoded}, wire={}",
                body.len()
            ));
        }
        evidence.record(json!({"event": "upload_received", "request": request, "wire_bytes": body.len(), "decoded_bytes": decoded}));
        ""
    } else if request == "GET /.well-known/openid-configuration HTTP/1.1" {
        discovery
    } else if request == "GET /jwks HTTP/1.1" {
        jwks
    } else {
        return Err(format!("unexpected OIDC route: {request}"));
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response}",
        response.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(failure)?;
    stream.shutdown().await.map_err(failure)
}

#[derive(Clone)]
struct Client {
    channel: Channel,
    token: String,
}

impl Client {
    async fn rpc(&self, work: &str) -> std::result::Result<String, tonic::Status> {
        let mut request = Request::new(UnaryRequest {
            message: CHEAP_BODY.to_owned(),
        });
        request.set_timeout(REQUEST_BUDGET);
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        if !work.is_empty() {
            request
                .metadata_mut()
                .insert("x-runtime-work", work.parse().unwrap());
        }
        let mut client = EchoServiceClient::new(self.channel.clone());
        timeout(REQUEST_BUDGET, client.unary(request))
            .await
            .map_err(|_| tonic::Status::deadline_exceeded("external request budget"))?
            .map(|response| response.into_inner().message)
    }

    async fn snapshot(&self, evidence: &Evidence, label: &str) -> Result<Value> {
        let sent = evidence.zero.elapsed().as_secs_f64();
        let value: Result<Value> = self
            .rpc("snapshot")
            .await
            .map_err(failure)
            .and_then(|body| serde_json::from_str(&body).map_err(failure));
        let received = evidence.zero.elapsed().as_secs_f64();
        evidence.record(json!({"event": "snapshot", "label": label,
            "sent_s": sent, "received_s": received,
            "value": value.as_ref().ok(), "error": value.as_ref().err()}));
        value
    }
}

struct Container {
    name: String,
    http: String,
    metrics: String,
    client: Client,
}

fn container_command(image: &str, name: &str, peers: &Peers, primary_failure: bool) -> Vec<String> {
    // A stopped cat holds the read end open without consuming it: this blocks
    // the actual application writer even though Docker continuously drains cat.
    let shell = "mkfifo /tmp/runtime-progress.stdout; cat /tmp/runtime-progress.stdout & echo $! > /tmp/runtime-progress.reader; exec /proof/runtime_progress > /tmp/runtime-progress.stdout";
    let mut command = args(&[
        "run",
        "--detach",
        "--name",
        name,
        "--cpu-period",
        "100000",
        "--cpu-quota",
        "100000",
        "--add-host",
        "host.docker.internal:host-gateway",
        "--entrypoint",
        "/bin/sh",
        "--publish",
        "127.0.0.1::8080",
        "--publish",
        "127.0.0.1::9090",
        "--publish",
        "127.0.0.1::50051",
        "--mount",
        &format!(
            "type=bind,source={},target=/proof/oidc-root.pem,readonly",
            peers.root.display()
        ),
    ]);
    for (key, value) in [
        ("APP__APP__ENV", "local"),
        (
            "APP__RUNTIME__WORKER_THREADS",
            if primary_failure { "0" } else { "1" },
        ),
        ("APP__HTTP__ADDR", "0.0.0.0:8080"),
        ("APP__HTTP__DRAIN_TIMEOUT", "25s"),
        ("APP__HTTP__GRACE_PERIOD", "45s"),
        ("APP__HTTP__READINESS_PROPAGATION_DELAY", "15s"),
        ("APP__HTTP__REQUEST_TIMEOUT", "8s"),
        ("APP__HEALTH__REFRESH_INTERVAL", "2s"),
        ("APP__HEALTH__PROBE_BUDGET", "4s"),
        ("APP__HEALTH__FAILURE_THRESHOLD", "3"),
        ("APP__OBSERVABILITY__METRICS__ADDR", "0.0.0.0:9090"),
        ("APP__GRPC__ENABLED", "true"),
        ("APP__GRPC__ADDR", "0.0.0.0:50051"),
        ("APP__GRPC__SECURITY", "plaintext"),
        ("APP__GRPC__REQUEST_TIMEOUT", "8s"),
        ("APP__AUTHN__MODE", "oidc-jwt"),
        ("APP__AUTHN__ISSUER", peers.issuer.as_str()),
        ("APP__AUTHN__AUDIENCE", "runtime-progress"),
        ("APP__LOG__FORMAT", "json"),
        ("SSL_CERT_FILE", "/proof/oidc-root.pem"),
        ("RUNTIME_PROGRESS_UPLOAD_ENDPOINT", peers.upload.as_str()),
    ] {
        command.extend(args(&["--env", &format!("{key}={value}")]));
    }
    command.extend(args(&[image, "-c", shell]));
    command
}

async fn launch(
    image: &str,
    name: &str,
    peers: &Peers,
    evidence: &Evidence,
    primary_failure: bool,
) -> Result<Option<Container>> {
    let command = container_command(image, name, peers, primary_failure);
    evidence.record(json!({"event": "container_command", "arguments": command}));
    let id = docker(&command, Duration::from_secs(30)).await?;
    evidence.record(json!({"event": "container_started", "name": name, "id": id.trim()}));
    if primary_failure {
        return Ok(None);
    }
    let mut addresses = Vec::new();
    for port in ["8080/tcp", "9090/tcp", "50051/tcp"] {
        addresses.push(
            docker(&args(&["port", name, port]), Duration::from_secs(5))
                .await?
                .trim()
                .to_owned(),
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if http_get(&addresses[0], "/health/ready")
            .await
            .is_ok_and(|response| response.0 == 200)
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!("{name} never became ready"));
        }
        sleep_until(Instant::now() + Duration::from_millis(100)).await;
    }
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{}", addresses[2]))
        .map_err(failure)?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(failure)?;
    let inspect = docker(&args(&["inspect", name]), Duration::from_secs(5)).await?;
    tokio::fs::write(
        evidence.directory.join(format!("{name}.inspect.json")),
        inspect,
    )
    .await
    .map_err(failure)?;
    let binary = docker(
        &args(&["exec", name, "sha256sum", "/proof/runtime_progress"]),
        Duration::from_secs(5),
    )
    .await?;
    evidence.record(json!({"event": "fixture_binary", "container": name, "sha256sum": binary}));
    Ok(Some(Container {
        name: name.to_owned(),
        http: addresses[0].clone(),
        metrics: addresses[1].clone(),
        client: Client {
            channel,
            token: peers.token.clone(),
        },
    }))
}

async fn http_get(address: &str, path: &str) -> Result<(u16, String)> {
    timeout(Duration::from_secs(2), async {
        let mut stream = TcpStream::connect(address).await.map_err(failure)?;
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .map_err(failure)?;
        let mut bytes = Vec::new();
        (&mut stream)
            .take(2 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .await
            .map_err(failure)?;
        let response = String::from_utf8(bytes).map_err(failure)?;
        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or("incomplete HTTP response")?;
        let status = head
            .split_whitespace()
            .nth(1)
            .ok_or("missing HTTP status")?
            .parse::<u16>()
            .map_err(failure)?;
        // Hyper's metrics response is a sized body; decoding a transport-chunked
        // response here keeps the observer independent of that optimization.
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            decode_chunks(body.as_bytes())?
        } else {
            body.as_bytes().to_vec()
        };
        Ok((status, String::from_utf8(body).map_err(failure)?))
    })
    .await
    .map_err(|_| "external HTTP scrape timed out after 2s".to_owned())?
}

fn decode_chunks(mut input: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let end = input
            .windows(2)
            .position(|pair| pair == b"\r\n")
            .ok_or("invalid chunk header")?;
        let size = usize::from_str_radix(
            std::str::from_utf8(&input[..end])
                .map_err(failure)?
                .split(';')
                .next()
                .unwrap(),
            16,
        )
        .map_err(failure)?;
        input = &input[end + 2..];
        if size == 0 {
            return Ok(output);
        }
        if size > input.len().saturating_sub(2) || &input[size..size + 2] != b"\r\n" {
            return Err("incomplete chunk".into());
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
}

async fn reader_signal(name: &str, signal: &str, evidence: &Evidence) -> Result<()> {
    docker(
        &args(&[
            "exec",
            name,
            "/bin/sh",
            "-c",
            &format!("kill -{signal} $(cat /tmp/runtime-progress.reader)"),
        ]),
        Duration::from_secs(5),
    )
    .await?;
    evidence.record(json!({"event": "stdout_reader_signal", "container": name, "signal": signal}));
    Ok(())
}

async fn cgroup(name: &str, label: &str, evidence: &Evidence) -> Result<BTreeMap<String, u64>> {
    let max_sent = evidence.zero.elapsed().as_secs_f64();
    let max = docker(
        &args(&["exec", name, "cat", "/sys/fs/cgroup/cpu.max"]),
        Duration::from_secs(5),
    )
    .await;
    let max_received = evidence.zero.elapsed().as_secs_f64();
    let stat_sent = evidence.zero.elapsed().as_secs_f64();
    let stat = docker(
        &args(&["exec", name, "cat", "/sys/fs/cgroup/cpu.stat"]),
        Duration::from_secs(5),
    )
    .await;
    let stat_received = evidence.zero.elapsed().as_secs_f64();
    evidence.record(json!({"event": "cgroup", "label": label,
        "cpu.max": max.as_ref().ok(), "cpu.stat": stat.as_ref().ok(),
        "cpu.max_error": max.as_ref().err(), "cpu.stat_error": stat.as_ref().err(),
        "cpu_max_sent_s": max_sent, "cpu_max_received_s": max_received,
        "cpu_stat_sent_s": stat_sent, "cpu_stat_received_s": stat_received}));
    let max = max?;
    let stat = stat?;
    if max.trim() != "100000 100000" {
        return Err(format!("unexpected cpu.max: {max}"));
    }
    stat.lines()
        .map(|line| {
            let (key, value) = line.split_once(' ').ok_or("invalid cpu.stat")?;
            Ok((key.to_owned(), value.parse().map_err(failure)?))
        })
        .collect()
}

#[derive(Clone)]
struct Completion {
    offered: f64,
    completed: f64,
    latency: f64,
    success: bool,
}

fn dropped_offer(
    evidence: &Evidence,
    label: &str,
    work: &str,
    index: u32,
    scheduled: Instant,
    start: Instant,
    reason: &str,
) -> Completion {
    let now = Instant::now();
    evidence.record(
        json!({"event": "dropped_offer", "phase": label, "work": work,
        "index": index, "reason": reason,
        "scheduled_s": scheduled.duration_since(evidence.zero).as_secs_f64(),
        "dropped_s": now.duration_since(evidence.zero).as_secs_f64()}),
    );
    Completion {
        offered: scheduled.duration_since(start).as_secs_f64(),
        completed: now.duration_since(start).as_secs_f64(),
        latency: now.duration_since(scheduled).as_secs_f64(),
        success: false,
    }
}

async fn traffic(
    client: Client,
    evidence: Evidence,
    label: String,
    work: &'static str,
    rate: u32,
    cap: usize,
    period: (Instant, u32),
) -> Vec<Completion> {
    let (start, seconds) = period;
    let end = start + Duration::from_secs(u64::from(seconds));
    let permits = Arc::new(Semaphore::new(cap));
    let mut requests = JoinSet::new();
    let mut completed = Vec::new();
    for offer in 0..rate * seconds {
        let scheduled = start + Duration::from_secs_f64(f64::from(offer) / f64::from(rate));
        sleep_until(scheduled).await;
        while let Some(result) = requests.try_join_next() {
            match result {
                Ok(value) => completed.push(value),
                Err(error) => evidence.task_error(error),
            }
        }
        let offset = scheduled.duration_since(start).as_secs_f64();
        evidence.record(json!({"event": "offer", "phase": label, "work": work, "index": offer, "scheduled_s": scheduled.duration_since(evidence.zero).as_secs_f64(), "dispatch_late_s": scheduled.elapsed().as_secs_f64()}));
        if work == "cpu" && Instant::now() >= end {
            completed.push(dropped_offer(
                &evidence,
                &label,
                work,
                offer,
                scheduled,
                start,
                "cpu_window_ended",
            ));
            continue;
        }
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            completed.push(dropped_offer(
                &evidence,
                &label,
                work,
                offer,
                scheduled,
                start,
                "outstanding_cap",
            ));
            continue;
        };
        let client = client.clone();
        let evidence = evidence.clone();
        let label = label.clone();
        requests.spawn(async move {
            let _permit = permit;
            // A spawned driver future can first run after its scheduled
            // window; it must not replay a missed CPU offer then either.
            if work == "cpu" && Instant::now() >= end {
                return dropped_offer(&evidence, &label, work, offer, scheduled, start, "cpu_window_ended");
            }
            let sent = Instant::now();
            let result = client.rpc(work).await;
            let done = Instant::now();
            let success = result.as_ref().is_ok_and(|body| if work.is_empty() { body == CHEAP_BODY } else { body == "ok" });
            let outcome = match &result {
                Ok(_) if success => "success".to_owned(),
                Ok(_) => "wrong_body".to_owned(),
                Err(status) if status.code() == tonic::Code::DeadlineExceeded => "timeout".to_owned(),
                Err(status) => format!("grpc_{:?}", status.code()),
            };
            evidence.record(json!({"event": "completion", "phase": label, "work": work, "index": offer, "outcome": outcome, "sent_s": sent.duration_since(evidence.zero).as_secs_f64(), "completed_s": done.duration_since(evidence.zero).as_secs_f64(), "service_latency_s": done.duration_since(sent).as_secs_f64(), "offer_latency_s": done.duration_since(scheduled).as_secs_f64()}));
            Completion { offered: offset, completed: done.duration_since(start).as_secs_f64(), latency: done.duration_since(scheduled).as_secs_f64(), success }
        });
    }
    sleep_until(end).await;
    while let Some(result) = requests.join_next().await {
        match result {
            Ok(value) => completed.push(value),
            Err(error) => evidence.task_error(error),
        }
    }
    completed
}

#[derive(Clone)]
struct Scrape {
    started: f64,
    completed: f64,
    route: &'static str,
    status: Option<u16>,
    body: String,
}

async fn observations(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    start: Instant,
    seconds: u32,
) -> Vec<Scrape> {
    let mut requests = JoinSet::new();
    let mut scrapes = Vec::new();
    for index in 0..seconds * 10 {
        let scheduled = start + Duration::from_millis(u64::from(index) * 100);
        sleep_until(scheduled).await;
        while let Some(result) = requests.try_join_next() {
            match result {
                Ok(scrape) => scrapes.push(scrape),
                Err(error) => evidence.task_error(error),
            }
        }
        for (address, route) in [
            (&container.http, "/health/live"),
            (&container.http, "/health/ready"),
            (&container.metrics, "/metrics"),
        ] {
            let address = address.clone();
            let evidence = evidence.clone();
            let label = label.to_owned();
            requests.spawn(async move {
                let started = Instant::now().duration_since(start).as_secs_f64();
                let response = http_get(&address, route).await;
                let completed = Instant::now().duration_since(start).as_secs_f64();
                let status = response.as_ref().ok().map(|value| value.0);
                let body = response.as_ref().map_or_else(Clone::clone, |value| value.1.clone());
                evidence.record(json!({"event": "scrape", "phase": label, "route": route, "index": index, "scheduled_s": scheduled.duration_since(evidence.zero).as_secs_f64(), "started_s": started, "completion_s": completed, "latency_s": scheduled.elapsed().as_secs_f64(), "status": status, "body": body}));
                Scrape { started, completed, route, status, body }
            });
        }
    }
    sleep_until(start + Duration::from_secs(u64::from(seconds))).await;
    while let Some(result) = requests.join_next().await {
        match result {
            Ok(scrape) => scrapes.push(scrape),
            Err(error) => evidence.task_error(error),
        }
    }
    scrapes
}

fn metric(body: &str, key: &str) -> Option<f64> {
    body.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == key)
            .then(|| value.split_whitespace().next()?.parse().ok())
            .flatten()
    })
}

fn total_metric(body: &str, prefix: &str) -> f64 {
    body.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(' ')?;
            name.starts_with(prefix)
                .then(|| value.parse::<f64>().ok())
                .flatten()
        })
        .sum()
}

fn gap(mut completions: Vec<f64>, start: f64, end: f64) -> f64 {
    completions.retain(|at| *at >= start && *at <= end);
    completions.extend([start, end]);
    completions.sort_by(f64::total_cmp);
    completions
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .fold(0.0, f64::max)
}

struct Summary {
    goodput: f64,
    offered: usize,
    succeeded: usize,
    p99: f64,
    gap: f64,
}

fn summarize(
    samples: &[Completion],
    start: f64,
    end: f64,
    evidence: &Evidence,
    label: &str,
) -> Summary {
    let offers: Vec<_> = samples
        .iter()
        .filter(|sample| sample.offered >= start && sample.offered < end)
        .collect();
    let mut latency: Vec<_> = offers
        .iter()
        .filter(|sample| sample.success)
        .map(|sample| sample.latency)
        .collect();
    latency.sort_by(f64::total_cmp);
    let p99 = latency
        .get((latency.len() * 99).div_ceil(100).saturating_sub(1))
        .copied()
        .unwrap_or(f64::INFINITY);
    let successes: Vec<_> = samples
        .iter()
        .filter(|sample| sample.success && sample.completed >= start && sample.completed < end)
        .map(|sample| sample.completed)
        .collect();
    let result = Summary {
        goodput: f64::from(u32::try_from(successes.len()).expect("finite offered count fits u32"))
            / (end - start),
        offered: offers.len(),
        succeeded: latency.len(),
        p99,
        gap: gap(successes, start, end),
    };
    evidence.record(json!({"event": "traffic_summary", "phase": label, "start_s": start, "end_s": end, "goodput": result.goodput, "offered": result.offered, "succeeded": result.succeeded, "p99_s": result.p99, "max_gap_s": result.gap}));
    result
}

fn require(failures: &mut Vec<String>, condition: bool, description: impl Into<String>) {
    if !condition {
        failures.push(description.into());
    }
}

fn number(snapshot: &Value, field: &str) -> f64 {
    snapshot[field].as_f64().unwrap_or(f64::NAN)
}

fn counter(snapshot: &Value, field: &str) -> Option<u64> {
    snapshot[field].as_u64()
}

fn work_accounted(snapshot: &Value, admitted: &str, terminal: &[&str]) -> bool {
    let Some(admitted) = counter(snapshot, admitted) else {
        return false;
    };
    terminal.iter().try_fold(0_u64, |sum, field| {
        sum.checked_add(counter(snapshot, field)?)
    }) == Some(admitted)
}

async fn cancel_active_waiter(client: &Client, evidence: &Evidence) -> Result<()> {
    let before = client.snapshot(evidence, "cancellation_before").await?;
    if number(&before, "cpu_active") != 0.0 {
        return Err("cancellation witness started with active CPU work".into());
    }
    let owned = client.clone();
    let mut call = tokio::spawn(async move { owned.rpc("cpu").await });
    let witness = async {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let active = client.snapshot(evidence, "cancellation_active").await?;
            if counter(&active, "cpu_active") == Some(1)
                && counter(&active, "cpu_occupied") == Some(1)
            {
                break;
            }
            if call.is_finished() || Instant::now() >= deadline {
                return Err("CPU closure ended before active waiter could be witnessed".to_owned());
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    }
    .await;
    call.abort();
    let _ = (&mut call).await;
    evidence.record(json!({"event": "cpu_waiter_aborted", "active_observed": witness.is_ok()}));
    witness?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut cancellation_seen = false;
    loop {
        let state = client.snapshot(evidence, "cancellation_custody").await?;
        if number(&state, "cpu_cancelled_active") > number(&before, "cpu_cancelled_active") {
            cancellation_seen = true;
            if counter(&state, "cpu_active") != Some(1)
                || counter(&state, "cpu_occupied") != Some(1)
            {
                return Err("CPU capacity not held after cancelled waiter".into());
            }
            break;
        }
        if Instant::now() >= deadline || number(&state, "cpu_active") == 0.0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    if !cancellation_seen {
        return Err("cancelled active CPU ownership was not witnessed".into());
    }
    let replacement = client.rpc("cpu").await;
    evidence.record(
        json!({"event": "cpu_replacement_after_cancel", "result": format!("{replacement:?}")}),
    );
    if !replacement
        .as_ref()
        .is_err_and(|error| error.code() == tonic::Code::ResourceExhausted)
    {
        return Err(
            "replacement CPU request was not refused while cancelled work remained active".into(),
        );
    }
    loop {
        let state = client.snapshot(evidence, "cancellation_completion").await?;
        if number(&state, "cpu_active") == 0.0 && number(&state, "cpu_occupied") == 0.0 {
            if number(&state, "cpu_refused_after_cancel")
                <= number(&before, "cpu_refused_after_cancel")
            {
                return Err("CPU owner did not record refusal after cancellation".into());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("cancelled finite CPU closure did not finish".into());
        }
        tokio::task::yield_now().await;
    }
}

fn sampler_limit_checks(scrape: &Scrape, failures: &mut Vec<String>, label: &str) {
    let count = metric(&scrape.body, "runtime_scheduler_lag_seconds_count").unwrap_or(f64::NAN);
    let within_two = metric(
        &scrape.body,
        "runtime_scheduler_lag_seconds_bucket{le=\"2\"}",
    )
    .unwrap_or(f64::NAN);
    require(
        failures,
        // Exact equality of finite positive counts; no rounding tolerance
        // may conceal even one observation above the bound.
        count.is_finite() && count > 0.0 && within_two.total_cmp(&count).is_eq(),
        format!(
            "{label}: sampler histogram exceeds2s or is unavailable at {}s",
            scrape.completed
        ),
    );
    // Inclusive le=2 alone cannot establish the strict frozen boundary.
    let maximum = metric(&scrape.body, "runtime_scheduler_lag_max_seconds").unwrap_or(f64::NAN);
    require(
        failures,
        maximum.is_finite() && (0.0..2.0).contains(&maximum),
        format!(
            "{label}: strict sampler maximum <2s is not established at {}s ({maximum})",
            scrape.completed
        ),
    );
}

fn final_sampler_checks(
    metrics: &[&Scrape],
    window_end: Option<&Scrape>,
    final_metrics: &Scrape,
    failures: &mut Vec<String>,
    label: &str,
) {
    let final_age =
        metric(&final_metrics.body, "runtime_scheduler_sample_age_seconds").unwrap_or(f64::NAN);
    let final_samples =
        metric(&final_metrics.body, "runtime_scheduler_samples_total").unwrap_or(f64::NAN);
    let mixed_end_samples = window_end
        .and_then(|scrape| metric(&scrape.body, "runtime_scheduler_samples_total"))
        .unwrap_or(f64::NAN);
    require(
        failures,
        final_metrics.status == Some(200)
            && final_metrics.started >= 40.0
            && final_metrics.completed >= final_metrics.started
            && final_age.is_finite()
            && (0.0..=1.0).contains(&final_age)
            && final_samples.is_finite()
            && final_samples > 0.0
            && final_samples > mixed_end_samples
            && metrics.iter().all(|scrape| {
                metric(&scrape.body, "runtime_scheduler_samples_total").is_some_and(|samples| {
                    samples.is_finite() && samples > 0.0 && samples <= final_samples
                })
            }),
        format!(
            "{label}: missing or stale final sampler observation after workload/resumed sampling \
             (status {:?}, start {}s, completion {}s, age {final_age}s, count {final_samples})",
            final_metrics.status, final_metrics.started, final_metrics.completed
        ),
    );
}

fn recovery_metrics_checks(
    scrapes: &[Scrape],
    metrics: &[&Scrape],
    failures: &mut Vec<String>,
    label: &str,
) {
    let recovery: Vec<_> = metrics
        .iter()
        .filter(|scrape| scrape.completed >= 35.0 && scrape.completed <= 40.0)
        .collect();
    require(
        failures,
        !recovery.is_empty(),
        format!("{label}: no successful final-five-second sampler observation"),
    );
    for scrape in &recovery {
        let age = metric(&scrape.body, "runtime_scheduler_sample_age_seconds").unwrap_or(f64::NAN);
        require(
            failures,
            age <= 1.0,
            format!("{label}: recovery sampler age {age}s"),
        );
        require(
            failures,
            metric(&scrape.body, "readiness_stale_after_seconds") == Some(16.0),
            format!("{label}: frozen readiness stale bound changed"),
        );
    }
    require(
        failures,
        scrapes.iter().any(|scrape| {
            scrape.route == "/health/ready"
                && scrape.status == Some(200)
                && scrape.completed >= 39.0
                && scrape.completed <= 40.0
        }),
        format!("{label}: readiness did not recover within10s"),
    );
    if let (Some(first), Some(last)) = (recovery.first(), recovery.last()) {
        require(
            failures,
            metric(&last.body, "runtime_scheduler_samples_total").unwrap_or(0.0)
                > metric(&first.body, "runtime_scheduler_samples_total").unwrap_or(f64::INFINITY),
            format!("{label}: frozen sampler observation did not recover"),
        );
        require(
            failures,
            metric(&last.body, "readiness_last_completed_timestamp_seconds").unwrap_or(0.0)
                > metric(&first.body, "readiness_last_completed_timestamp_seconds")
                    .unwrap_or(f64::INFINITY),
            format!("{label}: readiness completion did not refresh during recovery"),
        );
    }
}

fn metrics_checks(
    scrapes: &[Scrape],
    initial_metrics: &str,
    final_metrics: &Scrape,
    failures: &mut Vec<String>,
    evidence: &Evidence,
    label: &str,
) {
    for route in ["/health/live", "/health/ready", "/metrics"] {
        let successful: Vec<_> = scrapes
            .iter()
            .filter(|scrape| {
                scrape.route == route && scrape.status == Some(200) && scrape.completed <= 30.0
            })
            .map(|scrape| scrape.completed)
            .collect();
        let max_gap = gap(successful, 0.0, 30.0);
        evidence.record(
            json!({"event": "external_gap", "phase": label, "route": route, "max_gap_s": max_gap}),
        );
        require(
            failures,
            max_gap < 2.0,
            format!("{label}: {route} response gap {max_gap}s >=2s"),
        );
    }
    let mut metrics: Vec<_> = scrapes
        .iter()
        .filter(|scrape| scrape.route == "/metrics" && scrape.status == Some(200))
        .collect();
    metrics.sort_by(|a, b| a.completed.total_cmp(&b.completed));
    let window_end = metrics
        .iter()
        .rev()
        .find(|scrape| scrape.completed <= 30.0)
        .copied();
    let after = metrics
        .iter()
        .find(|scrape| scrape.completed >= 32.0)
        .copied();
    // The percentile snapshot includes the mixed boundary's upkeep spill;
    // it is not the final oracle for later cumulative observations. Retain
    // every adverse maximum/bucket, even if a later scrape looks healthy.
    for scrape in metrics
        .iter()
        .copied()
        .chain(std::iter::once(final_metrics))
        .filter(|scrape| scrape.status == Some(200))
    {
        sampler_limit_checks(scrape, failures, label);
    }
    final_sampler_checks(&metrics, window_end, final_metrics, failures, label);
    if let (Some(window_end), Some(after)) = (window_end, after) {
        let delta = |key: &str| {
            metric(&after.body, key).unwrap_or(f64::NAN)
                - metric(initial_metrics, key).unwrap_or(0.0)
        };
        let count = delta("runtime_scheduler_lag_seconds_count");
        let within_half = delta("runtime_scheduler_lag_seconds_bucket{le=\"0.5\"}");
        let within_two = delta("runtime_scheduler_lag_seconds_bucket{le=\"2\"}");
        let samples = metric(&window_end.body, "runtime_scheduler_samples_total")
            .unwrap_or(f64::NAN)
            - metric(initial_metrics, "runtime_scheduler_samples_total").unwrap_or(0.0);
        let drops = total_metric(&after.body, "telemetry_log_records_dropped_total")
            - total_metric(initial_metrics, "telemetry_log_records_dropped_total");
        // Histogram upkeep can lag its sampler counter by one second. Include
        // the tail through recovery in the numerator, but never let those
        // extra healthy recovery samples dilute the mixed-window denominator.
        let slow = count - within_half;
        evidence.record(json!({"event": "sampler_summary", "phase": label, "histogram_samples_including_boundary_spill": count, "mixed_samples": samples, "lag_over_500ms_including_boundary_spill": slow, "lag_le_2s": within_two, "log_drops": drops}));
        require(
            failures,
            count > 0.0 && samples > 0.0 && slow >= 0.0 && slow <= samples * 0.01,
            format!("{label}: sampler histogram p99 exceeds500ms or missing samples"),
        );
        require(
            failures,
            drops > 0.0,
            format!("{label}: actual log saturation did not produce counted drops"),
        );
    } else {
        failures.push(format!("{label}: missing before/after metric observations"));
    }
    recovery_metrics_checks(scrapes, &metrics, failures, label);
}

fn sampler_observation(completed: f64, samples: u32) -> Scrape {
    let readiness_completed = 1000.0 + completed;
    Scrape {
        started: completed - 0.01,
        completed,
        route: "/metrics",
        status: Some(200),
        body: format!(
            "runtime_scheduler_lag_seconds_count {samples}\n\
             runtime_scheduler_lag_seconds_bucket{{le=\"0.5\"}} {samples}\n\
             runtime_scheduler_lag_seconds_bucket{{le=\"2\"}} {samples}\n\
             runtime_scheduler_lag_max_seconds 0.1\n\
             runtime_scheduler_samples_total {samples}\n\
             runtime_scheduler_sample_age_seconds 0.1\n\
             readiness_stale_after_seconds 16\n\
             readiness_last_completed_timestamp_seconds {readiness_completed}\n\
             telemetry_log_records_dropped_total 1\n"
        ),
    }
}

#[test]
fn sampler_oracle_retains_late_violations_and_requires_a_fresh_final_observation() {
    let initial = sampler_observation(0.0, 100).body.replace(
        "telemetry_log_records_dropped_total 1",
        "telemetry_log_records_dropped_total 0",
    );
    let mut scrapes = Vec::new();
    for second in 0..40_u32 {
        for route in ["/health/live", "/health/ready", "/metrics"] {
            let mut scrape = sampler_observation(f64::from(second) + 0.1, 101 + second * 10);
            scrape.route = route;
            scrapes.push(scrape);
        }
    }
    let final_metrics = sampler_observation(40.1, 501);
    let check = |scrapes: &[Scrape], final_metrics: &Scrape| {
        let evidence = Evidence {
            zero: Instant::now(),
            directory: PathBuf::new(),
            events: Arc::default(),
            task_errors: Arc::default(),
        };
        let mut failures = Vec::new();
        metrics_checks(
            scrapes,
            &initial,
            final_metrics,
            &mut failures,
            &evidence,
            "oracle",
        );
        failures
    };
    let healthy = check(&scrapes, &final_metrics);
    assert!(healthy.is_empty(), "healthy control: {healthy:?}");

    // Keep the first >=32s scrape and the final scrape healthy. A later
    // retained violation must fail independently of those favorable samples.
    for (old, new, expected) in [
        (
            "runtime_scheduler_lag_max_seconds 0.1",
            "runtime_scheduler_lag_max_seconds 2",
            "strict sampler maximum <2s",
        ),
        (
            "runtime_scheduler_lag_seconds_bucket{le=\"2\"} 451",
            "runtime_scheduler_lag_seconds_bucket{le=\"2\"} 450",
            "sampler histogram exceeds2s",
        ),
    ] {
        let mut late = scrapes.clone();
        let scrape = late
            .iter_mut()
            .find(|scrape| scrape.route == "/metrics" && scrape.completed > 35.0)
            .unwrap();
        scrape.body = scrape.body.replace(old, new);
        let failures = check(&late, &final_metrics);
        assert!(
            failures.iter().any(|failure| failure.contains(expected)),
            "{failures:?}"
        );
    }

    for case in [
        "unavailable",
        "stale",
        "unknown",
        "not_resumed",
        "started_early",
    ] {
        let mut final_metrics = final_metrics.clone();
        match case {
            "unavailable" => final_metrics.status = None,
            "stale" => {
                final_metrics.body = final_metrics.body.replace(
                    "runtime_scheduler_sample_age_seconds 0.1",
                    "runtime_scheduler_sample_age_seconds 1.1",
                );
            }
            "unknown" => {
                final_metrics.body = final_metrics.body.replace(
                    "runtime_scheduler_samples_total 501",
                    "runtime_scheduler_samples_total NaN",
                );
            }
            "not_resumed" => {
                final_metrics.body = final_metrics.body.replace(
                    "runtime_scheduler_samples_total 501",
                    "runtime_scheduler_samples_total 391",
                );
            }
            "started_early" => final_metrics.started = 39.9,
            _ => unreachable!(),
        }
        let failures = check(&scrapes, &final_metrics);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("final sampler observation")),
            "{case}: {failures:?}"
        );
    }
}

async fn quiet_phase(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    rate: u32,
    seconds: u32,
) -> Vec<Completion> {
    let start = Instant::now() + Duration::from_millis(50);
    let (requests, _) = tokio::join!(
        traffic(
            container.client.clone(),
            evidence.clone(),
            label.to_owned(),
            "",
            rate,
            32,
            (start, seconds)
        ),
        observations(container, evidence, label, start, seconds),
    );
    requests
}

async fn recovery_snapshot(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    start: Instant,
    recovery_since: &Mutex<String>,
) -> Result<(Value, f64)> {
    sleep_until(start + Duration::from_secs(39)).await;
    *recovery_since.lock().unwrap() = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(failure)?
        .as_secs()
        .to_string();
    container.client.rpc("log").await.map_err(failure)?;
    let state = container
        .client
        .snapshot(evidence, &format!("{label}-recovery"))
        .await?;
    let at = Instant::now().duration_since(start).as_secs_f64();
    Ok((state, at))
}

async fn pause_stdout(container: &Container, evidence: &Evidence, start: Instant) -> Result<()> {
    sleep_until(start).await;
    reader_signal(&container.name, "STOP", evidence).await?;
    // STOP acknowledgement starts the full undrained interval.
    sleep_until(Instant::now() + Duration::from_secs(10)).await;
    reader_signal(&container.name, "CONT", evidence).await
}

struct MixedLoad {
    cheap: Vec<Completion>,
    logs: Vec<Completion>,
    scrapes: Vec<Scrape>,
    terminal: Result<(Value, f64)>,
    pressure_end: PressureEnd,
    recovery_since: Arc<Mutex<String>>,
    start: Instant,
}

struct PressureEnd {
    snapshot: Result<Value>,
    counters: Result<BTreeMap<String, u64>>,
}

async fn observe_pressure_end(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    start: Instant,
) -> PressureEnd {
    sleep_until(start + Duration::from_secs(30)).await;
    let sent = start.elapsed().as_secs_f64();
    let boundary = format!("{label}-pressure-end");
    let counters = cgroup(&container.name, &boundary, evidence).await;
    // Close the occupancy bracket after the cgroup reads too: delayed reads
    // cannot borrow a timely earlier pair and masquerade as pressure-end data.
    let snapshot = container.client.snapshot(evidence, &boundary).await;
    evidence.record(json!({"event": "pressure_boundary", "phase": label,
        "phase_request_s": sent, "phase_completion_s": start.elapsed().as_secs_f64()}));
    PressureEnd { snapshot, counters }
}

fn qualified_occupancy(before: &Value, after: &Value) -> Result<(u64, u64)> {
    fn pair(snapshot: &Value) -> Result<(u64, u64)> {
        let observed = counter(snapshot, "cpu_occupancy_observed_ns")
            .ok_or("missing integer occupancy observation time")?;
        let occupied = counter(snapshot, "cpu_occupancy_occupied_ns")
            .ok_or("missing integer occupied wall duration")?;
        if observed == 0 || occupied > observed {
            return Err("invalid cumulative occupancy pair".into());
        }
        Ok((observed, occupied))
    }
    let before = pair(before)?;
    let after = pair(after)?;
    let elapsed = after
        .0
        .checked_sub(before.0)
        .ok_or("occupancy clock decreased")?;
    let occupied = after
        .1
        .checked_sub(before.1)
        .ok_or("occupied duration decreased")?;
    if !(30_000_000_000..=32_000_000_000).contains(&elapsed) || occupied > elapsed {
        return Err(format!(
            "invalid pressure bracket: elapsed={elapsed}ns, occupied={occupied}ns"
        ));
    }
    // Both operands are bounded by 32 seconds, so this exact integer ratio
    // cannot overflow or round an interval below 90% into qualification.
    if occupied * 10 < elapsed * 9 {
        return Err(format!(
            "pressure occupancy below90%: {occupied}ns/{elapsed}ns"
        ));
    }
    Ok((elapsed, occupied))
}

#[test]
fn occupancy_qualification_keeps_exact_ratio_and_rejects_invalid_brackets() {
    let before = json!({"cpu_occupancy_observed_ns": 10_000_000_000_u64,
        "cpu_occupancy_occupied_ns": 1_000_000_000_u64});
    let after = |observed, occupied| {
        json!({"cpu_occupancy_observed_ns": observed,
        "cpu_occupancy_occupied_ns": occupied})
    };
    assert_eq!(
        qualified_occupancy(&before, &after(40_000_000_000_u64, 28_000_000_000_u64)),
        Ok((30_000_000_000, 27_000_000_000)),
    );
    for invalid in [
        Value::Null,
        after(40_000_000_000, 27_999_999_999), // One nanosecond below 90%.
        after(42_000_000_001, 33_000_000_001), // Late pressure bracket.
        after(39_999_999_999, 30_000_000_000), // Incomplete pressure window.
        after(10_000_000_000, 1_000_000_000),  // Stale pair.
        after(9_000_000_000, 1_000_000_000),   // Clock reset.
        after(40_000_000_000, 999_999_999),    // Occupancy reset.
        after(40_000_000_000, 32_000_000_000), // More work than elapsed time.
    ] {
        assert!(qualified_occupancy(&before, &invalid).is_err(), "{invalid}");
    }
}

#[tokio::test(start_paused = true)]
async fn expired_cpu_window_accounts_every_offer_without_dispatching_requests() {
    let evidence = Evidence {
        zero: Instant::now(),
        directory: PathBuf::new(),
        events: Arc::default(),
        task_errors: Arc::default(),
    };
    let client = Client {
        // Lazy transport is never polled for a request: a late offer must end
        // at the driver boundary before any RPC is attempted.
        channel: tonic::transport::Endpoint::from_static("http://127.0.0.1:9").connect_lazy(),
        token: "unused".to_owned(),
    };
    let start = Instant::now();
    tokio::time::advance(Duration::from_secs(30)).await;
    let outcomes = traffic(
        client,
        evidence.clone(),
        "expired".to_owned(),
        "cpu",
        1000,
        32,
        (start, 30),
    )
    .await;
    assert_eq!(outcomes.len(), 30_000);
    assert!(outcomes.iter().all(|outcome| !outcome.success));
    let events = evidence.events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "offer")
            .count(),
        30_000
    );
    let dropped: Vec<_> = events
        .iter()
        .filter(|event| event["event"] == "dropped_offer")
        .collect();
    assert_eq!(dropped.len(), 30_000);
    for (index, event) in dropped.iter().enumerate() {
        assert_eq!(event["index"].as_u64(), Some(u64::try_from(index).unwrap()));
        assert_eq!(event["reason"], "cpu_window_ended");
        assert!(event["scheduled_s"].as_f64().unwrap() < 30.0);
    }
    assert!(!events.iter().any(|event| event["event"] == "completion"));
}

fn pressure_checks(
    initial: &Value,
    initial_stat: &BTreeMap<String, u64>,
    pressure_end: &PressureEnd,
    failures: &mut Vec<String>,
    evidence: &Evidence,
    label: &str,
) {
    let occupancy = pressure_end
        .snapshot
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|after| qualified_occupancy(initial, after));
    evidence.record(json!({"event": "pressure_occupancy", "phase": label,
        "bracket_elapsed_and_occupied_ns": occupancy, "quantity": "occupied_wall_time"}));
    require(
        failures,
        occupancy.is_ok(),
        format!("{label}: {occupancy:?}"),
    );
    if let Ok(counters) = &pressure_end.counters {
        for field in ["nr_throttled", "throttled_usec"] {
            require(
                failures,
                counters.get(field).copied().unwrap_or(0)
                    > initial_stat.get(field).copied().unwrap_or(u64::MAX),
                format!("{label}: pressure-end cgroup {field} did not increase"),
            );
        }
    } else {
        failures.push(format!(
            "{label}: missing pressure-end cgroup observation: {:?}",
            pressure_end.counters
        ));
    }
}

async fn offer_mixed_load(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    failures: &mut Vec<String>,
) -> MixedLoad {
    let recovery_since = Arc::new(Mutex::new(String::new()));
    let start = Instant::now() + Duration::from_millis(50);
    let (cheap, cpu, upload, prepare, logs, scrapes, paused, terminal, pressure_end) = tokio::join!(
        traffic(
            container.client.clone(),
            evidence.clone(),
            label.to_owned(),
            "",
            100,
            32,
            (start, 40)
        ),
        traffic(
            container.client.clone(),
            evidence.clone(),
            label.to_owned(),
            "cpu",
            1000,
            32,
            (start, 30)
        ),
        traffic(
            container.client.clone(),
            evidence.clone(),
            label.to_owned(),
            "upload",
            4,
            2,
            (start, 30)
        ),
        traffic(
            container.client.clone(),
            evidence.clone(),
            label.to_owned(),
            "prepare",
            4,
            2,
            (start, 30)
        ),
        traffic(
            container.client.clone(),
            evidence.clone(),
            label.to_owned(),
            "log",
            1000,
            32,
            (start, 30)
        ),
        observations(container, evidence, label, start, 40),
        pause_stdout(container, evidence, start),
        recovery_snapshot(container, evidence, label, start, &recovery_since),
        observe_pressure_end(container, evidence, label, start),
    );
    // Retain all source timelines, including refused and late calls, through
    // traffic's event records. Each bounded task has finished before return.
    evidence.record(json!({"event": "source_totals", "phase": label, "cpu_scheduled_offers": 30_000, "cpu_accounted_offers": cpu.len(), "upload_offers": upload.len(), "prepare_offers": prepare.len(), "log_offers": logs.len()}));
    require(
        failures,
        cpu.len() == 30_000,
        format!("{label}: not all30000 scheduled CPU offers were accounted"),
    );
    if paused.is_err() {
        let _ = reader_signal(&container.name, "CONT", evidence).await;
    }
    require(
        failures,
        paused.is_ok(),
        format!("{label}: stdout backpressure sequence failed: {paused:?}"),
    );
    MixedLoad {
        cheap,
        logs,
        scrapes,
        terminal,
        pressure_end,
        recovery_since,
        start,
    }
}

fn cheap_progress_checks(
    cheap: &[Completion],
    baseline: &Summary,
    failures: &mut Vec<String>,
    evidence: &Evidence,
    label: &str,
) {
    let mixed = summarize(cheap, 0.0, 30.0, evidence, &format!("{label}-mixed"));
    let recovery = summarize(
        cheap,
        35.0,
        40.0,
        evidence,
        &format!("{label}-recovery-final-five"),
    );
    require(
        failures,
        mixed.goodput >= 95.0 && mixed.goodput >= 0.95 * baseline.goodput,
        format!(
            "{label}: mixed goodput {} is below95/s or95% of baseline{}",
            mixed.goodput, baseline.goodput
        ),
    );
    require(
        failures,
        mixed.p99 <= (4.0 * baseline.p99).max(0.5) && mixed.p99 <= 2.0,
        format!("{label}: mixed p99 {}s violates frozen bound", mixed.p99),
    );
    require(
        failures,
        mixed.gap < 2.0,
        format!("{label}: cheap completion gap {}s >=2s", mixed.gap),
    );
    require(
        failures,
        mixed.gap < 8.0,
        format!("{label}: unconditional R5 failure: an8s budget elapsed without a cheap success"),
    );
    require(
        failures,
        recovery.goodput >= 99.0 && recovery.p99 <= (2.0 * baseline.p99).max(0.2),
        format!(
            "{label}: recovery goodput {} or p99 {} violates frozen bound",
            recovery.goodput, recovery.p99
        ),
    );
}

async fn final_metrics_observation(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    start: Instant,
) -> Scrape {
    // All load generators and scheduled observations have reached the fixed
    // 40-second boundary and joined. Take one designated final observation;
    // a failed/stale response must not fall back to an earlier good scrape.
    let final_started = Instant::now();
    let response = http_get(&container.metrics, "/metrics").await;
    let final_metrics = Scrape {
        started: final_started.duration_since(start).as_secs_f64(),
        completed: Instant::now().duration_since(start).as_secs_f64(),
        route: "/metrics",
        status: response.as_ref().ok().map(|value| value.0),
        body: response.map_or_else(std::convert::identity, |value| value.1),
    };
    evidence.record(json!({"event": "final_metrics", "phase": label,
        "started_s": final_metrics.started,
        "completed_s": final_metrics.completed, "status": final_metrics.status,
        "body": final_metrics.body}));
    final_metrics
}

fn completed_work_checks(
    initial: &Value,
    final_state: &Value,
    final_at: f64,
    logs: &[Completion],
    failures: &mut Vec<String>,
    evidence: &Evidence,
    label: &str,
) {
    evidence.record(json!({"event": "actual_source_counts", "phase": label,
        "cpu_admitted": number(final_state, "cpu_admitted") - number(initial, "cpu_admitted"),
        "cpu_refused": number(final_state, "cpu_refused") - number(initial, "cpu_refused"),
        "upload_started": number(final_state, "upload_started") - number(initial, "upload_started"),
        "prepared": number(final_state, "prepared") - number(initial, "prepared"),
        "log_callbacks": number(final_state, "log_attempted") - number(initial, "log_attempted") - 1.0,
        "log_rpc_offers": logs.len(), "log_rpc_successes": logs.iter().filter(|call| call.success).count(),
        "recovery_log_callbacks": 1}));
    require(
        failures,
        final_at <= 40.0,
        format!("{label}: terminal accounting was observed after10s recovery"),
    );
    require(
        failures,
        number(final_state, "cpu_active") == 0.0
            && number(final_state, "cpu_occupied") == 0.0
            && counter(final_state, "cpu_max_active") == Some(1),
        format!("{label}: CPU active/cap accounting invalid"),
    );
    require(
        failures,
        number(final_state, "cpu_completed") - number(initial, "cpu_completed") >= 10.0,
        format!("{label}: fewer than10 finite CPU operations completed"),
    );
    require(
        failures,
        number(final_state, "cpu_refused") > number(initial, "cpu_refused"),
        format!("{label}: no CPU admission refusal"),
    );
    require(
        failures,
        number(final_state, "cpu_max_seconds") <= 1.0,
        format!("{label}: finite CPU operation exceeded1s"),
    );
    require(
        failures,
        work_accounted(
            final_state,
            "cpu_admitted",
            &["cpu_completed", "cpu_failed"],
        ),
        format!("{label}: admitted CPU work not fully accounted after recovery"),
    );
    require(
        failures,
        number(final_state, "cpu_failed") == 0.0,
        format!("{label}: CPU operation failed"),
    );
    require(
        failures,
        number(final_state, "upload_active") == 0.0
            && number(final_state, "upload_max_active") <= 2.0
            && number(final_state, "upload_completed") > number(initial, "upload_completed")
            && number(final_state, "upload_failed") == 0.0,
        format!("{label}: upload bound/completion failed"),
    );
    require(
        failures,
        work_accounted(
            final_state,
            "upload_started",
            &["upload_completed", "upload_failed", "upload_cancelled"],
        ),
        format!("{label}: admitted upload work not fully accounted after recovery"),
    );
    require(
        failures,
        number(final_state, "prepared") > number(initial, "prepared")
            && number(final_state, "log_attempted") > number(initial, "log_attempted"),
        format!("{label}: generic preparation/log source was not exercised"),
    );
}

async fn post_pressure_checks(
    container: &Container,
    evidence: &Evidence,
    label: &str,
    initial_stat: &BTreeMap<String, u64>,
    since: &str,
    failures: &mut Vec<String>,
) -> Result<()> {
    let final_stat = cgroup(&container.name, &format!("{label}-after"), evidence).await?;
    for field in ["nr_throttled", "throttled_usec"] {
        require(
            failures,
            final_stat.get(field).copied().unwrap_or(0)
                > initial_stat.get(field).copied().unwrap_or(u64::MAX),
            format!("{label}: cgroup {field} did not increase under mixed pressure"),
        );
    }
    let recovered_logs = docker(
        &args(&["logs", "--since", since, &container.name]),
        Duration::from_secs(5),
    )
    .await?;
    tokio::fs::write(
        evidence
            .directory
            .join(format!("{label}-recovery.stdout.jsonl")),
        &recovered_logs,
    )
    .await
    .map_err(failure)?;
    let records: Vec<_> = recovered_logs
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect();
    require(
        failures,
        records.iter().all(std::result::Result::is_ok)
            && records.iter().any(|record| {
                record
                    .as_ref()
                    .is_ok_and(|value| value["message"] == "runtime_progress_pressure")
            }),
        format!("{label}: valid log records did not recover after stdout resumed"),
    );
    Ok(())
}

async fn mixed_sequence(
    container: &Container,
    evidence: &Evidence,
    run: u32,
    failures: &mut Vec<String>,
) -> Result<()> {
    let label = format!("run-{run}");
    let _ = quiet_phase(container, evidence, &format!("{label}-warmup"), 100, 5).await;
    let baseline = quiet_phase(container, evidence, &format!("{label}-baseline"), 100, 30).await;
    let baseline = summarize(&baseline, 0.0, 30.0, evidence, &format!("{label}-baseline"));
    let cancellation = cancel_active_waiter(&container.client, evidence).await;
    require(
        failures,
        cancellation.is_ok(),
        format!("{label}: cancellation custody: {cancellation:?}"),
    );
    let initial_stat = cgroup(&container.name, &format!("{label}-before"), evidence).await?;
    let initial_metrics = http_get(&container.metrics, "/metrics").await?.1;
    evidence
        .record(json!({"event": "metrics_before_mixed", "phase": label, "body": initial_metrics}));
    // This paired occupancy observation is the last external operation before
    // mixed scheduling; setup reads cannot pad its measured pressure bracket.
    let initial = container
        .client
        .snapshot(evidence, &format!("{label}-before"))
        .await?;
    let MixedLoad {
        cheap,
        logs,
        scrapes,
        terminal,
        pressure_end,
        recovery_since,
        start,
    } = offer_mixed_load(container, evidence, &label, failures).await;
    pressure_checks(
        &initial,
        &initial_stat,
        &pressure_end,
        failures,
        evidence,
        &label,
    );
    cheap_progress_checks(&cheap, &baseline, failures, evidence, &label);
    let final_metrics = final_metrics_observation(container, evidence, &label, start).await;
    metrics_checks(
        &scrapes,
        &initial_metrics,
        &final_metrics,
        failures,
        evidence,
        &label,
    );
    let (final_state, final_at) = terminal?;
    completed_work_checks(
        &initial,
        &final_state,
        final_at,
        &logs,
        failures,
        evidence,
        &label,
    );
    let since = recovery_since.lock().unwrap().clone();
    post_pressure_checks(container, evidence, &label, &initial_stat, &since, failures).await?;
    evidence.save(&format!("{label}.jsonl")).await
}

async fn capture_logs(name: &str, directory: &Path) -> Result<()> {
    let out = tokio::fs::File::create(directory.join(format!("{name}.stdout.jsonl")))
        .await
        .map_err(failure)?
        .into_std()
        .await;
    let err = tokio::fs::File::create(directory.join(format!("{name}.stderr.txt")))
        .await
        .map_err(failure)?
        .into_std()
        .await;
    let mut child = Command::new("docker")
        .args(["logs", name])
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .kill_on_drop(true)
        .spawn()
        .map_err(failure)?;
    if let Ok(status) = timeout(Duration::from_secs(20), child.wait()).await {
        if status.map_err(failure)?.success() {
            Ok(())
        } else {
            Err("Docker log capture failed".into())
        }
    } else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        Err("Docker log capture exceeded20s".into())
    }
}

async fn stop_case(
    name: &str,
    expected: i32,
    signal: bool,
    evidence: &Evidence,
    failures: &mut Vec<String>,
) -> Result<()> {
    let start = Instant::now();
    if signal {
        docker(
            &args(&["kill", "--signal", "TERM", name]),
            Duration::from_secs(5),
        )
        .await?;
    }
    let waited = docker(
        &args(&["wait", name]),
        Duration::from_secs(46).saturating_sub(start.elapsed()),
    )
    .await;
    let elapsed = start.elapsed().as_secs_f64();
    evidence.record(json!({"event": "process_exit", "container": name, "expected": expected, "result": waited, "signal_to_wait_s": elapsed, "external_tolerance_s": 1}));
    require(
        failures,
        waited
            .as_ref()
            .is_ok_and(|value| value.trim().parse::<i32>() == Ok(expected))
            && elapsed <= 46.0,
        format!("{name}: expected exit{expected} within46s, observed{waited:?} after{elapsed}s"),
    );
    if waited.is_err() {
        let _ = docker(&args(&["kill", name]), Duration::from_secs(5)).await;
    }
    capture_logs(name, &evidence.directory).await
}

async fn negative_control(
    container: &Container,
    evidence: &Evidence,
    failures: &mut Vec<String>,
) -> Result<()> {
    let start = Instant::now() + Duration::from_millis(50);
    let before = http_get(&container.metrics, "/metrics").await?.1;
    let (cheap, scrapes, (), cutoff) = tokio::join!(
        traffic(
            container.client.clone(),
            evidence.clone(),
            "negative-control".into(),
            "",
            100,
            32,
            (start, 12)
        ),
        observations(container, evidence, "negative-control", start, 12),
        async {
            sleep_until(start + Duration::from_millis(250)).await;
            let result = container.client.rpc("freeze").await;
            evidence
                .record(json!({"event": "negative_freeze_rpc", "result": format!("{result:?}")}));
        },
        async {
            // Absolute external cutoff cannot be delayed by the container's sole
            // frozen worker. Kill the owned process if it has not resumed.
            sleep_until(start + Duration::from_secs(12)).await;
            docker(&args(&["kill", &container.name]), Duration::from_secs(5)).await
        },
    );
    require(
        failures,
        cutoff.is_ok(),
        format!("negative control external cutoff failed: {cutoff:?}"),
    );
    let result = summarize(&cheap, 0.0, 12.0, evidence, "negative-control");
    require(
        failures,
        result.gap > 8.0,
        format!(
            "negative control failed to reveal an>8s cheap request gap: {}",
            result.gap
        ),
    );
    let after = scrapes
        .iter()
        .filter(|scrape| scrape.route == "/metrics" && scrape.status == Some(200))
        .max_by(|a, b| a.completed.total_cmp(&b.completed));
    let old_over8 = metric(&before, "runtime_scheduler_lag_seconds_count").unwrap_or(0.0)
        - metric(&before, "runtime_scheduler_lag_seconds_bucket{le=\"8\"}").unwrap_or(0.0);
    let missed = after.is_some_and(|scrape| {
        metric(&scrape.body, "runtime_scheduler_lag_seconds_count").unwrap_or(f64::NAN)
            - metric(
                &scrape.body,
                "runtime_scheduler_lag_seconds_bucket{le=\"8\"}",
            )
            .unwrap_or(f64::NAN)
            > old_over8
    });
    require(
        failures,
        missed,
        "negative control did not retain a missed timer observation on resumption",
    );
    evidence.save("negative-control.jsonl").await
}

async fn experiment(
    image: &str,
    prefix: &str,
    peers: &Peers,
    evidence: &Evidence,
    failures: &mut Vec<String>,
) -> Result<()> {
    let ordinary = launch(image, &format!("{prefix}-ordinary"), peers, evidence, false)
        .await?
        .ok_or("missing ordinary container")?;
    let state = ordinary.client.snapshot(evidence, "initial").await?;
    require(
        failures,
        counter(&state, "runtime_workers") == Some(1) && counter(&state, "cpu_permits") == Some(1),
        "effective runtime workers/CPU permit are not1",
    );
    let _ = cgroup(&ordinary.name, "qualification-before", evidence).await?;
    let _ = quiet_phase(&ordinary, evidence, "qualification-warmup", 200, 5).await;
    let qualified = quiet_phase(&ordinary, evidence, "qualification", 200, 30).await;
    let qualified = summarize(&qualified, 0.0, 30.0, evidence, "qualification");
    let capacity_met = qualified.offered == 6000
        && qualified.succeeded >= 5940
        && qualified.p99 <= 0.2
        && qualified.gap < 1.0;
    evidence.save("qualification.jsonl").await?;
    if !capacity_met {
        return Err("capacity precondition failed; retained qualification; reopen Design before changing the experiment".into());
    }
    for run in 1..=3 {
        mixed_sequence(&ordinary, evidence, run, failures).await?;
    }
    stop_case(&ordinary.name, 0, true, evidence, failures).await?;
    evidence.save("ordinary-exit.jsonl").await?;

    let blocked = launch(image, &format!("{prefix}-blocked"), peers, evidence, false)
        .await?
        .ok_or("missing blocked container")?;
    reader_signal(&blocked.name, "STOP", evidence).await?;
    let start = Instant::now();
    let _ = traffic(
        blocked.client.clone(),
        evidence.clone(),
        "blocked-final-output".into(),
        "log",
        1000,
        32,
        (start, 2),
    )
    .await;
    stop_case(&blocked.name, 3, true, evidence, failures).await?;
    evidence.save("blocked-exit.jsonl").await?;

    let primary_name = format!("{prefix}-primary");
    let _ = launch(image, &primary_name, peers, evidence, true).await?;
    stop_case(&primary_name, 1, false, evidence, failures).await?;
    evidence.save("primary-exit.jsonl").await?;

    let negative = launch(image, &format!("{prefix}-negative"), peers, evidence, false)
        .await?
        .ok_or("missing negative container")?;
    negative_control(&negative, evidence, failures).await?;
    stop_case(&negative.name, 137, false, evidence, failures).await?;
    Ok(())
}

fn pinned_image(inspect: &str, source: &str) -> Result<String> {
    let value: Value = serde_json::from_str(inspect).map_err(failure)?;
    let inspected = value
        .as_array()
        .and_then(|values| values.first())
        .ok_or("missing Docker image inspection")?;
    let revision = inspected["Config"]["Labels"]["org.opencontainers.image.revision"]
        .as_str()
        .ok_or("release image must carry org.opencontainers.image.revision")?;
    if source.is_empty() || revision != source {
        return Err(format!(
            "image source label {revision:?} differs from declared source {source:?}"
        ));
    }
    let image = inspected["Id"]
        .as_str()
        .ok_or("missing immutable Docker image ID")?;
    if !image.starts_with("sha256:") {
        return Err("Docker did not return an immutable image ID".into());
    }
    if inspected["Os"] != "linux" {
        return Err("quota fixture image is not Linux".into());
    }
    Ok(image.to_owned())
}

async fn cleanup_containers(prefix: &str, evidence: &Evidence, failures: &mut Vec<String>) {
    for suffix in ["ordinary", "blocked", "primary", "negative"] {
        let name = format!("{prefix}-{suffix}");
        let state = docker(
            &args(&["inspect", "--format", "{{.State.Running}}", &name]),
            Duration::from_secs(5),
        )
        .await;
        match state {
            Ok(state) => {
                if state.trim() == "true" {
                    let killed = docker(&args(&["kill", &name]), Duration::from_secs(5)).await;
                    let waited = docker(&args(&["wait", &name]), Duration::from_secs(5)).await;
                    evidence.record(json!({"event": "cleanup_kill_wait", "container": name, "kill": killed, "wait": waited}));
                    require(
                        failures,
                        killed.is_ok() && waited.is_ok(),
                        format!("could not kill and wait owned container {name}"),
                    );
                }
                let captured = capture_logs(&name, &evidence.directory).await;
                require(
                    failures,
                    captured.is_ok(),
                    format!("could not retain final logs for {name}: {captured:?}"),
                );
                let removed = docker(&args(&["rm", &name]), Duration::from_secs(10)).await;
                evidence.record(
                    json!({"event": "container_cleanup", "container": name, "result": removed}),
                );
                require(
                    failures,
                    removed.is_ok(),
                    format!("could not remove owned container {name}: {removed:?}"),
                );
            }
            Err(error) if error.contains("No such object") => {}
            Err(error) => failures.push(format!("cleanup could not inspect {name}: {error}")),
        }
    }
}

/// Owns the real quota boundary that source and paused-clock tests cannot see.
/// A worker polling monopoly, early permit release, stale zero-lag report, or
/// logging sink wait changes the external result despite passing unit tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit release-image R5 proof; needs Docker cgroup v2, image/source/results inputs; about six minutes"]
async fn bounded_sources_preserve_process_progress_under_one_cpu_quota() {
    let image = std::env::var("RUNTIME_PROGRESS_IMAGE")
        .expect("set RUNTIME_PROGRESS_IMAGE to the single Linux release image");
    let source = std::env::var("RUNTIME_PROGRESS_SOURCE")
        .expect("set RUNTIME_PROGRESS_SOURCE to the immutable build source identity");
    let directory = PathBuf::from(
        std::env::var("RUNTIME_PROGRESS_RESULTS")
            .expect("set RUNTIME_PROGRESS_RESULTS to a new evidence directory"),
    );
    tokio::fs::create_dir(&directory)
        .await
        .expect("results directory must not already exist; retain prior runs");
    let directory = tokio::fs::canonicalize(directory)
        .await
        .expect("absolute results path");
    let evidence = Evidence {
        zero: Instant::now(),
        directory,
        events: Arc::default(),
        task_errors: Arc::default(),
    };
    let prefix = format!(
        "runtime-progress-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    );
    let identity = docker(
        &args(&["image", "inspect", &image]),
        Duration::from_secs(10),
    )
    .await;
    evidence.record(json!({"event": "identity", "image": image, "source": source, "image_inspect": identity, "quota_us": 100_000, "period_us": 100_000, "workers": 1, "cheap_bytes": CHEAP_BODY.len(), "request_timeout_s": 8, "drain_s": 25, "grace_s": 45, "readiness_interval_s": 2, "readiness_probe_budget_s": 4, "readiness_stale_s": 16}));
    let pinned = identity
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|inspect| pinned_image(inspect, &source));
    let mut failures = Vec::new();
    let peers = Peers::start(&evidence).await;
    if let Ok(peers) = peers {
        // Cleanup and evidence retention also run after an assertion inside a
        // fixture helper; no Tokio child or external peer survives the test.
        let result = std::panic::AssertUnwindSafe(async {
            let pinned = pinned?;
            experiment(&pinned, &prefix, &peers, &evidence, &mut failures).await
        })
        .catch_unwind()
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(error),
            Err(_) => failures.push("driver panicked; partial evidence retained".into()),
        }
        cleanup_containers(&prefix, &evidence, &mut failures).await;
        peers.finish().await;
    } else if let Err(error) = peers {
        failures.push(error);
    }
    require(
        &mut failures,
        identity.is_ok(),
        "image inspection did not establish image identity",
    );
    failures.extend(evidence.task_errors.lock().unwrap().iter().cloned());
    evidence.record(json!({"event": "verdict", "failures": failures, "outcome": if failures.is_empty() { "passed" } else { "failed" }}));
    evidence
        .save("final.jsonl")
        .await
        .expect("persist failure evidence before assertion");
    assert!(
        failures.is_empty(),
        "R5 quota proof failed; retain {} and reopen the named owner: {failures:#?}",
        evidence.directory.display()
    );
}
