use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use infra_outbound_http::{Bytes, Client, Error, ExchangeError, Limits, Method, Request, Url};
use operation_context::{Deadline, OperationContext};
use serde_json::{Value, json};

use crate::{Config, Observation, Result, control, measure_with};

const BODY: Bytes = Bytes::from_static(b"outbound experiment");

#[derive(Clone)]
struct Workload {
    client: Client,
    canonical: http::Uri,
    alternate: http::Uri,
    method: Method,
    body: Bytes,
    caller_host: bool,
    mixed: bool,
    headers_only: bool,
    retry_probe: bool,
}

impl Workload {
    fn new(config: &Config) -> Result<Self> {
        let authority = match config.shape.as_str() {
            "canonical" | "alternate" | "mixed" | "unstarted-retry" => {
                format!("localhost:{}", config.http_port)
            }
            _ => config.authority(config.http_port),
        };
        let origin = Url::parse(&format!("https://{authority}"))?;
        let mut alternate_authority = authority.to_ascii_uppercase();
        if config.shape == "bad-port" {
            alternate_authority = config.authority(if config.http_port == 1 { 2 } else { 1 });
        }
        if config.shape == "bad-origin" {
            alternate_authority = format!("127.0.0.2:{}", config.http_port);
        }
        if config.shape == "userinfo" {
            alternate_authority = format!("synthetic@{authority}");
        }
        let resource = match config.shape.as_str() {
            "empty-post" | "empty-put" | "empty-patch" => "empty",
            "empty" | "one" | "multi" | "tiny" | "large" | "late-error" => &config.shape,
            _ => "one",
        };
        let canonical = format!("https://{authority}/{resource}").parse()?;
        let alternate = match config.shape.as_str() {
            "bad-scheme" => format!("http://{authority}/{resource}").parse()?,
            "alternate" | "mixed" | "unstarted-retry" | "bad-port" | "bad-origin" | "userinfo" => {
                format!("https://{alternate_authority}/{resource}").parse()?
            }
            _ => canonical,
        };
        let canonical = format!("https://{authority}/{resource}").parse()?;
        Ok(Self {
            client: Client::try_new(&origin, limits())?,
            canonical,
            alternate,
            method: match config.shape.as_str() {
                "empty" => Method::HEAD,
                "empty-put" => Method::PUT,
                "empty-patch" => Method::PATCH,
                _ => Method::POST,
            },
            body: if resource == "empty" {
                Bytes::new()
            } else {
                BODY.clone()
            },
            caller_host: config.shape == "caller-host",
            mixed: config.shape == "mixed",
            headers_only: config.scenario == "http-headers",
            retry_probe: false,
        })
    }

    fn request(&self, index: usize) -> Result<Request<Bytes>> {
        let uri = if self.mixed && index % 2 == 0 {
            &self.canonical
        } else {
            &self.alternate
        };
        let mut builder = Request::builder().method(&self.method).uri(uri);
        if self.caller_host {
            builder = builder.header(http::header::HOST, "fixture-derived.invalid");
        }
        if self.retry_probe && index == 0 {
            builder = builder.header("x-outbound-perf-retry-probe", "1");
        }
        if self.body.is_empty() {
            builder = builder
                .header(http::header::CONTENT_LENGTH, "7")
                .header(http::header::TRANSFER_ENCODING, "chunked");
        }
        Ok(builder.body(self.body.clone())?)
    }

    async fn buffered(
        &self,
        index: usize,
    ) -> Result<std::result::Result<http::Response<Bytes>, ExchangeError>> {
        let context = OperationContext::from_deadline(Deadline::after(Duration::from_secs(3)));
        Ok(self
            .client
            .execute_buffered(self.request(index)?, &context)
            .await)
    }

    async fn call(&self, index: usize) -> Result<Observation> {
        if self.headers_only {
            let context = OperationContext::from_deadline(Deadline::after(Duration::from_secs(3)));
            let status = self
                .client
                .execute_for_headers(self.request(index)?, &context)
                .await;
            return Ok(match status {
                Ok(parts) => Observation::success(parts.status.as_u16(), &[]),
                Err(error) => observe_error(error, None),
            });
        }
        Ok(match self.buffered(index).await? {
            Ok(response) => Observation::success(response.status().as_u16(), response.body()),
            Err(ExchangeError::BeforeResponse(error)) => observe_error(error, None),
            Err(ExchangeError::AfterResponse { status, source }) => {
                observe_error(source, Some(status.as_u16()))
            }
        })
    }
}

fn limits() -> Limits {
    Limits {
        operation_timeout: Duration::from_secs(3),
        response_header_count: 32,
        response_body_bytes: 128 * 1024,
    }
}

fn observe_error(error: Error, status: Option<u16>) -> Observation {
    let outcome = match error {
        Error::InvalidTarget | Error::DispatchRefused => "refused",
        Error::Timeout => "timed_out",
        Error::Cancelled => "cancelled",
        _ if status.is_some() => "post-header-failure",
        _ => "failed",
    };
    Observation::failure(outcome, status)
}

fn expected(config: &Config) -> Observation {
    if matches!(
        config.shape.as_str(),
        "empty" | "empty-post" | "empty-put" | "empty-patch"
    ) {
        return Observation::success(204, &[]);
    }
    if matches!(
        config.shape.as_str(),
        "caller-host" | "bad-port" | "bad-origin" | "bad-scheme" | "userinfo"
    ) {
        return Observation::failure("refused", None);
    }
    if config.scenario == "http-headers" {
        return Observation::success(200, &[]);
    }
    if config.shape == "late-error" {
        return Observation::failure("post-header-failure", Some(200));
    }
    let (bytes, last) = match config.shape.as_str() {
        "tiny" => (1, b'a'),
        "large" => (65_536, b'a'),
        "multi" => (1024, b'b'),
        _ => (1024, b'a'),
    };
    Observation {
        outcome: "successful",
        status: Some(200),
        bytes,
        checksum: (u64::from(b'a') << 8) | u64::from(last),
    }
}

fn equivalent(observed: &Observation, expected: &Observation) -> bool {
    (
        observed.outcome,
        observed.status,
        observed.bytes,
        observed.checksum,
    ) == (
        expected.outcome,
        expected.status,
        expected.bytes,
        expected.checksum,
    )
}

fn origin_probes() -> Result<Value> {
    let ipv4 = Client::try_new(&Url::parse("https://localhost")?, limits())?;
    let ipv6 = Client::try_new(&Url::parse("https://[::1]")?, limits())?;
    let probes = [
        (&ipv4, "https://localhost/", true),
        (&ipv4, "https://LOCALHOST/", true),
        (&ipv4, "https://localhost:443/", true),
        (&ipv4, "https://localhost:/", true),
        (&ipv4, "https://localhost:444/", false),
        (&ipv4, "http://localhost/", false),
        (&ipv4, "https://synthetic@localhost/", false),
        (&ipv6, "https://[::1]/", true),
        (&ipv6, "https://[::1]:443/", true),
        (&ipv6, "https://[::1]:/", true),
    ];
    let mut results = Vec::with_capacity(probes.len());
    for (client, uri, expected) in probes {
        let actual = client.matches_origin(&uri.parse()?);
        results.push(json!({"uri": uri, "expected": expected, "actual": actual}));
    }
    Ok(json!(results))
}

fn memory_marker(phase: &str, started: Instant) -> Result<Value> {
    let proc = std::fs::read_to_string("/proc/self/status")?;
    let rss: Vec<_> = proc
        .lines()
        .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
        .collect();
    Ok(json!({
        "phase": phase,
        "elapsed_ns": started.elapsed().as_nanos(),
        "unix_ns": SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
        "process_memory": rss,
    }))
}

async fn retain(config: &Config, workload: &Workload, expected: &Observation) -> Result<Value> {
    let started = Instant::now();
    let mut bodies = Vec::with_capacity(config.count);
    let before = memory_marker("before-retention", started)?;
    let before_peer = control(config, "GET", "/stats")?;
    for index in 0..config.count {
        let response = workload.buffered(index).await??;
        if !equivalent(
            &Observation::success(response.status().as_u16(), response.body()),
            expected,
        ) {
            return Err("retained response mismatch".into());
        }
        bodies.push(response.into_body());
    }
    let filled = memory_marker("filled", started)?;
    tokio::time::sleep(Duration::from_millis(config.hold_ms)).await;
    let held = memory_marker("held", started)?;
    let visible_bytes = bodies.iter().map(Bytes::len).sum::<usize>();
    drop(bodies);
    let dropped = memory_marker("dropped", started)?;
    Ok(json!({
        "valid": true,
        "windows": [],
        "retained": config.count,
        "visible_bytes": visible_bytes,
        "hold_ms": config.hold_ms,
        "memory_markers": [before, filled, held, dropped],
        "peer_before": before_peer,
        "peer_after": control(config, "GET", "/stats")?,
    }))
}

pub(crate) async fn run(config: &Config) -> Result<Value> {
    let mut workload = Workload::new(config)?;
    let expected = expected(config);
    let warm = measure_with(
        config.warmup,
        config.concurrency,
        workload.clone(),
        |workload, index| async move {
            let observed = workload
                .call(index)
                .await
                .unwrap_or_else(|_| Observation::failure("harness-error", None));
            (workload, observed)
        },
    )
    .await?;
    let expected_json = serde_json::to_value(&expected)?;
    let warm_valid = matches_window(&warm, &expected_json)?;
    if !warm_valid {
        return Ok(
            json!({"valid": false, "warmup_window": warm, "expected": expected_json, "windows": []}),
        );
    }
    if config.scenario == "http-retain" {
        return retain(config, &workload, &expected).await;
    }
    let retry_arm = if config.shape == "retry" {
        Some(control(config, "POST", "/retry-arm")?)
    } else {
        None
    };
    let peer_before = control(config, "GET", "/stats")?;
    workload.retry_probe = config.shape == "unstarted-retry";
    let window = measure_with(
        config.count,
        config.concurrency,
        workload.clone(),
        |workload, index| async move {
            let observed = workload
                .call(index)
                .await
                .unwrap_or_else(|_| Observation::failure("harness-error", None));
            (workload, observed)
        },
    )
    .await?;
    if config.shape == "late-error" {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let peer_after = control(config, "GET", "/stats")?;
    let wire = wire_matches(
        &workload,
        &peer_before,
        &peer_after,
        &expected,
        config.count,
    )?;
    let probes = origin_probes()?;
    let reconnect = retry_arm.as_ref().is_none_or(|armed| {
        armed["closed"].as_u64().is_some_and(|n| n > 0)
            && peer_after["http_connections"].as_u64() > peer_before["http_connections"].as_u64()
    });
    let valid = wire
        && reconnect
        && matches_window(&window, &expected_json)?
        && probes.as_array().is_some_and(|records| {
            records
                .iter()
                .all(|record| record["expected"] == record["actual"])
        });
    Ok(json!({
        "valid": valid,
        "window": window,
        "expected": expected_json,
        "retry_arm": retry_arm,
        "wire_matches": wire,
        "reconnect_observed": retry_arm.as_ref().map(|_| reconnect),
        "origin_probes": probes,
        "peer_before": peer_before,
        "peer_after": peer_after,
    }))
}

fn wire_matches(
    workload: &Workload,
    before: &Value,
    after: &Value,
    expected: &Observation,
    offered: usize,
) -> Result<bool> {
    let requests = |stats: &Value| stats["resources"].as_u64().ok_or("missing resource count");
    let wanted = if expected.outcome == "refused" {
        0
    } else {
        u64::try_from(offered)?
    };
    if requests(after)?.checked_sub(requests(before)?) != Some(wanted) {
        return Ok(false);
    }
    if wanted == 0 {
        return Ok(true);
    }
    let host = workload
        .canonical
        .authority()
        .ok_or("missing canonical authority")?
        .as_str();
    let target = workload.canonical.path();
    let body_bytes = workload.body.len();
    let content_length = if body_bytes > 0
        || matches!(workload.method, Method::POST | Method::PUT | Method::PATCH)
    {
        json!([body_bytes.to_string()])
    } else {
        json!([])
    };
    let transfer_encoding = json!([]);
    let wire_count = |stats: &Value| -> Result<u64> {
        let rows = stats["http_wire"]
            .as_array()
            .ok_or("missing HTTP wire counts")?;
        Ok(rows
            .iter()
            .filter(|entry| {
                entry["method"] == workload.method.as_str()
                    && entry["target"] == target
                    && entry["host"] == host
                    && entry["content_length"] == content_length
                    && entry["transfer_encoding"] == transfer_encoding
                    && entry["body_bytes"] == body_bytes
            })
            .filter_map(|entry| entry["requests"].as_u64())
            .sum())
    };
    Ok(wire_count(after)?.checked_sub(wire_count(before)?) == Some(wanted))
}

fn matches_window(window: &Value, expected: &Value) -> Result<bool> {
    let samples = window["samples"]
        .as_array()
        .ok_or("measurement did not return samples")?;
    let expected = expected
        .as_object()
        .ok_or("expected observation was not an object")?;
    Ok(samples
        .iter()
        .all(|sample| expected.iter().all(|(key, value)| sample[key] == *value)))
}
