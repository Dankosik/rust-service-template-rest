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
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use hyper_util::rt::{TokioExecutor, TokioIo};
use infra_grpc::{Client, ClientSecurity, Operation};
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::Instant,
};
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
}

#[derive(Clone)]
struct ClientStreamPeer {
    calls: Arc<AtomicUsize>,
}

impl tonic::server::ClientStreamingService<ClientStreamRequest> for ClientStreamPeer {
    type Response = ClientStreamResponse;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send>>;

    fn call(&mut self, request: Request<tonic::Streaming<ClientStreamRequest>>) -> Self::Future {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            let mut messages = request.into_inner();
            let Some(_) = messages.message().await? else {
                return Err(Status::invalid_argument(
                    "fixture expected the first stream item",
                ));
            };
            calls.fetch_add(1, Ordering::SeqCst);
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
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let message = request.into_inner().message;
            Ok(Response::new(Box::pin(tokio_stream::iter([
                Ok(ServerStreamResponse {
                    message: format!("{message}-one"),
                }),
                Ok(ServerStreamResponse {
                    message: format!("{message}-two"),
                }),
            ]))))
        })
    }
}

struct ResourceFixture {
    address: SocketAddr,
    calls: Arc<AtomicUsize>,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl ResourceFixture {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve_peer(listener, Arc::clone(&calls), receiver));
        Self {
            address,
            calls,
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
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => result.unwrap(),
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                connections.spawn(serve_connection(stream, Arc::clone(&calls)));
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

async fn serve_connection(stream: tokio::net::TcpStream, calls: Arc<AtomicUsize>) {
    let service = service_fn(move |request| {
        let calls = Arc::clone(&calls);
        async move { Ok::<_, Infallible>(route_peer(request, calls).await) }
    });
    let connection = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), service);
    let _ = connection.await;
}

async fn route_peer(
    request: HyperRequest<Incoming>,
    calls: Arc<AtomicUsize>,
) -> HyperResponse<Body> {
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
            .server_streaming(StreamPeer { calls }, request)
            .await
        }
        "/example.v1.EchoService/ClientStream" => {
            tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                ClientStreamResponse,
                ClientStreamRequest,
            >::default())
            .client_streaming(ClientStreamPeer { calls }, request)
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
        &serde_json::json!({"access_token": "late", "token_type": "Bearer", "expires_in": 1}),
    );
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let gate = tokens.block_tokens();
    let mut client = resource.client(&credentials);
    let mut call = Box::pin(client.unary(rpc(
        UnaryRequest {
            message: "expiry".to_owned(),
        },
        Duration::from_secs(10),
    )));
    tokio::select! {
        () = tokens.token_received() => {},
        result = &mut call => panic!("expiry call completed before token release: {result:?}"),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::resume();
    gate.add_permits(1);
    assert_eq!(call.as_mut().await.unwrap_err().code(), Code::Unavailable);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn grpc_resource_authentication_statuses_pass_through_without_cache_invalidation_or_replay() {
    let tokens = Fixture::new().await;
    let resource = ResourceFixture::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut client = resource.client(&credentials);
    for (message, expected) in [
        ("unauthenticated", Code::Unauthenticated),
        ("permission-denied", Code::PermissionDenied),
    ] {
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
    }
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
async fn generated_client_stream_rejects_a_later_oversized_item_with_native_encoding_status() {
    let resource = ResourceFixture::new().await;
    let oversized = string_payload_for_encoded_len(MAX_MESSAGE_BYTES + 1);
    let mut client = resource.bare_client();
    let mut request = Request::new(tokio_stream::iter([
        ClientStreamRequest {
            message: "first".to_owned(),
        },
        ClientStreamRequest { message: oversized },
    ]));
    request.extensions_mut().insert(Operation {
        deadline: Instant::now() + Duration::from_secs(10),
    });

    let error = client.client_stream(request).await.unwrap_err();

    assert_eq!(error.code(), Code::Internal);
    assert_eq!(resource.calls(), 1);
    resource.finish().await;
}
