use std::{fs, future::pending, time::Duration};

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, header, uri::PathAndQuery};
use http_body_util::{BodyExt as _, Full};
use infra_oauth2_client_credentials::{ServiceTokenTarget, grpc::AuthenticatedClient};
use infra_outbound_grpc::{Client, ClientSecurity, ClientTlsMaterial};
use opentelemetry::{
    Context,
    context::FutureExt as _,
    trace::{SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TracerProvider as _},
};
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider};
use serde_json::{Value, json};
use tonic::{Code, GrpcMethod, Status, body::Body, client::Grpc};
use tonic_prost::ProstCodec;
use tower::Service as _;
use tracing_subscriber::{filter::LevelFilter, layer::SubscriberExt as _};

use crate::{Config, Observation, Result, control, measure_with};

const SERVICE: &str = "bench.v1.Outbound";
const FILL_SERVICE: &str = "bench.v1.Fill";
const METHOD_LIMIT: usize = 256;
const USER_AGENT: &str = "outbound-call-perf/1";
const EMPTY_PROTO_FRAME: Bytes = Bytes::from_static(&[0, 0, 0, 0, 0]);
const RESPONSE_FRAME: &[u8] = &[0, 0, 0, 0, 2, 8, 7];
const RESPONSE_VALUE: u32 = 7;
const TRACE_ID: &str = "123456789abcdef0123456789abcdef0";
const PARENT_ID: &str = "123456789abcdef0";
const PARENT_HEADER: &str = "00-123456789abcdef0123456789abcdef0-123456789abcdef0-01";

#[derive(Clone, PartialEq, prost::Message)]
struct Empty {}

#[derive(Clone, PartialEq, prost::Message)]
struct Item {
    #[prost(uint32, tag = "1")]
    value: u32,
}

#[derive(Clone)]
struct Call {
    path: PathAndQuery,
    method: &'static str,
    items: usize,
}

#[derive(Clone)]
enum ClientPath {
    Transport(AuthenticatedClient),
    Decoded(Grpc<AuthenticatedClient>),
}

#[derive(Clone)]
struct Worker {
    client: ClientPath,
    call: Call,
    context: Context,
}

impl Worker {
    async fn observe(&mut self) -> Observation {
        let context = self.context.clone();
        self.invoke()
            .with_context(context)
            .await
            .map_or_else(status_observation, |body| body)
    }

    async fn invoke(&mut self) -> std::result::Result<Observation, Status> {
        match &mut self.client {
            ClientPath::Transport(client) => {
                let request = raw_request(self.call.path.clone(), SERVICE, self.call.method);
                let response = client.call(request).await?;
                consume_transport(response, self.call.items).await
            }
            ClientPath::Decoded(client) => consume_decoded(client, &self.call).await,
        }
    }
}

pub(super) async fn run(config: &Config) -> Result<Value> {
    let (fallback, shape) = config
        .shape
        .strip_prefix("fallback-")
        .map_or((false, config.shape.as_str()), |shape| (true, shape));
    let (method, items) = match shape {
        "immediate" => ("Immediate", 0),
        "unary" => ("Unary", 1),
        "stream" => ("Stream", 3),
        _ => return Err(format!("unknown gRPC response shape {shape}").into()),
    };
    if config.warmup == 0 {
        return Err("a gRPC workload needs at least one warm-up call".into());
    }
    let (_provider, context) = telemetry(config.telemetry.as_str())?;
    let path: PathAndQuery = format!("/{SERVICE}/{method}").parse()?;
    let origin = format!("https://{}", config.authority(config.grpc_port));
    let resource = Client::try_new(
        &origin,
        ClientSecurity::Tls(ClientTlsMaterial {
            ca_certificate_pem: Some(fs::read_to_string(&config.ca)?),
            identity: None,
        }),
        Duration::from_secs(10),
    )?;
    if fallback {
        for index in 0..METHOD_LIMIT {
            let method: &'static str = Box::leak(format!("Method{index}").into_boxed_str());
            let path = format!("/{FILL_SERVICE}/{method}").parse()?;
            drop(resource.prepare_call(raw_request(path, FILL_SERVICE, method))?);
        }
    }

    let registration = crate::credentials::registration(config)?;
    let (tokens, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), origin.parse()?);
    let client = tokens.grpc(resource)?;
    let client = match config.scenario.as_str() {
        "grpc-transport" => ClientPath::Transport(client),
        "grpc-decoded" => ClientPath::Decoded(Grpc::new(client)),
        scenario => return Err(format!("unknown gRPC scenario {scenario}").into()),
    };
    let driver = tokio::spawn(driver.run(pending()));
    let mut worker = Worker {
        client,
        call: Call {
            path: path.clone(),
            method,
            items,
        },
        context,
    };
    for _ in 0..config.warmup {
        let warmup = worker.observe().await;
        if warmup.outcome != "successful" {
            return Err(format!("gRPC warm-up failed with status {:?}", warmup.status).into());
        }
    }
    let before = control(config, "GET", "/stats")?;
    let window = measure_with(
        config.count,
        config.concurrency,
        worker,
        |mut worker, _| async move {
            let observation = worker.observe().await;
            (worker, observation)
        },
    )
    .await?;
    let after = control(config, "GET", "/stats")?;
    drop(registration);
    driver.await?;
    let peer_validation = validate_peer(config, &before, &after, path.as_str(), items)?;
    let peer_valid = peer_validation
        .as_object()
        .is_some_and(|checks| checks.values().all(|check| check.as_bool() == Some(true)));
    let caller_valid = window["samples"].as_array().is_some_and(|samples| {
        samples.len() == config.count
            && samples
                .iter()
                .all(|sample| sample["outcome"] == "successful")
    });
    Ok(json!({
        "window": window,
        "scenario": config.scenario,
        "shape": config.shape,
        "telemetry": config.telemetry,
        "method": path.as_str(),
        "registry_fill": if fallback { METHOD_LIMIT } else { 0 },
        "peer_before": before,
        "peer_after": after,
        "peer_validation": peer_validation,
        "valid": caller_valid && peer_valid
    }))
}

fn telemetry(mode: &str) -> Result<(Option<SdkTracerProvider>, Context)> {
    if mode == "absent" {
        return Ok((None, Context::new()));
    }
    let filter = match mode {
        "filtered" => LevelFilter::OFF,
        "enabled" => LevelFilter::TRACE,
        _ => return Err(format!("unknown gRPC telemetry mode {mode}").into()),
    };
    let provider = SdkTracerProvider::builder().build();
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("outbound-call-perf")));
    tracing::subscriber::set_global_default(subscriber)?;
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let context = Context::new().with_remote_span_context(SpanContext::new(
        TraceId::from_hex(TRACE_ID)?,
        SpanId::from_hex(PARENT_ID)?,
        TraceFlags::SAMPLED,
        true,
        Default::default(),
    ));
    Ok((Some(provider), context))
}

fn raw_request(path: PathAndQuery, service: &'static str, method: &'static str) -> Request<Body> {
    let mut request = Request::new(Body::new(Full::new(EMPTY_PROTO_FRAME)));
    *request.method_mut() = Method::POST;
    *request.uri_mut() = path.into();
    let headers = request.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/grpc"),
    );
    headers.insert(header::TE, HeaderValue::from_static("trailers"));
    headers.insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
    request
        .extensions_mut()
        .insert(GrpcMethod::new(service, method));
    request
}

async fn consume_transport(
    response: Response<Body>,
    items: usize,
) -> std::result::Result<Observation, Status> {
    if response.status() != http::StatusCode::OK {
        return Err(Status::unknown(
            "synthetic gRPC response has non-200 HTTP status",
        ));
    }
    let mut status = Status::from_header_map(response.headers());
    let terminal_in_headers = status.is_some();
    let mut body = response.into_body();
    let mut received = 0;
    let mut expected = RESPONSE_FRAME
        .iter()
        .copied()
        .cycle()
        .take(items * RESPONSE_FRAME.len());
    let mut payload_matches = true;
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            received += data.len();
            payload_matches &= data.iter().all(|byte| Some(*byte) == expected.next());
        }
        if let Some(trailers) = frame.trailers_ref() {
            status = Status::from_header_map(trailers);
        }
    }
    let status =
        status.ok_or_else(|| Status::unknown("synthetic gRPC response has no gRPC status"))?;
    if status.code() != Code::Ok {
        return Err(status);
    }
    if terminal_in_headers != (items == 0) || !payload_matches || expected.next().is_some() {
        return Err(Status::data_loss(
            "synthetic gRPC frame or terminal status mismatch",
        ));
    }
    Ok(Observation {
        outcome: "successful",
        status: Some(0),
        bytes: received,
        checksum: items as u64 * u64::from(RESPONSE_VALUE),
    })
}

async fn consume_decoded(
    client: &mut Grpc<AuthenticatedClient>,
    call: &Call,
) -> std::result::Result<Observation, Status> {
    client.ready().await?;
    let mut request = tonic::Request::new(Empty {});
    request
        .extensions_mut()
        .insert(GrpcMethod::new(SERVICE, call.method));
    request.metadata_mut().insert(
        "user-agent",
        tonic::metadata::MetadataValue::from_static(USER_AGENT),
    );
    if call.items == 1 {
        let item = client
            .unary(
                request,
                call.path.clone(),
                ProstCodec::<Empty, Item>::default(),
            )
            .await?
            .into_inner();
        if item.value != RESPONSE_VALUE {
            return Err(Status::data_loss("synthetic gRPC message mismatch"));
        }
        return Ok(Observation {
            outcome: "successful",
            status: Some(0),
            bytes: RESPONSE_FRAME.len() - 5,
            checksum: u64::from(item.value),
        });
    }
    let mut response = client
        .server_streaming(
            request,
            call.path.clone(),
            ProstCodec::<Empty, Item>::default(),
        )
        .await?
        .into_inner();
    let mut items = 0;
    let mut checksum = 0;
    let mut payload_matches = true;
    while let Some(item) = response.message().await? {
        items += 1;
        checksum += u64::from(item.value);
        payload_matches &= item.value == RESPONSE_VALUE;
    }
    if items != call.items || !payload_matches {
        return Err(Status::data_loss("synthetic gRPC message mismatch"));
    }
    Ok(Observation {
        outcome: "successful",
        status: Some(0),
        bytes: items * (RESPONSE_FRAME.len() - 5),
        checksum,
    })
}

fn status_observation(status: Status) -> Observation {
    let outcome = match status.code() {
        Code::Cancelled => "cancelled",
        Code::DeadlineExceeded => "timed_out",
        Code::Unauthenticated | Code::PermissionDenied | Code::ResourceExhausted => "refused",
        _ => "failed",
    };
    Observation::failure(outcome, Some(status.code() as u16))
}

fn validate_peer(
    config: &Config,
    before: &Value,
    after: &Value,
    method: &str,
    items: usize,
) -> Result<Value> {
    let delta = |key: &str| {
        after[key]
            .as_u64()
            .zip(before[key].as_u64())
            .and_then(|(after, before)| after.checked_sub(before))
    };
    let offered = u64::try_from(config.count)?;
    let expected_messages = offered.checked_mul(u64::try_from(items)?);
    let expected_request_bytes = offered.checked_mul(u64::try_from(EMPTY_PROTO_FRAME.len())?);
    let method_delta = after["grpc_methods"][method]
        .as_u64()
        .zip(before["grpc_methods"][method].as_u64())
        .and_then(|(after, before)| after.checked_sub(before));
    let traceparent = after["grpc_last_traceparent"].as_str().unwrap_or_default();
    let status_delta = after["grpc_status"]["0"]
        .as_u64()
        .zip(before["grpc_status"]["0"].as_u64())
        .and_then(|(after, before)| after.checked_sub(before));
    let expected_context = match config.telemetry.as_str() {
        "filtered" => traceparent == PARENT_HEADER,
        "enabled" => traceparent
            .strip_prefix("00-")
            .and_then(|value| value.strip_prefix(TRACE_ID))
            .and_then(|value| value.strip_prefix('-'))
            .and_then(|value| value.strip_suffix("-01"))
            .is_some_and(|span| {
                span.len() == PARENT_ID.len()
                    && SpanId::from_hex(span).is_ok_and(|id| id != SpanId::INVALID)
                    && span != PARENT_ID
            }),
        _ => true,
    };
    Ok(json!({
        "requests": delta("grpc_requests") == Some(offered),
        "method": method_delta == Some(offered),
        "status": status_delta == Some(offered),
        "messages": expected_messages.is_some() && delta("grpc_messages") == expected_messages,
        "request_bytes": expected_request_bytes.is_some() && delta("grpc_request_bytes") == expected_request_bytes,
        "warm_connection": delta("grpc_connections") == Some(0),
        "only_setup_grant": before["service_grants"].as_u64() == Some(1) && delta("service_grants") == Some(0),
        "old_token": delta("grpc_old_token_requests") == Some(offered),
        "traceparent_count": delta("grpc_traceparent") == Some(if config.telemetry == "absent" { 0 } else { offered }),
        "traceparent_context": expected_context
    }))
}
