use std::{
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use grpc_contracts::generated::{
    ClientStreamRequest, ClientStreamResponse, ServerStreamRequest, ServerStreamResponse,
    UnaryRequest, UnaryResponse, echo_service_client::EchoServiceClient,
    echo_service_client_transport,
};
use http_body_util::{BodyExt as _, Full};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use infra_grpc::{Client, ClientSecurity, Operation};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, oneshot},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_stream::StreamExt as _;
use tonic::{Code, Request, Response, Status, body::Body, metadata::MetadataValue};
use tower::service_fn;

use super::{Credentials, Fixture};

#[derive(Clone)]
struct UnaryPeer {
    calls: Arc<AtomicUsize>,
}

impl tonic::server::UnaryService<UnaryRequest> for UnaryPeer {
    type Response = UnaryResponse;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send>>;

    fn call(&mut self, request: Request<UnaryRequest>) -> Self::Future {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            match request.into_inner().message.as_str() {
                "unauthenticated" => Err(Status::unauthenticated("resource rejected credentials")),
                "permission-denied" => Err(Status::permission_denied("resource denied access")),
                message => Ok(Response::new(UnaryResponse {
                    message: message.to_owned(),
                })),
            }
        })
    }
}

#[derive(Clone)]
struct StreamPeer {
    calls: Arc<AtomicUsize>,
    terminal: Arc<StreamTerminal>,
}

struct StreamTerminal {
    started: Semaphore,
    release: Semaphore,
}

#[derive(Clone)]
struct ClientStreamPeer {
    calls: Arc<AtomicUsize>,
    first_item_received: Arc<Semaphore>,
}

impl tonic::server::ClientStreamingService<ClientStreamRequest> for ClientStreamPeer {
    type Response = ClientStreamResponse;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send>>;

    fn call(&mut self, request: Request<tonic::Streaming<ClientStreamRequest>>) -> Self::Future {
        let calls = Arc::clone(&self.calls);
        let first_item_received = Arc::clone(&self.first_item_received);
        Box::pin(async move {
            let mut messages = request.into_inner();
            let Some(_) = messages.message().await? else {
                return Err(Status::invalid_argument(
                    "fixture expected the first stream item",
                ));
            };
            calls.fetch_add(1, Ordering::SeqCst);
            first_item_received.add_permits(1);
            while messages.message().await?.is_some() {}
            Ok(Response::new(ClientStreamResponse {
                message: "complete".to_owned(),
            }))
        })
    }
}

impl tonic::server::ServerStreamingService<ServerStreamRequest> for StreamPeer {
    type Response = ServerStreamResponse;
    type ResponseStream =
        Pin<Box<dyn tokio_stream::Stream<Item = Result<ServerStreamResponse, Status>> + Send>>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Response<Self::ResponseStream>, Status>> + Send>>;

    fn call(&mut self, request: Request<ServerStreamRequest>) -> Self::Future {
        let calls = Arc::clone(&self.calls);
        let terminal = Arc::clone(&self.terminal);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let message = request.into_inner().message;
            let first = Ok(ServerStreamResponse {
                message: format!("{message}-one"),
            });
            let response: Self::ResponseStream = Box::pin(tokio_stream::once(first).chain(
                tokio_stream::once(message).then(move |message| {
                    let terminal = Arc::clone(&terminal);
                    async move {
                        if message == "delayed-unauthenticated" {
                            terminal.started.add_permits(1);
                            tokio::time::timeout(
                                Duration::from_secs(20),
                                terminal.release.acquire(),
                            )
                            .await
                            .unwrap()
                            .unwrap()
                            .forget();
                        }
                        match message.as_str() {
                            "unauthenticated" | "delayed-unauthenticated" => {
                                Err(Status::unauthenticated("resource rejected credentials"))
                            }
                            "permission-denied" => {
                                Err(Status::permission_denied("resource denied access"))
                            }
                            _ => Ok(ServerStreamResponse {
                                message: format!("{message}-two"),
                            }),
                        }
                    }
                }),
            ));
            Ok(Response::new(response))
        })
    }
}

struct ResourceFixture {
    address: SocketAddr,
    calls: Arc<AtomicUsize>,
    first_stream_item_received: Arc<Semaphore>,
    terminal: Arc<StreamTerminal>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ResourceFixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let first_stream_item_received = Arc::new(Semaphore::new(0));
        let terminal = Arc::new(StreamTerminal {
            started: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve_peer(
            listener,
            Arc::clone(&calls),
            Arc::clone(&first_stream_item_received),
            Arc::clone(&terminal),
            receiver,
        ));
        Self {
            address,
            calls,
            first_stream_item_received,
            terminal,
            shutdown,
            task,
        }
    }

    fn client(
        &self,
        credentials: &Credentials,
    ) -> EchoServiceClient<crate::grpc::AuthenticatedClient> {
        let transport = Client::new(
            &format!("http://{}", self.address),
            ClientSecurity::Plaintext,
        )
        .unwrap();
        let transport = echo_service_client_transport(transport).unwrap();
        EchoServiceClient::new(credentials.grpc(transport))
    }

    fn bare_client(&self) -> EchoServiceClient<Client> {
        let transport = Client::new(
            &format!("http://{}", self.address),
            ClientSecurity::Plaintext,
        )
        .unwrap();
        EchoServiceClient::new(echo_service_client_transport(transport).unwrap())
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn first_stream_item_received(&self) -> Arc<Semaphore> {
        Arc::clone(&self.first_stream_item_received)
    }

    async fn finish(self) {
        self.shutdown.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn serve_peer(
    listener: TcpListener,
    calls: Arc<AtomicUsize>,
    first_stream_item_received: Arc<Semaphore>,
    terminal: Arc<StreamTerminal>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => result.unwrap(),
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                connections.spawn(serve_connection(
                    stream,
                    Arc::clone(&calls),
                    Arc::clone(&first_stream_item_received),
                    Arc::clone(&terminal),
                ));
            }
        }
    }
    connections.abort_all();
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            assert!(
                error.is_cancelled(),
                "resource peer failed before shutdown: {error}"
            );
        }
    }
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    calls: Arc<AtomicUsize>,
    first_stream_item_received: Arc<Semaphore>,
    terminal: Arc<StreamTerminal>,
) {
    let service = service_fn(move |request| {
        let calls = Arc::clone(&calls);
        let first_stream_item_received = Arc::clone(&first_stream_item_received);
        let terminal = Arc::clone(&terminal);
        async move {
            Ok::<_, Infallible>(
                route_peer(request, calls, first_stream_item_received, terminal).await,
            )
        }
    });
    let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service));
    let _ = connection.await;
}

async fn route_peer(
    request: HyperRequest<Incoming>,
    calls: Arc<AtomicUsize>,
    first_stream_item_received: Arc<Semaphore>,
    terminal: Arc<StreamTerminal>,
) -> HyperResponse<Body> {
    if let Some(status) = request.headers().get("x-fixture-http-status") {
        calls.fetch_add(1, Ordering::SeqCst);
        let mut response = HyperResponse::builder()
            .status(http::StatusCode::from_bytes(status.as_bytes()).unwrap())
            .header("content-type", "application/grpc");
        if let Some(status) = request.headers().get("x-fixture-initial-status") {
            response = response.header("grpc-status", status);
        }
        let body = if request.headers().contains_key("x-fixture-body-error") {
            // A valid empty protobuf message followed by a reset, never a peer status.
            Body::new(
                Full::new(bytes::Bytes::from_static(&[0, 0, 0, 0, 0]))
                    .map_err(|never| match never {})
                    .with_trailers(async move {
                        tokio::time::timeout(Duration::from_secs(20), terminal.release.acquire())
                            .await
                            .unwrap()
                            .unwrap()
                            .forget();
                        Some(Err(Status::unavailable("fixture body failure")))
                    }),
            )
        } else if request.headers().contains_key("x-fixture-trailers") {
            let mut trailers = http::HeaderMap::new();
            trailers.insert("peer-proof", http::HeaderValue::from_static("preserved"));
            if let Some(status) = request.headers().get("x-fixture-terminal-status") {
                trailers.insert("grpc-status", status.clone());
            }
            Body::new(Body::empty().with_trailers(std::future::ready(Some(Ok(trailers)))))
        } else {
            Body::empty()
        };
        return response.body(body).unwrap();
    }
    match request.uri().path() {
        "/example.v1.EchoService/Unary" => {
            tonic::server::Grpc::new(
                tonic_prost::ProstCodec::<UnaryResponse, UnaryRequest>::default(),
            )
            .unary(UnaryPeer { calls }, request)
            .await
        }
        "/example.v1.EchoService/ServerStream" => {
            tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                ServerStreamResponse,
                ServerStreamRequest,
            >::default())
            .server_streaming(StreamPeer { calls, terminal }, request)
            .await
        }
        "/example.v1.EchoService/ClientStream" => {
            tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                ClientStreamResponse,
                ClientStreamRequest,
            >::default())
            .client_streaming(
                ClientStreamPeer {
                    calls,
                    first_item_received: first_stream_item_received,
                },
                request,
            )
            .await
        }
        _ => {
            let (parts, ()) = Status::unimplemented("resource method unavailable")
                .into_http::<()>()
                .into_parts();
            HyperResponse::from_parts(parts, Body::empty())
        }
    }
}

fn rpc<T>(message: T, after: Duration) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Operation {
        deadline: Instant::now() + after,
    });
    request
}

const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

fn varint_len(mut value: usize) -> usize {
    let mut bytes = 1;
    while value >= 0x80 {
        bytes += 1;
        value >>= 7;
    }
    bytes
}

fn string_payload_for_encoded_len(encoded_len: usize) -> String {
    let payload_len = encoded_len - 5;
    assert_eq!(1 + varint_len(payload_len) + payload_len, encoded_len);
    "x".repeat(payload_len)
}

fn stream_with_later_oversized_item(
    first_stream_item_received: Arc<Semaphore>,
    oversized: String,
) -> impl tokio_stream::Stream<Item = ClientStreamRequest> {
    tokio_stream::iter([ClientStreamRequest {
        message: "first".to_owned(),
    }])
    .chain(
        tokio_stream::once(ClientStreamRequest { message: oversized }).then(move |item| {
            let received = Arc::clone(&first_stream_item_received);
            async move {
                tokio::time::timeout(Duration::from_secs(2), received.acquire_owned())
                    .await
                    .unwrap()
                    .unwrap()
                    .forget();
                item
            }
        }),
    )
}

#[tokio::test]
async fn grpc_authorization_conflict_stops_before_token_or_resource_io() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut request = rpc(
        UnaryRequest {
            message: "conflict".to_owned(),
        },
        Duration::from_secs(10),
    );
    request.metadata_mut().insert(
        "authorization",
        MetadataValue::try_from("Bearer caller-token").unwrap(),
    );
    let error = resource
        .client(&credentials)
        .unary(request)
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(tokens.token_requests().is_empty());
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_token_wait_spends_the_original_deadline_and_never_dispatches() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let gate = tokens.block_tokens();
    let mut client = resource.client(&credentials);
    let mut call = Box::pin(client.unary(rpc(
        UnaryRequest {
            message: "deadline".to_owned(),
        },
        Duration::from_secs(1),
    )));
    tokio::select! {
        () = tokens.token_received() => {},
        result = &mut call => panic!("deadline call completed before token release: {result:?}"),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        call.as_mut().await.unwrap_err().code(),
        Code::DeadlineExceeded
    );
    tokio::time::resume();
    gate.add_permits(1);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_token_acquisition_failure_has_a_safe_status_and_no_resource_dispatch() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        "500 Internal Server Error",
        &serde_json::json!({"error": "private"}),
    );
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let error = resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "failure".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_dispatches_one_sensitive_cached_bearer_to_the_real_resource_boundary() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let response = resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "success".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    let cached = credentials
        .acquire(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(response.message, "success");
    assert_eq!(tokens.token_requests().len(), 1);
    assert!(cached.header.is_sensitive());
    assert_eq!(resource.calls(), 1);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_hard_expiry_before_dispatch_refuses_the_resource_call() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "cached", "token_type": "Bearer", "expires_in": 60}),
    );
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut client = resource.client(&credentials);
    let response = client
        .unary(rpc(
            UnaryRequest {
                message: "warm".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.message, "warm");

    // Moka's real clock retains the valid cached value while Tokio's clock
    // moves past its hard expiry, isolating the adapter's pre-dispatch check.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    let error = client
        .unary(rpc(
            UnaryRequest {
                message: "expiry".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 1);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_initial_authentication_statuses_pass_through_and_only_rejection_evicts() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    for (message, expected, extra_fetches) in [
        ("unauthenticated", Code::Unauthenticated, 1),
        ("permission-denied", Code::PermissionDenied, 0),
    ] {
        let credentials = tokens.credentials(&[], None);
        let mut client = resource.client(&credentials);
        let before_tokens = tokens.token_requests().len();
        let before_calls = resource.calls();
        let error = client
            .unary(rpc(
                UnaryRequest {
                    message: message.to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(tokens.token_requests().len(), before_tokens + 1);
        assert_eq!(resource.calls(), before_calls + 1);
        client
            .unary(rpc(
                UnaryRequest {
                    message: "success".to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        assert_eq!(
            tokens.token_requests().len(),
            before_tokens + 1 + extra_fetches
        );
        assert_eq!(resource.calls(), before_calls + 2);
    }
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_terminal_authentication_status_evicts_before_the_next_call_without_replay() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    for (message, expected, extra_fetches) in [
        ("unauthenticated", Code::Unauthenticated, 1),
        ("permission-denied", Code::PermissionDenied, 0),
    ] {
        let credentials = tokens.credentials(&[], None);
        let before_tokens = tokens.token_requests().len();
        let before_calls = resource.calls();
        let mut stream = resource
            .client(&credentials)
            .server_stream(rpc(
                ServerStreamRequest {
                    message: message.to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            stream.message().await.unwrap().unwrap().message,
            format!("{message}-one")
        );
        assert_eq!(tokens.token_requests().len(), before_tokens + 1);
        let error = stream.message().await.unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(
            error.message(),
            if extra_fetches == 1 {
                "resource rejected credentials"
            } else {
                "resource denied access"
            }
        );
        assert_eq!(tokens.token_requests().len(), before_tokens + 1);
        assert_eq!(resource.calls(), before_calls + 1);
        resource
            .client(&credentials)
            .unary(rpc(
                UnaryRequest {
                    message: "success".to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        assert_eq!(
            tokens.token_requests().len(),
            before_tokens + 1 + extra_fetches
        );
        assert_eq!(resource.calls(), before_calls + 2);
    }
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_late_rejection_of_an_old_stream_spares_the_newer_cached_credential() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        "200 OK",
        &serde_json::json!({
            "access_token": "first", "token_type": "Bearer", "expires_in": 1,
        }),
    );
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut stream = resource
        .client(&credentials)
        .server_stream(rpc(
            ServerStreamRequest {
                message: "delayed-unauthenticated".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(stream.message().await.unwrap().is_some());
    tokio::time::timeout(Duration::from_secs(2), resource.terminal.started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    tokens.token_json(
        "200 OK",
        &serde_json::json!({
            "access_token": "second", "token_type": "Bearer", "expires_in": 60,
        }),
    );
    resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "success".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap();
    assert_eq!(tokens.token_requests().len(), 2);

    resource.terminal.release.add_permits(1);
    assert_eq!(
        stream.message().await.unwrap_err().code(),
        Code::Unauthenticated
    );
    resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "success".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap();
    assert_eq!(tokens.token_requests().len(), 2);
    assert_eq!(resource.calls(), 3);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_deadline_and_unread_drop_do_not_invalidate_stream_credentials() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    for expire in [false, true] {
        let credentials = tokens.credentials(&[], None);
        let before_tokens = tokens.token_requests().len();
        let mut stream = resource
            .client(&credentials)
            .server_stream(rpc(
                ServerStreamRequest {
                    message: "delayed-unauthenticated".to_owned(),
                },
                Duration::from_secs(1),
            ))
            .await
            .unwrap()
            .into_inner();
        assert!(stream.message().await.unwrap().is_some());
        tokio::time::timeout(Duration::from_secs(2), resource.terminal.started.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        if expire {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(2)).await;
            assert_eq!(
                stream.message().await.unwrap_err().code(),
                Code::DeadlineExceeded
            );
            tokio::time::resume();
        }
        drop(stream);
        resource
            .client(&credentials)
            .unary(rpc(
                UnaryRequest {
                    message: "success".to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        assert_eq!(tokens.token_requests().len(), before_tokens + 1);
    }
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_http_fallback_requires_absent_status_and_completes_without_hanging() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    for (http_status, initial, terminal, trailers, expected, extra_fetches) in [
        ("401", None, None, false, Code::Unauthenticated, 1),
        ("401", None, None, true, Code::Unauthenticated, 1),
        ("200", None, None, false, Code::Unknown, 0),
        ("403", None, None, false, Code::PermissionDenied, 0),
        ("401", Some("0"), Some("16"), true, Code::Internal, 0),
        ("401", Some("7"), None, false, Code::PermissionDenied, 0),
        ("401", Some("invalid"), None, false, Code::Unknown, 0),
        ("401", None, Some("7"), true, Code::PermissionDenied, 0),
        ("401", None, Some("0"), true, Code::Internal, 0),
        ("401", None, Some("invalid"), true, Code::Unknown, 0),
    ] {
        let credentials = tokens.credentials(&[], None);
        let before_tokens = tokens.token_requests().len();
        let before_calls = resource.calls();
        let mut request = rpc(UnaryRequest::default(), Duration::from_secs(10));
        request.metadata_mut().insert(
            "x-fixture-http-status",
            MetadataValue::from_static(http_status),
        );
        if let Some(status) = initial {
            request.metadata_mut().insert(
                "x-fixture-initial-status",
                MetadataValue::from_static(status),
            );
        }
        if trailers {
            request
                .metadata_mut()
                .insert("x-fixture-trailers", MetadataValue::from_static("yes"));
        }
        if let Some(status) = terminal {
            request.metadata_mut().insert(
                "x-fixture-terminal-status",
                MetadataValue::from_static(status),
            );
        }
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            resource.client(&credentials).unary(request),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(
            error.code(),
            expected,
            "HTTP {http_status}, initial {initial:?}, terminal {terminal:?}"
        );
        if terminal == Some("7") {
            assert_eq!(error.metadata().get("peer-proof").unwrap(), "preserved");
        }
        assert_eq!(tokens.token_requests().len(), before_tokens + 1);
        assert_eq!(resource.calls(), before_calls + 1);
        resource
            .client(&credentials)
            .unary(rpc(
                UnaryRequest {
                    message: "success".to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        assert_eq!(
            tokens.token_requests().len(),
            before_tokens + 1 + extra_fetches
        );
        assert_eq!(resource.calls(), before_calls + 2);
    }
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_body_failure_does_not_turn_http_401_into_a_remote_rejection() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut request = rpc(ServerStreamRequest::default(), Duration::from_secs(10));
    request
        .metadata_mut()
        .insert("x-fixture-http-status", MetadataValue::from_static("401"));
    request
        .metadata_mut()
        .insert("x-fixture-body-error", MetadataValue::from_static("yes"));
    let mut stream = resource
        .client(&credentials)
        .server_stream(request)
        .await
        .unwrap()
        .into_inner();
    assert!(stream.message().await.unwrap().is_some());
    resource.terminal.release.add_permits(1);
    let error = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_ne!(error.code(), Code::Unauthenticated);
    assert!(!error.message().contains("fixture body failure"));
    resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "success".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap();
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_stream_authorizes_once_at_opening_without_refresh_on_later_polls() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut stream = resource
        .client(&credentials)
        .server_stream(rpc(
            ServerStreamRequest {
                message: "stream".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        stream.message().await.unwrap().unwrap().message,
        "stream-one"
    );
    assert_eq!(
        stream.message().await.unwrap().unwrap().message,
        "stream-two"
    );
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 1);
    assert!(stream.message().await.unwrap().is_none());
    drop(stream);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn generated_unary_client_enforces_the_four_mebib_encoded_limit_for_bare_and_oauth_transports()
 {
    let resource = ResourceFixture::new().await;
    let exact = string_payload_for_encoded_len(MAX_MESSAGE_BYTES);
    let oversized = format!("{exact}x");

    let mut bare = resource.bare_client();
    let response = bare
        .unary(rpc(
            UnaryRequest {
                message: exact.clone(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.message.len(), exact.len());
    assert_eq!(resource.calls(), 1);
    let error = bare
        .unary(rpc(
            UnaryRequest {
                message: oversized.clone(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(resource.calls(), 1);

    let tokens = Fixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut oauth = resource.client(&credentials);
    let response = oauth
        .unary(rpc(
            UnaryRequest { message: exact },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(response.message.len(), MAX_MESSAGE_BYTES - 5);
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 2);
    let error = oauth
        .unary(rpc(
            UnaryRequest { message: oversized },
            Duration::from_secs(10),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn generated_client_stream_rejects_a_later_oversized_item_for_bare_and_oauth_transports() {
    let resource = ResourceFixture::new().await;
    let oversized = string_payload_for_encoded_len(MAX_MESSAGE_BYTES + 1);
    let mut client = resource.bare_client();
    let mut request = Request::new(stream_with_later_oversized_item(
        resource.first_stream_item_received(),
        oversized.clone(),
    ));
    request.extensions_mut().insert(Operation {
        deadline: Instant::now() + Duration::from_secs(10),
    });

    let error = client.client_stream(request).await.unwrap_err();

    assert_eq!(error.code(), Code::Internal);
    assert_eq!(resource.calls(), 1);

    let tokens = Fixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut client = resource.client(&credentials);
    let mut request = Request::new(stream_with_later_oversized_item(
        resource.first_stream_item_received(),
        oversized,
    ));
    request.extensions_mut().insert(Operation {
        deadline: Instant::now() + Duration::from_secs(10),
    });

    let error = client.client_stream(request).await.unwrap_err();

    assert_eq!(error.code(), Code::Internal);
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}
