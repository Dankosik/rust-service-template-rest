use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::IntoResponse as _;
use axum::routing::{MethodRouter, get, put};
use axum_test::TestServer;
use infra_object_storage::{CredentialSource, ObjectStorageOptions, Provider, PutOptions};
use operation_context::Stopped;
use tokio::sync::{Notify, oneshot, watch};
use tower::ServiceExt as _;

use super::*;
use crate::test_support::body_json;

fn write_problem(error: MutationError) -> Problem {
    mutation_problem(
        error,
        || Problem::new(Code::AlreadyExists),
        || Problem::new(Code::Conflict),
    )
}

#[tokio::test]
async fn pre_header_problems_preserve_route_policy_and_uncertain_writes() {
    use MutationError::{NotApplied, OutcomeUnknown};
    use MutationFailure as F;
    use ObjectStorageError as E;

    let read = |error| read_problem(error, || Problem::new(Code::Forbidden));
    for (problem, status, code) in [
        (read(E::NotFound), 403, "forbidden"),
        (read(E::Busy), 503, "service_unavailable"),
        (read(E::Unavailable), 503, "service_unavailable"),
        (read(E::Stopped(Stopped::Deadline)), 504, "gateway_timeout"),
        (read(E::Integrity), 500, "internal_error"),
        (
            write_problem(NotApplied(F::TooLarge)),
            413,
            "request_entity_too_large",
        ),
        (
            write_problem(NotApplied(F::AlreadyExists)),
            409,
            "already_exists",
        ),
        (write_problem(NotApplied(F::Conflict)), 409, "conflict"),
    ] {
        let response = problem.into_response();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        assert_eq!(body_json(response).await["code"], code);
    }

    let refused = write_problem(NotApplied(F::Unavailable)).into_response();
    let unknown = write_problem(OutcomeUnknown(F::Unavailable)).into_response();
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unknown.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!unknown.headers().contains_key(RETRY_AFTER));
    assert!(body_json(refused).await.get("detail").is_none());
    assert_eq!(
        body_json(unknown).await["detail"],
        "the storage provider could not confirm whether the write took effect"
    );
}

async fn upload(
    State(storage): State<ObjectStorage>,
    context: RequestContext,
    request: Request,
) -> impl axum::response::IntoResponse {
    let length = request.headers()[CONTENT_LENGTH]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let key = ObjectKey::try_new("attachment").unwrap();
    let body = context.upload_body(length, request.into_body());
    storage
        .put(context.operation(), &key, body, PutOptions::overwrite())
        .await
        .map_or_else(
            |error| write_problem(error).into_response(),
            |()| StatusCode::CREATED.into_response(),
        )
}

const OPENING_BUDGET: Duration = Duration::from_secs(30);

fn file_route(
    provider_route: MethodRouter,
    file: MethodRouter<ObjectStorage>,
) -> (TestServer, Router) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the provider fixture receives S3 file operations"
    )]
    let provider = TestServer::builder()
        .http_transport()
        .build(Router::new().route("/file-bucket/attachment", provider_route));
    let storage = ObjectStorage::try_new(ObjectStorageOptions {
        provider: Provider::Local {
            endpoint: provider
                .server_address()
                .unwrap()
                .origin()
                .ascii_serialization(),
            region: String::new(),
        },
        bucket: "file-bucket".to_owned(),
        credentials: CredentialSource::AccessKey {
            access_key_id: "example".to_owned(),
            secret_access_key: "example".into(),
        },
        max_object_bytes: 1024,
        max_concurrency: 2,
        operation_timeout: Duration::from_mins(2),
    })
    .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the fixture drives one hardened file route"
    )]
    let routes = Router::new().route("/attachment", file).with_state(storage);
    let app = crate::harden(
        routes,
        &crate::HardenOptions {
            max_body_bytes: 1024,
            request_timeout: OPENING_BUDGET,
            max_in_flight: None,
            admission_wait: Duration::ZERO,
            log_health_probes: false,
            draining: None,
        },
    );
    (provider, app)
}

#[tokio::test]
async fn upload_timeout_identifies_the_pending_client_or_provider_on_one_file_route() {
    let received = Arc::new(Notify::new());
    let notify = received.clone();
    let (release_reply, hold_reply) = watch::channel(());
    let provider_handler = move |request: Request| {
        let notify = notify.clone();
        let mut hold_reply = hold_reply.clone();
        async move {
            if let Ok(bytes) = to_bytes(request.into_body(), 1024).await {
                assert!(bytes.is_empty() || bytes.windows(4).any(|window| window == b"file"));
                notify.notify_one();
                let _ = hold_reply.changed().await;
            }
            StatusCode::OK
        }
    };
    let (_provider, app) = file_route(put(provider_handler), put(upload));

    for (length, send_file) in [(4, false), (4, true), (0, false), (0, true)] {
        let (poll, polled) = oneshot::channel();
        let (send, data) = oneshot::channel::<Bytes>();
        let body = Body::from_stream(stream::once(async move {
            poll.send(()).unwrap();
            data.await
        }));
        let request = Request::put("/attachment")
            .header(CONTENT_LENGTH, length)
            .body(body)
            .unwrap();
        let response = tokio::spawn(app.clone().oneshot(request));
        tokio::time::timeout(OPENING_BUDGET, polled)
            .await
            .unwrap()
            .unwrap();
        if send_file {
            send.send(if length == 0 {
                Bytes::new()
            } else {
                Bytes::from_static(b"file")
            })
            .unwrap();
            tokio::time::timeout(OPENING_BUDGET, received.notified())
                .await
                .unwrap();
        }
        tokio::time::pause();
        tokio::time::advance(OPENING_BUDGET).await;
        let response = response.await.unwrap().unwrap();
        tokio::time::resume();
        let (status, code, connection, detail) = if send_file {
            (
                StatusCode::GATEWAY_TIMEOUT,
                "gateway_timeout",
                None,
                "the storage provider could not confirm whether the write took effect",
            )
        } else {
            (
                StatusCode::REQUEST_TIMEOUT,
                "request_timeout",
                Some(HeaderValue::from_static("close")),
                "request body was not received within the request budget",
            )
        };
        assert_eq!(response.status(), status);
        assert_eq!(response.headers().get(CONNECTION), connection.as_ref());
        assert!(!response.headers().contains_key(RETRY_AFTER));
        let problem = body_json(response).await;
        assert_eq!(problem["code"], code);
        assert_eq!(problem["detail"], detail);
    }
    drop(release_reply);
}

#[tokio::test]
async fn a_later_timeout_distinguishes_unsent_acknowledged_and_unconfirmed_puts() {
    let waiting = Arc::new(Notify::new());
    let notify = waiting.clone();
    let handler =
        move |State(storage): State<ObjectStorage>, context: RequestContext, request: Request| {
            let notify = notify.clone();
            async move {
                let body = context.buffered_upload(Bytes::new());
                if let Some(scenario) = request.uri().query() {
                    let key = ObjectKey::try_new("attachment").unwrap();
                    let options = if scenario == "unconfirmed" {
                        PutOptions::create_only()
                    } else {
                        PutOptions::overwrite()
                    };
                    let result = storage.put(context.operation(), &key, body, options).await;
                    if scenario == "unconfirmed" {
                        std::assert_matches!(result, Err(MutationError::OutcomeUnknown(_)));
                    } else {
                        result.unwrap();
                    }
                }
                notify.notify_one();
                std::future::pending::<StatusCode>().await
            }
        };
    let provider = |headers: axum::http::HeaderMap| async move {
        if headers.contains_key(axum::http::header::IF_NONE_MATCH) {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            StatusCode::OK
        }
    };
    let (_provider, app) = file_route(put(provider), put(handler));
    for uri in [
        "/attachment",
        "/attachment?acknowledged",
        "/attachment?unconfirmed",
    ] {
        let response = tokio::spawn(
            app.clone()
                .oneshot(Request::put(uri).body(Body::empty()).unwrap()),
        );
        tokio::time::timeout(OPENING_BUDGET, waiting.notified())
            .await
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(OPENING_BUDGET).await;
        let response = response.await.unwrap().unwrap();
        tokio::time::resume();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            body_json(response).await["detail"],
            if uri == "/attachment?unconfirmed" {
                "the storage provider could not confirm whether the write took effect"
            } else {
                "request budget expired before a response could be committed"
            }
        );
    }
}

#[tokio::test]
async fn file_download_can_deliver_bytes_after_the_opening_deadline() {
    let release = Arc::new(Notify::new());
    let gate = release.clone();
    let provider_handler = move || {
        let gate = gate.clone();
        async move {
            let body = Body::from_stream(stream::once(async move {
                gate.notified().await;
                Ok::<_, std::io::Error>(Bytes::from_static(b"file"))
            }));
            ([(CONTENT_LENGTH, "4")], body)
        }
    };
    let handler = |State(storage): State<ObjectStorage>, context: ResponseContext| async move {
        context
            .open_download(&storage, &ObjectKey::try_new("attachment").unwrap())
            .await
            .map(Body::new)
            .map_err(|error| read_problem(error, || Problem::new(Code::NotFound)))
    };
    let (_provider, app) = file_route(get(provider_handler), get(handler));
    let response = app
        .oneshot(Request::get("/attachment").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::pause();
    tokio::time::advance(OPENING_BUDGET + Duration::from_secs(1)).await;
    tokio::time::resume();
    release.notify_one();
    let bytes = tokio::time::timeout(OPENING_BUDGET, to_bytes(response.into_body(), 4))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&bytes[..], b"file");
}
