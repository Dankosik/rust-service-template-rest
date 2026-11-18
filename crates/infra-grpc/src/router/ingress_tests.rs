use std::convert::Infallible;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use bytes::Bytes;
use http::{Method, StatusCode, header};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};
use tower::ServiceExt as _;

use super::native_router;
use crate::Services;

const HEALTH_CHECK: &str = "/grpc.health.v1.Health/Check";
const HEALTH_WATCH: &str = "/grpc.health.v1.Health/Watch";
const UNKNOWN: &str = "/unknown.Service/Call";
const OPENING: Duration = Duration::from_secs(1);

fn app() -> (axum::Router, ::health::Readiness) {
    let readiness = ::health::Readiness::new(
        Vec::new(),
        ::health::RefreshPolicy::try_new(Duration::from_mins(1), OPENING, NonZeroU32::MIN).unwrap(),
    );
    let mut services = Services::new();
    services.set_listener_layer(tower::util::MapResponseLayer::new(
        |mut response: axum::response::Response| {
            response
                .headers_mut()
                .insert("x-native", "yes".parse().unwrap());
            response
        },
    ));
    services.attach_reflection().unwrap();
    services
        .learn(tonic_health::pb::FILE_DESCRIPTOR_SET)
        .unwrap();
    let methods = Arc::new(services.methods());
    let routes = services.routes.routes().into_axum_router();
    (
        native_router(
            routes,
            services.listener_layer,
            readiness.control().reader(),
            services.names,
            methods,
            OPENING,
        ),
        readiness,
    )
}

fn request(path: &str, method: Method, media: &[&str], body: Body) -> Request {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(path)
        .version(http::Version::HTTP_2);
    for &value in media {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    builder.body(body).unwrap()
}

fn pending_body() -> Body {
    Body::from_stream(futures_util::stream::pending::<Result<Bytes, Infallible>>())
}

#[tokio::test(start_paused = true)]
async fn native_protocol_precedes_health_business_and_fallback_dispatch() {
    let (app, _readiness) = app();
    let reflection = crate::services::REFLECTION_PATHS[0];
    for path in [HEALTH_CHECK, reflection, UNKNOWN] {
        let request = request(path, Method::GET, &[], pending_body());
        let response = tokio::time::timeout(OPENING, app.clone().oneshot(request))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED, "{path}");
        assert_eq!(response.headers()[header::ALLOW], "POST");
        assert!(!response.headers().contains_key("x-native"));
        assert!(!response.headers().contains_key("grpc-status"));
    }
    for (path, media) in [
        (HEALTH_CHECK, &[][..]),
        (reflection, &["application/grpc-web+proto"][..]),
        (UNKNOWN, &["text/plain"][..]),
        (UNKNOWN, &["application/grpc+"][..]),
        (HEALTH_CHECK, &["application/grpc", "text/plain"][..]),
        (UNKNOWN, &["application/grpc; version=2, text/plain"][..]),
        (UNKNOWN, &["application/grpc; profile"][..]),
        (UNKNOWN, &["application/grpc; profile="][..]),
    ] {
        let request = request(path, Method::POST, media, Body::empty());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{path} {media:?}"
        );
        assert!(!response.headers().contains_key("x-native"));
        assert!(!response.headers().contains_key("grpc-status"));
    }
    for media in [
        "application/grpc",
        "application/grpc+proto",
        "application/grpc+proto; profile=vendor~v2|next",
        "Application/GRPC+vendor.v2 ; version=2 ; profile=\"custom;v2, test\"",
        r#"application/grpc+custom+v2; label="x\"y"; empty="""#,
    ] {
        let request = request(UNKNOWN, Method::POST, &[media], Body::empty());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{media}");
        assert_eq!(response.headers()["grpc-status"], "12", "{media}");
        assert_eq!(response.headers()["x-native"], "yes", "{media}");
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_health_upload_expires_at_the_earlier_opening_deadline() {
    let (app, _readiness) = app();
    for (path, caller, budget) in [
        (HEALTH_CHECK, None, OPENING),
        (HEALTH_WATCH, Some("1H"), OPENING),
        (HEALTH_CHECK, Some("20m"), Duration::from_millis(20)),
    ] {
        let mut request = request(path, Method::POST, &["application/grpc"], pending_body());
        if let Some(caller) = caller {
            request
                .headers_mut()
                .insert("grpc-timeout", http::HeaderValue::from_static(caller));
        }
        let started = tokio::time::Instant::now();
        let response = tokio::time::timeout(
            budget + Duration::from_millis(10),
            app.clone().oneshot(request),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.headers()["grpc-status"], "4", "{path} {caller:?}");
        assert!(started.elapsed() >= budget);
    }
}

#[tokio::test(start_paused = true)]
async fn opened_health_watch_obeys_only_the_caller_deadline() {
    for caller in [None, Some(OPENING * 3)] {
        let (app, readiness) = app();
        let mut request = tonic::Request::new(HealthCheckRequest::default());
        if let Some(budget) = caller {
            request.set_timeout(budget);
        }
        let mut watch = HealthClient::new(app)
            .watch(request)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            watch.message().await.unwrap().unwrap().status(),
            ServingStatus::NotServing
        );
        tokio::time::advance(OPENING * 2).await;
        let _refresher = readiness.prepare().await.unwrap().activate().unwrap();
        let next = tokio::time::timeout(OPENING, watch.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(next.status(), ServingStatus::Serving);
        if caller.is_some() {
            tokio::time::advance(OPENING).await;
            let status = tokio::time::timeout(OPENING, watch.message())
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        }
    }
}
