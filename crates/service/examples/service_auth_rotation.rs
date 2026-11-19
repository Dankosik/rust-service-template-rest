#![expect(clippy::print_stdout, reason = "the drill prints its operator receipt")]

use std::error::Error;
use std::io::ErrorKind;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use axum::http::{HeaderValue, Uri};
use grpc_contracts::example::v1::{UnaryRequest, echo_service_client::EchoServiceClient};
use infra_serviceauth::{
    JwtType, KeyId, ServiceName, SigningOptions, TokenSource, TrustRegistry, Verifier,
    test_support::{TestKey, registry},
};
use tls_test_support::TlsMaterial;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Request};

#[path = "../tests/common/mod.rs"]
mod common;
#[path = "grpc.rs"]
mod grpc;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const CALLER: &str = "example-caller";
const RECEIVER: &str = "catalog";
const JWT_TYPE: &str = "example-svc+jwt";
const CHILD: &str = "SERVICE_AUTH_ROTATION_RECEIVER";
const STARTUP: Duration = Duration::from_secs(20);
const RPC: Duration = Duration::from_secs(5);

struct Keys {
    old: TokenSource,
    new: TokenSource,
    old_trust: String,
    overlap: String,
    new_trust: String,
    control: Verifier,
}

fn source(key: &TestKey) -> Result<TokenSource> {
    Ok(TokenSource::try_new(SigningOptions {
        name: ServiceName::parse(CALLER)?,
        jwt_type: JwtType::parse(JWT_TYPE)?,
        key_id: KeyId::parse(key.kid())?,
        private_key: key.pem().into(),
    })?)
}

fn verifier(trust: &str) -> Result<Verifier> {
    Ok(Verifier::new(
        ServiceName::parse(RECEIVER)?,
        JwtType::parse(JWT_TYPE)?,
        TrustRegistry::parse(trust)?,
    ))
}

impl Keys {
    fn generate() -> Result<Self> {
        let old = TestKey::new("old");
        let new = TestKey::new("new");
        let overlap = registry(&[(CALLER, &[&old, &new])]);
        Ok(Self {
            old: source(&old)?,
            new: source(&new)?,
            old_trust: registry(&[(CALLER, &[&old])]),
            new_trust: registry(&[(CALLER, &[&new])]),
            control: verifier(&overlap)?,
            overlap,
        })
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "the local drill owns the receiver process"
)]
struct Process(Child);

#[expect(
    clippy::disallowed_methods,
    reason = "reap the owned child on every return path"
)]
impl Drop for Process {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Receiver {
    process: Process,
    uri: Uri,
    client: EchoServiceClient<Channel>,
    _stdout: mpsc::Receiver<String>,
}

impl Receiver {
    #[expect(
        clippy::disallowed_types,
        clippy::disallowed_methods,
        reason = "the local drill starts an independent receiver for each replica"
    )]
    async fn start(tls: &TlsMaterial, trust: &str) -> Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        let mut process = Process(
            common::baseline_env(&mut command)
                .env(CHILD, "1")
                .env("APP__APP__ENV", "local")
                .env("APP__RUNTIME__WORKER_THREADS", "1")
                .env("APP__GRPC__ENABLED", "true")
                .env("APP__GRPC__ADDR", "127.0.0.1:0")
                .env("APP__GRPC__SECURITY", "tls")
                .env("APP__GRPC__CERTIFICATE", &tls.cert_pem)
                .env("APP__GRPC__PRIVATE_KEY", &tls.key_pem)
                .env("APP__SERVICE_AUTH__MODE", "jwt")
                .env("APP__SERVICE_AUTH__NAME", RECEIVER)
                .env("APP__SERVICE_AUTH__JWT_TYPE", JWT_TYPE)
                .env("APP__SERVICE_AUTH__TRUST", trust)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let (lines, _reader) = common::spawn_stdout_lines(&mut process.0);
        let deadline = Instant::now() + STARTUP;
        let mut http = None;
        let mut grpc = None;
        loop {
            let line = lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| "receiver did not report readiness")?;
            let record: serde_json::Value = serde_json::from_str(&line)?;
            match record["message"].as_str() {
                Some("http listener bound") => http = record["addr"].as_str().map(str::to_owned),
                Some("grpc listener bound") => grpc = record["addr"].as_str().map(str::to_owned),
                Some("service_ready") => break,
                _ => {}
            }
        }
        let http = http.ok_or("receiver omitted its HTTP address")?;
        if common::poll_until(&format!("http://{http}/health/ready"), 200, RPC).is_none() {
            return Err("receiver failed HTTP readiness".into());
        }
        let uri: Uri = format!(
            "https://{}",
            grpc.ok_or("receiver omitted its gRPC address")?
        )
        .parse()?;
        let channel = Endpoint::from(uri.clone())
            .connect_timeout(RPC)
            .timeout(RPC)
            .tls_config(
                ClientTlsConfig::new().ca_certificate(Certificate::from_pem(&tls.root_pem)),
            )?
            .connect()
            .await?;
        Ok(Self {
            process,
            uri,
            client: EchoServiceClient::new(channel),
            _stdout: lines,
        })
    }

    fn fresh(&self, source: &TokenSource) -> Result<HeaderValue> {
        source
            .receiver(RECEIVER, self.uri.clone())?
            .authorization()?
            .ok_or_else(|| "signer omitted its credential".into())
    }

    async fn probe(&mut self, token: &HeaderValue, control: &Verifier, want: Code) -> Result<()> {
        control.authenticate([token.as_bytes()])?;
        let mut request = Request::new(UnaryRequest {
            message: "rotation".to_owned(),
        });
        let mut authorization = token
            .to_str()?
            .parse::<tonic::metadata::AsciiMetadataValue>()?;
        authorization.set_sensitive(true);
        request
            .metadata_mut()
            .insert("authorization", authorization);
        let observed = match timeout(RPC, self.client.unary(request)).await? {
            Ok(reply) if reply.get_ref().message == "rotation" => Code::Ok,
            Ok(_) => return Err("unexpected authenticated Echo response".into()),
            Err(status) => status.code(),
        };
        control.authenticate([token.as_bytes()])?;
        if observed != want {
            return Err(format!("{} returned {observed:?}, expected {want:?}", self.uri).into());
        }
        Ok(())
    }

    async fn verify_isolated(&mut self, token: &HeaderValue, control: &Verifier) -> Result<()> {
        self.probe(token, control, Code::Unavailable).await?;
        let addr = self.uri.authority().ok_or("missing receiver address")?;
        match timeout(RPC, TcpStream::connect(addr.as_str())).await? {
            Err(err) if err.kind() == ErrorKind::ConnectionRefused => Ok(()),
            Err(err) => Err(err.into()),
            Ok(_) => Err(format!("isolated endpoint {addr} still accepts connections").into()),
        }
    }

    async fn retire(self) -> Result<()> {
        let Self {
            mut process,
            client,
            ..
        } = self;
        drop(client);
        common::terminate(&process.0);
        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            if let Some(status) = process.0.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("receiver exited with {status}").into())
                };
            }
            if Instant::now() >= deadline {
                return Err("receiver did not finish draining".into());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RotationMode {
    Planned,
    Emergency,
}

async fn drill(mode: RotationMode) -> Result<()> {
    let keys = Keys::generate()?;
    let tls = TlsMaterial::new("127.0.0.1");
    let mut fleet = Vec::new();
    let initial_trust = if mode == RotationMode::Emergency {
        &keys.overlap
    } else {
        &keys.old_trust
    };
    for _ in 0..3 {
        let mut server = Receiver::start(&tls, initial_trust).await?;
        let warm = server.fresh(&keys.old)?;
        server.probe(&warm, &keys.control, Code::Ok).await?;
        server.probe(&warm, &keys.control, Code::Ok).await?;
        let new = server.fresh(&keys.new)?;
        server
            .probe(
                &new,
                &keys.control,
                if mode == RotationMode::Emergency {
                    Code::Ok
                } else {
                    Code::Unauthenticated
                },
            )
            .await?;
        fleet.push((server, warm));
    }

    if mode == RotationMode::Emergency {
        println!(
            "emergency baseline: three independent receivers admitted the same old token twice per receiver and admitted standby"
        );
    } else {
        println!("planned baseline: all three receivers admitted old and rejected new");
        for (server, warm) in &mut fleet {
            let mut replacement = Receiver::start(&tls, &keys.overlap).await?;
            replacement.probe(warm, &keys.control, Code::Ok).await?;
            std::mem::replace(server, replacement).retire().await?;
        }
        for (server, warm) in &mut fleet {
            let new = server.fresh(&keys.new)?;
            server.probe(&new, &keys.control, Code::Ok).await?;
            server.probe(warm, &keys.control, Code::Ok).await?;
        }
        println!(
            "planned cutover: new signer admitted 3/3; all previously issued old credentials still admitted; sole synthetic caller switched and all its RPCs completed"
        );
    }

    let started = Instant::now();
    for (index, (server, warm)) in fleet.iter_mut().enumerate() {
        let mut replacement = Receiver::start(&tls, &keys.new_trust).await?;
        let old = server.fresh(&keys.old)?;
        server.probe(&old, &keys.control, Code::Ok).await?;
        replacement
            .probe(&old, &keys.control, Code::Unauthenticated)
            .await?;
        let new = replacement.fresh(&keys.new)?;
        replacement.probe(&new, &keys.control, Code::Ok).await?;
        std::mem::replace(server, replacement).retire().await?;
        server
            .probe(warm, &keys.control, Code::Unauthenticated)
            .await?;
        let old = server.fresh(&keys.old)?;
        server
            .probe(&old, &keys.control, Code::Unauthenticated)
            .await?;
        let new = server.fresh(&keys.new)?;
        server.probe(&new, &keys.control, Code::Ok).await?;
        if mode == RotationMode::Emergency {
            println!(
                "emergency replica {}: stale process accepted same fresh old token rejected by replacement; stale process exited; warm and fresh old UNAUTHENTICATED, new OK; elapsed_upper_us={}",
                index + 1,
                started.elapsed().as_micros().saturating_add(1)
            );
        }
    }
    for (server, _) in &mut fleet {
        let old = server.fresh(&keys.old)?;
        server
            .probe(&old, &keys.control, Code::Unauthenticated)
            .await?;
        let new = server.fresh(&keys.new)?;
        server.probe(&new, &keys.control, Code::Ok).await?;
    }
    if mode == RotationMode::Emergency {
        println!(
            "emergency fleet: 3/3 old processes exited, 3/3 live receivers rejected fresh old and admitted new; elapsed_upper_us={}",
            started.elapsed().as_micros().saturating_add(1)
        );
    } else {
        println!(
            "planned retirement: 3/3 old processes exited, 3/3 replacements rejected warm and fresh old and admitted new"
        );
    }
    for (server, _) in fleet {
        server.retire().await?;
    }
    Ok(())
}

async fn cold_emergency() -> Result<()> {
    let tls = TlsMaterial::new("127.0.0.1");
    let old = TestKey::new("old");
    let old_signer = source(&old)?;
    let old_trust = registry(&[(CALLER, &[&old])]);
    let old_control = verifier(&old_trust)?;
    let mut old_receivers = Vec::new();
    for _ in 0..3 {
        let mut server = Receiver::start(&tls, &old_trust).await?;
        let token = server.fresh(&old_signer)?;
        server.probe(&token, &old_control, Code::Ok).await?;
        old_receivers.push((server, token));
    }
    println!("cold baseline: 3/3 receivers admitted old, replacement key not yet generated");
    let withdrawal = Instant::now();
    for (server, _) in &mut old_receivers {
        server.process.0.kill()?;
    }
    for (server, _) in &mut old_receivers {
        loop {
            if let Some(status) = server.process.0.try_wait()? {
                if status.signal() != Some(nix::libc::SIGKILL) {
                    return Err(format!("{} exited unexpectedly with {status}", server.uri).into());
                }
                break;
            }
            if withdrawal.elapsed() >= RPC {
                return Err(
                    format!("{} did not exit after emergency isolation", server.uri).into(),
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    for (server, token) in &mut old_receivers {
        server.verify_isolated(token, &old_control).await?;
    }
    let containment_upper_us = withdrawal.elapsed().as_micros().saturating_add(1);
    println!(
        "cold containment: 3/3 old processes exited, old RPCs UNAVAILABLE on established channels and 3/3 direct endpoints refused connections, 0/3 serving; containment_upper_us={containment_upper_us}"
    );
    drop(old_receivers);

    let new = TestKey::new("replacement");
    let new_signer = source(&new)?;
    let new_trust = registry(&[(CALLER, &[&new])]);
    let new_control = verifier(&new_trust)?;
    let mut replacements = Vec::new();
    for _ in 0..3 {
        replacements.push(Receiver::start(&tls, &new_trust).await?);
    }
    for server in &mut replacements {
        let old = server.fresh(&old_signer)?;
        server
            .probe(&old, &old_control, Code::Unauthenticated)
            .await?;
        let new = server.fresh(&new_signer)?;
        server.probe(&new, &new_control, Code::Ok).await?;
    }
    let restoration_upper_us = withdrawal.elapsed().as_micros().saturating_add(1);
    println!(
        "cold restoration: 3/3 healthy replacements rejected fresh old (UNAUTHENTICATED) and admitted replacement (OK); restoration_upper_us={restoration_upper_us}"
    );
    for server in replacements {
        server.retire().await?;
    }
    Ok(())
}

fn main() -> Result<ExitCode> {
    if std::env::var_os(CHILD).is_some() {
        return Ok(grpc::main());
    }
    let mut args = std::env::args_os().skip(1);
    let cold_only = match (args.next(), args.next()) {
        (None, None) => false,
        (Some(arg), None) if arg == "--cold" => true,
        _ => return Err("usage: service_auth_rotation [--cold]".into()),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::select! {
            result = async {
                if !cold_only {
                    drill(RotationMode::Planned).await?;
                    drill(RotationMode::Emergency).await?;
                }
                cold_emergency().await
            } => result,
            signal = tokio::signal::ctrl_c() => { signal?; Err("drill interrupted".into()) }
        }
    })?;
    Ok(ExitCode::SUCCESS)
}
