mod credentials;
mod grpc;
mod outbound_http;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Config {
    scenario: String,
    count: usize,
    concurrency: usize,
    warmup: usize,
    http_port: u16,
    control_port: u16,
    grpc_port: u16,
    ca: PathBuf,
    ttl: u64,
    telemetry: String,
    shape: String,
    hold_ms: u64,
    bind: String,
    workers: usize,
}

impl Config {
    fn parse() -> Result<Self> {
        let mut arguments = std::env::args().skip(1);
        let mut values = BTreeMap::new();
        while let Some(key) = arguments.next() {
            if !key.starts_with("--") || values.contains_key(&key) {
                return Err(format!("unexpected argument {key}").into());
            }
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {key}"))?;
            values.insert(key, value);
        }
        let mut take =
            |key: &str, default: &str| values.remove(key).unwrap_or_else(|| default.to_owned());
        let config = Self {
            scenario: take("--scenario", ""),
            count: take("--count", "2048").parse()?,
            concurrency: take("--concurrency", "1").parse()?,
            warmup: take("--warmup", "256").parse()?,
            http_port: take("--http-port", "8443").parse()?,
            control_port: take("--control-port", "8445").parse()?,
            grpc_port: take("--grpc-port", "8444").parse()?,
            ca: take("--ca", "ca.pem").into(),
            ttl: take("--ttl", "30").parse()?,
            telemetry: take("--telemetry", "absent"),
            shape: take("--shape", "default"),
            hold_ms: take("--hold-ms", "1000").parse()?,
            bind: take("--bind", "127.0.0.1"),
            workers: take("--workers", "4").parse()?,
        };
        let ip: IpAddr = config.bind.parse()?;
        if !values.is_empty()
            || !ip.is_loopback()
            || config.concurrency == 0
            || config.count < config.concurrency
            || config.warmup < config.concurrency
            || config.workers == 0
        {
            return Err("invalid arguments, loopback address, or call population".into());
        }
        Ok(config)
    }

    fn authority(&self, port: u16) -> String {
        if self.bind.contains(':') {
            format!("[{}]:{port}", self.bind)
        } else {
            format!("{}:{port}", self.bind)
        }
    }
}

#[derive(Serialize)]
struct Observation {
    outcome: &'static str,
    status: Option<u16>,
    bytes: usize,
    checksum: u64,
}

impl Observation {
    fn success(status: u16, body: &[u8]) -> Self {
        Self {
            outcome: "successful",
            status: Some(status),
            bytes: body.len(),
            checksum: body.first().zip(body.last()).map_or(0, |(&first, &last)| {
                (u64::from(first) << 8) | u64::from(last)
            }),
        }
    }

    fn failure(outcome: &'static str, status: Option<u16>) -> Self {
        Self {
            outcome,
            status,
            bytes: 0,
            checksum: 0,
        }
    }
}

#[derive(Serialize)]
struct Sample {
    index: usize,
    latency_ns: u128,
    #[serde(flatten)]
    observation: Observation,
}

fn process_cpu_ns() -> Result<u64> {
    use rustix::time::{ClockId, clock_gettime};
    let clock = clock_gettime(ClockId::ProcessCPUTime);
    Ok(u64::try_from(clock.tv_sec)? * 1_000_000_000 + u64::try_from(clock.tv_nsec)?)
}

async fn measure_with<S, F, Fut>(
    count: usize,
    concurrency: usize,
    state: S,
    operation: F,
) -> Result<Value>
where
    S: Clone + Send + 'static,
    F: Fn(S, usize) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = (S, Observation)> + Send + 'static,
{
    let ready = Arc::new(Barrier::new(concurrency + 1));
    let start = Arc::new(Barrier::new(concurrency + 1));
    let mut workers = JoinSet::new();
    for worker in 0..concurrency {
        let ready = Arc::clone(&ready);
        let start = Arc::clone(&start);
        let operation = operation.clone();
        let mut state = state.clone();
        workers.spawn(async move {
            let indices = (worker..count).step_by(concurrency);
            let mut samples = Vec::with_capacity(indices.len());
            ready.wait().await;
            start.wait().await;
            for index in indices {
                let began = Instant::now();
                let (next, observation) = operation(state, index).await;
                samples.push(Sample {
                    index,
                    latency_ns: began.elapsed().as_nanos(),
                    observation,
                });
                state = next;
            }
            (state, samples)
        });
    }
    let mut completed = Vec::with_capacity(concurrency);
    ready.wait().await;
    let cpu_start = process_cpu_ns()?;
    let wall_start = Instant::now();
    start.wait().await;
    while let Some(result) = workers.join_next().await {
        completed.push(result?);
    }
    let wall_ns = wall_start.elapsed().as_nanos();
    let cpu_ns = process_cpu_ns()? - cpu_start;
    let mut samples: Vec<_> = completed
        .into_iter()
        .flat_map(|(_, samples)| samples)
        .collect();
    samples.sort_unstable_by_key(|sample| sample.index);
    Ok(json!({
        "offered": count,
        "completed": samples.len(),
        "cpu_ns": cpu_ns,
        "wall_ns": wall_ns,
        "samples": samples,
    }))
}

fn control(config: &Config, method: &str, path: &str) -> Result<Value> {
    let host = SocketAddr::new(config.bind.parse()?, config.control_port);
    let output = Command::new("curl")
        .args([
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            "--request",
            method,
        ])
        .arg(format!("http://{host}{path}"))
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "fixture control {method} {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn run(config: &Config) -> Result<Value> {
    let mut result = match config.scenario.as_str() {
        "service-before" | "service-refresh" | "service-demand" | "exchange" => {
            credentials::run(config).await?
        }
        "grpc-transport" | "grpc-decoded" => grpc::run(config).await?,
        "http-buffered" | "http-headers" | "http-retain" => outbound_http::run(config).await?,
        _ => return Err(format!("unknown scenario {}", config.scenario).into()),
    };
    if let Some(window) = result
        .as_object_mut()
        .and_then(|object| object.remove("window"))
    {
        result["windows"] = json!([window]);
    }
    result["scenario"] = json!(config.scenario);
    result["shape"] = json!(config.shape);
    result["telemetry"] = json!(config.telemetry);
    result["warmup"] = json!(config.warmup);
    result["concurrency"] = json!(config.concurrency);
    result["worker_threads"] = json!(config.workers);
    result["caller_pid"] = json!(std::process::id());
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status")?;
        result["caller_effective_cpus"] = json!(
            status
                .lines()
                .find_map(|line| { line.strip_prefix("Cpus_allowed_list:").map(str::trim) })
        );
    }
    Ok(result)
}

fn main() -> Result<()> {
    let config = Config::parse()?;
    if std::env::var_os("SSL_CERT_FILE").as_deref() != Some(config.ca.as_os_str()) {
        return Err("SSL_CERT_FILE must name the fixture CA at process startup".into());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.workers)
        .enable_all()
        .build()?;
    println!("{}", runtime.block_on(run(&config))?);
    Ok(())
}
