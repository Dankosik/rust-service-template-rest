#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]

use std::time::Duration;

use http_body_util::BodyExt as _;
use infra_outbound_grpc::{Client, ClientSecurity};
use tower::Service as _;

const CLIENT_METHOD_LIMIT: usize = 256;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one process-wide metric registry must cover admission and terminal paths without a test-only reset"
)]
async fn client_observation_is_global_bounded_and_terminal_exactly_once() {
    const OWN_PROCESS: &str = "INFRA_GRPC_CLIENT_OBSERVATION_OWN_PROCESS";
    const WAIT: Duration = Duration::from_secs(5);
    #[expect(
        clippy::disallowed_types,
        reason = "native gRPC wire fixture uses the native path extractor; its handler retains runtime checks"
    )]
    type NativePath = axum::extract::Path<String>;
    if std::env::var("NEXTEST_EXECUTION_MODE").as_deref() != Ok("process-per-test")
        && std::env::var_os(OWN_PROCESS).is_none()
    {
        let name = "client_observation_is_global_bounded_and_terminal_exactly_once";
        #[expect(
            clippy::disallowed_methods,
            clippy::disallowed_types,
            reason = "cargo test fallback isolates the global metric registry in a fresh child process"
        )]
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--test-threads=1"])
            .env(OWN_PROCESS, "1")
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        return;
    }
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let handler_1 = |path: NativePath| async move {
        let method = path.0;
        if method == "Header" {
            return tonic::Status::new(tonic::Code::Ok, "").into_http();
        }
        if method == "Proxy" {
            return http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(tonic::body::Body::empty())
                .unwrap();
        }
        let body = if method == "Trailer" {
            let mut trailers = http::HeaderMap::new();
            tonic::Status::aborted("peer trailers")
                .add_header(&mut trailers)
                .unwrap();
            tonic::body::Body::new(http_body_util::StreamBody::new(futures_util::stream::iter(
                [
                    Ok::<_, tonic::Status>(http_body::Frame::data(bytes::Bytes::from_static(
                        b"data",
                    ))),
                    Ok(http_body::Frame::trailers(trailers)),
                ],
            )))
        } else {
            tonic::body::Body::new(http_body_util::StreamBody::new(
                futures_util::stream::pending::<
                    Result<http_body::Frame<bytes::Bytes>, tonic::Status>,
                >(),
            ))
        };
        http::Response::builder()
            .status(if method == "Trailer" {
                http::StatusCode::SERVICE_UNAVAILABLE
            } else {
                http::StatusCode::OK
            })
            .header("content-type", "application/grpc")
            .body(body)
            .unwrap()
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
    )]
    let app = axum::Router::new()
        .route("/test.Observation/{method}", axum::routing::post(handler_1))
        .route(
            "/example.v1.EchoService/Unary",
            axum::routing::post(client_status_fixture),
        )
        .route(
            "/example.v1.EchoService/ServerStream",
            axum::routing::post(client_status_fixture),
        );
    let server = infra_http::BoundListener::bind("127.0.0.1:0".parse().unwrap())
        .unwrap()
        .serve_h2(
            app,
            infra_http::ServerOptions {
                header_read_timeout: WAIT,
                max_header_bytes: 16 * 1024,
                max_connections: None,
                max_connection_age: None,
            },
            None,
        );
    let destination = format!("http://{}", server.local_addr());
    for (method, expected) in [
        ("Header", tonic::Code::Ok),
        ("Trailer", tonic::Code::Aborted),
        ("Cancelled", tonic::Code::Cancelled),
        ("Deadline", tonic::Code::DeadlineExceeded),
    ] {
        let mut client = Client::try_new(
            &destination,
            ClientSecurity::Plaintext,
            Duration::from_millis(200),
        )
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
    let adapter = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
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
    let parent = tokio_util::sync::CancellationToken::new();
    let client = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
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
    let canceled = r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="Canceled"} 1"#;
    tokio::time::timeout(WAIT, async {
        while !handle.render().contains(canceled) {
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
    let empty_ok = r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="OK"} 1"#;
    assert!(handle.render().contains(empty_ok));
    drop(empty);
    drop(generated);
    let refusing = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
    let status = refusing
        .prepare_call(client_request(
            "/example.v1.EchoService/Unary",
            Some(("example.v1.EchoService", "Unary")),
        ))
        .refuse(tonic::Status::unavailable("credentials unavailable"));
    assert_eq!(status.code(), tonic::Code::Unavailable);
    let mut proxied = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
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

    let oversized: &'static str = Box::leak("x".repeat(257).into_boxed_str());
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
        let mut client = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
        drop(client.call(client_request(&path, method)));
    }
    let boundary: &'static str = Box::leak("b".repeat(256).into_boxed_str());
    let mut client = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
    drop(client.call(client_request(
        &format!("/{boundary}/{boundary}"),
        Some((boundary, boundary)),
    )));

    for index in 0..=CLIENT_METHOD_LIMIT {
        let method: &'static str = Box::leak(format!("Method{index}").into_boxed_str());
        let mut client = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
        drop(client.call(client_request(
            &format!("/bounded.Service/{method}"),
            Some(("bounded.Service", method)),
        )));
    }
    let mut client = Client::try_new(&destination, ClientSecurity::Plaintext, WAIT).unwrap();
    drop(client.call(client_request(
        "/test.Observation/Header",
        Some(("test.Observation", "Header")),
    )));
    let rendered = handle.render();
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
        rendered.contains(&format!("grpc_client_started_total{{{unary}}} 8")),
        "{rendered}"
    );
    let handled_prefix = format!("grpc_client_handled_total{{{unary},");
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.starts_with(&handled_prefix))
            .count(),
        5,
        "{rendered}"
    );
    for (code, count) in [
        ("InvalidArgument", 1),
        ("Internal", 3),
        ("Unknown", 2),
        ("Unimplemented", 1),
        ("Unavailable", 1),
    ] {
        assert!(
            rendered.contains(&format!(r#"{handled_prefix}grpc_code="{code}"}} {count}"#)),
            "{rendered}"
        );
    }
    assert!(rendered.contains(r#"grpc_client_handled_total{grpc_service="example.v1.EchoService",grpc_method="ServerStream",grpc_code="Internal"} 1"#), "{rendered}");
    assert!(rendered.contains(canceled), "{rendered}");
    assert!(rendered.contains(empty_ok), "{rendered}");
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
    drop(client);
    assert_eq!(
        tokio::time::timeout(WAIT, server.drain(Duration::from_secs(1)))
            .await
            .unwrap()
            .unwrap(),
        infra_http::Drained::Complete
    );

    tokio::time::pause();
    let mut client = Client::try_new(
        "http://127.0.0.1:1",
        ClientSecurity::Plaintext,
        Duration::from_secs(1),
    )
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
