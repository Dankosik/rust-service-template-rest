#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]

use std::{num::NonZeroUsize, time::Duration};

use futures_util::FutureExt as _;
use http_body_util::BodyExt as _;
use infra_outbound_grpc::{Client, ClientSecurity, transport::PreparedCall};
use tower::Service as _;

const CLIENT_METHOD_LIMIT: usize = 256;
const ECHO_UNARY_PATH: &str = "/example.v1.EchoService/Unary";
const WAIT: Duration = Duration::from_secs(5);
const CANCELLED_STREAM_METRIC: &str = r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="Canceled"} 1"#;
const EMPTY_STREAM_METRIC: &str = r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="OK"} 1"#;

#[expect(
    clippy::disallowed_types,
    reason = "native gRPC wire fixture uses the native path extractor; its handler retains runtime checks"
)]
type NativePath = axum::extract::Path<String>;

#[tokio::test]
async fn client_observation_is_global_bounded_and_terminal_exactly_once() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let server = start_observation_server();
    let destination = format!("http://{}", server.local_addr());
    check_transport_statuses(&destination).await;
    check_decoded_statuses(&destination).await;
    check_unpolled_stream_statuses(&destination, &handle).await;
    let mut refusing = Client::builder(&destination, ClientSecurity::Plaintext, WAIT)
        .opening_limit(NonZeroUsize::MIN)
        .build()
        .unwrap();
    check_opening_refusals(&mut refusing);
    let mut proxied = Client::builder(&destination, ClientSecurity::Plaintext, WAIT)
        .build()
        .unwrap();
    check_proxy_status(&mut proxied).await;
    let oversized: &'static str = Box::leak("x".repeat(257).into_boxed_str());
    record_unverified_methods(&destination, oversized);
    let boundary: &'static str = Box::leak("b".repeat(256).into_boxed_str());
    let mut client = Client::builder(&destination, ClientSecurity::Plaintext, WAIT)
        .build()
        .unwrap();
    drop(client.call(client_request(
        &format!("/{boundary}/{boundary}"),
        Some((boundary, boundary)),
    )));
    fill_method_series(&destination);
    let mut client = Client::builder(&destination, ClientSecurity::Plaintext, WAIT)
        .build()
        .unwrap();
    drop(client.call(client_request(
        "/test.Observation/Header",
        Some(("test.Observation", "Header")),
    )));
    let rendered = handle.render();
    assert_observation_totals(&rendered, boundary, oversized);
    drop(client);
    assert_eq!(
        tokio::time::timeout(WAIT, server.drain(Duration::from_secs(1)))
            .await
            .unwrap()
            .unwrap(),
        infra_http_transport::Drained::Complete
    );
    check_expired_unpolled_call().await;
}

fn start_observation_server() -> infra_http_transport::Server {
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let app = axum::Router::new()
        .route(
            "/test.Observation/{method}",
            axum::routing::post(transport_status_fixture),
        )
        .route(ECHO_UNARY_PATH, axum::routing::post(client_status_fixture))
        .route(
            "/example.v1.EchoService/ServerStream",
            axum::routing::post(client_status_fixture),
        );
    infra_http_transport::BoundListener::bind("127.0.0.1:0".parse().unwrap())
        .unwrap()
        .serve_h2(
            app,
            infra_http_transport::Http2Options {
                preface_timeout: WAIT,
                max_header_list_size: 16 * 1024,
                connections: infra_http_transport::ConnectionLimits {
                    max_connections: None,
                    max_connection_age: None,
                },
            },
            None,
        )
}

#[allow(
    clippy::unused_async,
    reason = "axum's wire fixture handler must return a future"
)]
async fn transport_status_fixture(path: NativePath) -> http::Response<tonic::body::Body> {
    use http_body::Frame;
    use http_body_util::StreamBody;
    use tonic::body::Body;

    let (status, body) = match path.0.as_str() {
        "Header" => return tonic::Status::new(tonic::Code::Ok, "").into_http(),
        "Proxy" => {
            return http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(Body::empty())
                .unwrap();
        }
        "Trailer" => {
            let mut trailers = http::HeaderMap::new();
            tonic::Status::aborted("peer trailers")
                .add_header(&mut trailers)
                .unwrap();
            let frames = [
                Ok::<_, tonic::Status>(Frame::data(bytes::Bytes::from_static(b"data"))),
                Ok(Frame::trailers(trailers)),
            ];
            (
                http::StatusCode::SERVICE_UNAVAILABLE,
                Body::new(StreamBody::new(futures_util::stream::iter(frames))),
            )
        }
        _ => (
            http::StatusCode::OK,
            Body::new(StreamBody::new(futures_util::stream::pending::<
                Result<Frame<bytes::Bytes>, tonic::Status>,
            >())),
        ),
    };
    http::Response::builder()
        .status(status)
        .header("content-type", "application/grpc")
        .body(body)
        .unwrap()
}

async fn check_transport_statuses(destination: &str) {
    for (method, expected) in [
        ("Header", tonic::Code::Ok),
        ("Trailer", tonic::Code::Aborted),
        ("Cancelled", tonic::Code::Cancelled),
        ("Deadline", tonic::Code::DeadlineExceeded),
    ] {
        let mut client = Client::builder(
            destination,
            ClientSecurity::Plaintext,
            Duration::from_millis(200),
        )
        .build()
        .unwrap();
        let request = client_request(
            &format!("/test.Observation/{method}"),
            Some(("test.Observation", method)),
        );
        let response = tokio::time::timeout(WAIT, client.call(request))
            .await
            .unwrap()
            .unwrap();
        if method == "Header" {
            assert_eq!(
                tonic::Status::from_header_map(response.headers())
                    .unwrap()
                    .code(),
                expected
            );
        } else if method != "Cancelled" {
            let mut body = response.into_body();
            if method == "Trailer" {
                let data = tokio::time::timeout(WAIT, body.frame())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .into_data()
                    .unwrap();
                assert_eq!(data, bytes::Bytes::from_static(b"data"));
            }
            let status = tokio::time::timeout(WAIT, trailer_status(&mut body))
                .await
                .unwrap();
            assert_eq!(status, expected);
            assert!(body.frame().await.is_none());
        }
    }
}

async fn check_decoded_statuses(destination: &str) {
    let adapter = Client::builder(destination, ClientSecurity::Plaintext, WAIT)
        .build()
        .unwrap();
    let mut raw = client_request(ECHO_UNARY_PATH, Some(("example.v1.EchoService", "Unary")));
    raw.headers_mut()
        .insert("x-test-reply", "malformed-status-head".parse().unwrap());
    let (report, wire_code) = tokio::sync::oneshot::channel();
    let response = PreparedCall::prepare(&adapter, raw)
        .unwrap()
        .on_status(move |code| report.send(code).unwrap())
        .send()
        .await
        .unwrap();
    drop(response);
    assert_eq!(wire_code.await.unwrap(), tonic::Code::Unavailable);
    let mut generated =
        grpc_contracts::example::v1::echo_service_client::EchoServiceClient::new(adapter);
    for (reply, expected) in [
        ("invalid-argument", tonic::Code::InvalidArgument),
        ("empty-head", tonic::Code::Internal),
        ("empty-trailers", tonic::Code::Internal),
        ("decode-error", tonic::Code::Internal),
        ("malformed-status-head", tonic::Code::Unknown),
        ("malformed-status-trailers", tonic::Code::Unknown),
        ("unsupported-encoding", tonic::Code::Unimplemented),
    ] {
        let mut request = tonic::Request::new(grpc_contracts::example::v1::UnaryRequest {
            message: "native identity".to_owned(),
        });
        request
            .metadata_mut()
            .insert("x-test-reply", reply.parse().unwrap());
        let status = tokio::time::timeout(WAIT, generated.unary(request))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(status.code(), expected, "{reply}");
    }
    let mut request = tonic::Request::new(grpc_contracts::example::v1::ServerStreamRequest {
        message: "native streaming identity".to_owned(),
    });
    request
        .metadata_mut()
        .insert("x-test-reply", "decode-error".parse().unwrap());
    let mut stream = tokio::time::timeout(WAIT, generated.server_stream(request))
        .await
        .unwrap()
        .unwrap()
        .into_inner();
    let status = tokio::time::timeout(WAIT, stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Internal);
    drop(stream);
    drop(generated);
}

async fn check_unpolled_stream_statuses(
    destination: &str,
    handle: &metrics_exporter_prometheus::PrometheusHandle,
) {
    let parent = tokio_util::sync::CancellationToken::new();
    let client = Client::builder(destination, ClientSecurity::Plaintext, WAIT)
        .build()
        .unwrap();
    let mut generated =
        grpc_contracts::example::v1::echo_service_client::EchoServiceClient::new(client);
    let mut request = tonic::Request::new(grpc_contracts::example::v1::ServerStreamRequest {
        message: "unpolled response".to_owned(),
    });
    request
        .metadata_mut()
        .insert("x-test-reply", "held-stream".parse().unwrap());
    request
        .extensions_mut()
        .insert(operation_context::OperationContext::new(
            None,
            parent.clone(),
        ));
    let mut held = tokio::time::timeout(WAIT, generated.server_stream(request))
        .await
        .unwrap()
        .unwrap()
        .into_inner();
    parent.cancel();
    tokio::time::timeout(WAIT, async {
        while !handle.render().contains(CANCELLED_STREAM_METRIC) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("transport cancellation must emit a metric without polling the decoded stream");
    assert_eq!(
        tokio::time::timeout(WAIT, held.message())
            .await
            .unwrap()
            .unwrap_err()
            .code(),
        tonic::Code::Cancelled
    );
    drop(held);
    let mut request = tonic::Request::new(grpc_contracts::example::v1::ServerStreamRequest {
        message: "empty streaming identity".to_owned(),
    });
    request
        .metadata_mut()
        .insert("x-test-reply", "empty-head".parse().unwrap());
    let empty = tokio::time::timeout(WAIT, generated.server_stream(request))
        .await
        .unwrap()
        .unwrap()
        .into_inner();
    assert!(handle.render().contains(EMPTY_STREAM_METRIC));
    drop(empty);
    drop(generated);
}

fn check_opening_refusals(refusing: &mut Client) {
    let unary_request =
        || client_request(ECHO_UNARY_PATH, Some(("example.v1.EchoService", "Unary")));
    let abandoned = refusing.call(unary_request());
    let at_capacity = refusing
        .clone()
        .call(unary_request())
        .now_or_never()
        .unwrap();
    assert_eq!(
        at_capacity.unwrap_err().code(),
        tonic::Code::ResourceExhausted
    );
    drop(abandoned);
    let status = PreparedCall::prepare(refusing, unary_request())
        .unwrap()
        .try_step(|_, _| Err(tonic::Status::unavailable("credentials unavailable")))
        .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

async fn check_proxy_status(proxied: &mut Client) {
    let response = tokio::time::timeout(
        WAIT,
        proxied.call(client_request(
            "/test.Observation/Proxy",
            Some(("test.Observation", "Proxy")),
        )),
    )
    .await
    .unwrap()
    .unwrap();
    drop(response);
}

fn record_unverified_methods(destination: &str, oversized: &'static str) {
    for (path, method) in [
        ("/raw.Service/Chosen".to_owned(), None),
        (
            "/mismatch.Service/Chosen".to_owned(),
            Some(("other.Service", "Chosen")),
        ),
        (
            "/extra.Service/Chosen/Tail".to_owned(),
            Some(("extra.Service", "Chosen/Tail")),
        ),
        (format!("/{oversized}/Chosen"), Some((oversized, "Chosen"))),
        (
            format!("/overlong.Service/{oversized}"),
            Some(("overlong.Service", oversized)),
        ),
    ] {
        let mut client = Client::builder(destination, ClientSecurity::Plaintext, WAIT)
            .build()
            .unwrap();
        drop(client.call(client_request(&path, method)));
    }
}

fn fill_method_series(destination: &str) {
    for index in 0..=CLIENT_METHOD_LIMIT {
        let method: &'static str = Box::leak(format!("Method{index}").into_boxed_str());
        let mut client = Client::builder(destination, ClientSecurity::Plaintext, WAIT)
            .build()
            .unwrap();
        drop(client.call(client_request(
            &format!("/bounded.Service/{method}"),
            Some(("bounded.Service", method)),
        )));
    }
}

fn assert_observation_totals(rendered: &str, boundary: &str, oversized: &str) {
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with("grpc_client_started_total{"))
            .count(),
        CLIENT_METHOD_LIMIT + 1,
        "{rendered}"
    );
    for (method, status) in [
        ("Header", "OK"),
        ("Trailer", "Aborted"),
        ("Cancelled", "Canceled"),
        ("Deadline", "DeadlineExceeded"),
    ] {
        let labels = format!(r#"grpc_service="test.Observation",grpc_method="{method}""#);
        assert!(
            rendered.contains(&format!(
                r#"grpc_client_handled_total{{{labels},grpc_code="{status}"}} 1"#
            )),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!(
                "grpc_client_started_total{{{labels}}} {}",
                if method == "Header" { 2 } else { 1 }
            )),
            "{rendered}"
        );
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with(&format!("grpc_client_handled_total{{{labels},")))
                .count(),
            if method == "Header" { 2 } else { 1 },
            "{rendered}"
        );
    }
    let unary = r#"grpc_service="example.v1.EchoService",grpc_method="Unary""#;
    assert!(
        rendered.contains(&format!("grpc_client_started_total{{{unary}}} 11")),
        "{rendered}"
    );
    let handled_prefix = format!("grpc_client_handled_total{{{unary},");
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with(&handled_prefix))
            .count(),
        7,
        "{rendered}"
    );
    for (code, count) in [
        ("Canceled", 1),
        ("InvalidArgument", 1),
        ("Internal", 3),
        ("ResourceExhausted", 1),
        ("Unknown", 3),
        ("Unimplemented", 1),
        ("Unavailable", 1),
    ] {
        assert!(
            rendered.contains(&format!(r#"{handled_prefix}grpc_code="{code}"}} {count}"#)),
            "{rendered}"
        );
    }
    assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="Internal"} 1"#), "{rendered}");
    assert!(rendered.contains(CANCELLED_STREAM_METRIC), "{rendered}");
    assert!(rendered.contains(EMPTY_STREAM_METRIC), "{rendered}");
    assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="test.Observation",grpc_method="Header",grpc_code="Canceled"} 1"#), "{rendered}");
    assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="test.Observation",grpc_method="Proxy",grpc_code="Unauthenticated"} 1"#), "{rendered}");
    assert!(
        rendered.contains(
            r#"grpc_client_started_total{grpc_service="unknown",grpc_method="unknown"} 14"#
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!(
            r#"grpc_service="{boundary}",grpc_method="{boundary}""#
        )),
        "{rendered}"
    );
    for forbidden in [
        "raw.Service",
        "mismatch.Service",
        "other.Service",
        "extra.Service",
        "overlong.Service",
        oversized,
        "Method256",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "unexpected client identity {forbidden}: {rendered}"
        );
    }
}

async fn check_expired_unpolled_call() {
    tokio::time::pause();
    let mut client = Client::builder(
        "http://127.0.0.1:1",
        ClientSecurity::Plaintext,
        Duration::from_secs(1),
    )
    .build()
    .unwrap();
    let pending = client.call(client_request("/unpolled.Service/Unary", None));
    tokio::time::advance(Duration::from_secs(1)).await;
    let result = tokio::time::timeout(Duration::ZERO, pending)
        .await
        .expect("expired call answers without channel progress");
    assert_eq!(result.unwrap_err().code(), tonic::Code::DeadlineExceeded);
}

#[allow(
    clippy::unused_async,
    reason = "axum's wire fixture handler must return a future"
)]
async fn client_status_fixture(headers: http::HeaderMap) -> http::Response<tonic::body::Body> {
    use http_body::Frame;
    use tonic::body::Body;

    let reply = headers["x-test-reply"].to_str().unwrap();
    let trailers = |code, message| {
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static(code));
        if let Some(message) = message {
            trailers.insert("grpc-message", http::HeaderValue::from_static(message));
        }
        Frame::trailers(trailers)
    };
    let frames = |frame| {
        Body::new(http_body_util::StreamBody::new(futures_util::stream::iter(
            [Ok::<_, tonic::Status>(frame)],
        )))
    };
    let mut response = http::Response::builder().header("content-type", "application/grpc");
    let body = match reply {
        "invalid-argument" => {
            return tonic::Status::invalid_argument("native generated method").into_http();
        }
        "empty-head" => {
            response = response.header("grpc-status", "0");
            Body::empty()
        }
        "empty-trailers" => frames(trailers("0", None)),
        "decode-error" => frames(Frame::data(bytes::Bytes::from_static(&[2, 0, 0, 0, 0]))),
        "held-stream" => Body::new(http_body_util::StreamBody::new(
            futures_util::stream::pending::<Result<Frame<bytes::Bytes>, tonic::Status>>(),
        )),
        "malformed-status-head" => {
            response = response
                .header("grpc-status", "14")
                .header("grpc-message", "%FF");
            Body::empty()
        }
        "malformed-status-trailers" => frames(trailers("14", Some("%FF"))),
        "unsupported-encoding" => {
            response = response
                .header("grpc-status", "14")
                .header("grpc-encoding", "gzip");
            Body::empty()
        }
        _ => panic!("unknown test reply {reply}"),
    };
    response.body(body).unwrap()
}

async fn trailer_status(body: &mut tonic::body::Body) -> tonic::Code {
    while let Some(frame) = body.frame().await {
        let frame = frame.unwrap();
        if let Some(trailers) = frame.trailers_ref() {
            return tonic::Status::from_header_map(trailers).unwrap().code();
        }
    }
    tonic::Code::Unknown
}

fn client_request(
    path: &str,
    method: Option<(&'static str, &'static str)>,
) -> http::Request<tonic::body::Body> {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .body(tonic::body::Body::empty())
        .unwrap();
    if let Some((service, method)) = method {
        request
            .extensions_mut()
            .insert(tonic::GrpcMethod::new(service, method));
    }
    request
}
