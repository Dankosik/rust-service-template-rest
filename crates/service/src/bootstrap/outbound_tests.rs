use std::net::SocketAddr;
use std::sync::Arc;

use axum::Extension;
use axum::http::{Request, StatusCode};
use infra_http::{HttpContract, RequestContext};
use infra_oauth2_client_credentials::{Algorithm, Options, Registration, ServiceTokenTarget};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use super::*;

#[derive(Clone)]
enum Resource {
    Http(Arc<infra_oauth2_client_credentials::AuthenticatedClient>),
    // template:begin outbound-auth-grpc:service-test-grpc-resource
    Grpc(Arc<infra_oauth2_client_credentials::grpc::AuthenticatedClient>),
    // template:end outbound-auth-grpc:service-test-grpc-resource
}

#[utoipa::path(
    get, path = "/_test/resource", operation_id = "testOutboundResource", security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "test-only resource wiring and failure mapping"
    }))),
    responses((status = 502, description = "Resource unavailable"))
)]
async fn resource(Extension(resource): Extension<Resource>, context: RequestContext) -> StatusCode {
    let resource_unavailable = match resource {
        Resource::Http(client) => {
            let request = Request::get("https://127.0.0.1:0/value")
                .body(infra_outbound_http::Bytes::new())
                .unwrap();
            matches!(
                client
                    .with_context(context.operation())
                    .execute(request)
                    .await,
                Err(infra_oauth2_client_credentials::Error::Resource(
                    infra_outbound_http::Error::Transport { .. }
                ))
            )
        }
        // template:begin outbound-auth-grpc:service-test-grpc-call
        Resource::Grpc(client) => grpc_unavailable(&client, context).await,
        // template:end outbound-auth-grpc:service-test-grpc-call
    };
    if resource_unavailable {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

pub(crate) fn route() -> Result<HttpContract<crate::AppState>, infra_http::FinalizeError> {
    infra_http::http_route!(resource)
}

type Build = fn(&infra_oauth2_client_credentials::ServiceTokens) -> Resource;

fn http(tokens: &infra_oauth2_client_credentials::ServiceTokens) -> Resource {
    let transport = infra_outbound_http::Client::try_new(
        &"https://127.0.0.1:0".parse().unwrap(),
        infra_outbound_http::Limits {
            operation_timeout: Duration::from_secs(5),
            response_header_count: 16,
            response_body_bytes: 1024,
        },
    )
    .unwrap();
    Resource::Http(Arc::new(tokens.http(transport).unwrap()))
}

// template:begin outbound-auth-grpc:service-test-build-grpc-resource
async fn grpc_unavailable(
    client: &infra_oauth2_client_credentials::grpc::AuthenticatedClient,
    context: RequestContext,
) -> bool {
    use grpc_contracts::example::v1::{UnaryRequest, echo_service_client::EchoServiceClient};
    use std::error::Error as _;

    let result = EchoServiceClient::new(client.with_context(context.operation()))
        .unary(UnaryRequest::default())
        .await;
    result
        .is_err_and(|status| status.code() == tonic::Code::Unavailable && status.source().is_none())
}

fn grpc(tokens: &infra_oauth2_client_credentials::ServiceTokens) -> Resource {
    use infra_outbound_grpc::{Client, ClientSecurity, ClientTlsMaterial};

    let transport = Client::try_new(
        "https://127.0.0.1:0",
        ClientSecurity::Tls(ClientTlsMaterial::default()),
        Duration::from_secs(5),
    )
    .unwrap();
    Resource::Grpc(Arc::new(tokens.grpc(transport).unwrap()))
}
// template:end outbound-auth-grpc:service-test-build-grpc-resource

async fn serve_token_peer(
    listener: TcpListener,
    requests: mpsc::Sender<oneshot::Sender<serde_json::Value>>,
    stop: CancellationToken,
) {
    #[expect(
        clippy::disallowed_methods,
        reason = "the loopback OAuth provider fixture serves its own token endpoint"
    )]
    let endpoint = axum::Router::new().route(
        "/token",
        axum::routing::post(move || {
            let requests = requests.clone();
            async move {
                let (response, receive) = oneshot::channel();
                requests.send(response).await.unwrap();
                axum::Json(receive.await.unwrap())
            }
        }),
    );
    axum::serve(listener, endpoint)
        .with_graceful_shutdown(stop.cancelled_owned())
        .await
        .unwrap();
}

async fn assert_resource_unavailable(address: SocketAddr) {
    let mut connection = TcpStream::connect(address).await.unwrap();
    connection
        .write_all(b"GET /_test/resource HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut status = String::new();
    BufReader::new(connection)
        .read_line(&mut status)
        .await
        .unwrap();
    assert!(status.starts_with("HTTP/1.1 502 "), "{status}");
}

fn token_options(peer_addr: SocketAddr) -> Options {
    Options {
        token_url: format!("http://{peer_addr}/token"),
        assertion_audience: format!("https://{peer_addr}"),
        client_id: "service".to_owned(),
        key_id: "test-key".to_owned(),
        algorithm: Algorithm::Es256,
        private_key: tls_test_support::new_issuer().key().serialize_pem().into(),
        provider_concurrency: 1,
    }
}

#[tokio::test]
async fn served_handlers_use_the_registered_grant_until_process_shutdown_joins_its_driver() {
    for build in [
        http as Build,
        // template:begin outbound-auth-grpc:service-test-grpc-driver-case
        grpc,
        // template:end outbound-auth-grpc:service-test-grpc-driver-case
    ] {
        let token_peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer_addr = token_peer.local_addr().unwrap();
        let (requests, mut tokens) = mpsc::channel(1);
        let stop_peer = CancellationToken::new();
        let peer = tokio::spawn(serve_token_peer(token_peer, requests, stop_peer.clone()));
        let (installed, lifetime) = oneshot::channel();
        let registration = crate::ServiceRegistration {
            http_resource: Some(Box::new(move |_, background| {
                let registration = Arc::new(Registration::prepare_for_test_http(token_options(
                    peer_addr,
                ))?);
                let lifetime = Arc::downgrade(&registration);
                let tokens = background.service_tokens(
                    registration,
                    ServiceTokenTarget::default(),
                    "https://127.0.0.1:0".parse().unwrap(),
                );
                let resource = build(&tokens);
                installed.send((lifetime, tokens)).unwrap();
                Ok(resource)
            })),
            // template:begin grpc:service-test-outbound-registration-defaults
            grpc: None,
            // template:end grpc:service-test-outbound-registration-defaults
        };

        let config = test_config(&[
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
        ]);
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = Dependencies::default();
        let mut serving = Serving::default();
        let prepared = start(
            &config,
            registration,
            &tests::shared_metrics(),
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap();
        let mut signals = Signals::install().unwrap();
        finish_startup(
            Ok(Some(prepared)),
            &mut signals,
            &mut background,
            &mut serving,
        )
        .unwrap();
        let (registration, retained_client) = lifetime.await.unwrap();
        assert!(registration.upgrade().is_some());
        let address = serving.app_listener.as_ref().unwrap().local_addr();
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(assert_resource_unavailable(address), async {
                tokens
                    .recv()
                    .await
                    .unwrap()
                    .send(serde_json::json!({
                        "access_token": "fixture", "token_type": "Bearer", "expires_in": 3600
                    }))
                    .unwrap();
            });
        })
        .await
        .unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(3325)).await;
        tokio::time::resume();
        let mut refresh = tokio::time::timeout(Duration::from_secs(5), async {
            let ((), refresh) = tokio::join!(assert_resource_unavailable(address), tokens.recv());
            refresh.unwrap()
        })
        .await
        .unwrap();
        assert!(!refresh.is_closed());

        let outcome = shutdown::run(shutdown::Plan {
            http_config: &config.http,
            signals: &mut signals,
            deadline: Instant::now() + config.http.grace_period,
            serving,
            background,
            dependencies,
            tracer_provider: None,
            logger: None,
        })
        .await;
        assert_eq!(outcome, Outcome::Graceful);
        assert!(registration.upgrade().is_none());
        tokio::time::timeout(Duration::from_secs(2), refresh.closed())
            .await
            .unwrap();
        stop_peer.cancel();
        peer.await.unwrap();
        drop(retained_client);
    }
}
