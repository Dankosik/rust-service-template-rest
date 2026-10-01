use std::{
    convert::Infallible,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use grpc_contracts::example::v1::{
    UnaryRequest, UnaryResponse, echo_service_client::EchoServiceClient,
};
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use infra_grpc::{Client, ClientSecurity};
use tokio::{
    net::TcpListener,
    sync::{Notify, oneshot},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tonic::{Code, Request, Response, Status, body::Body};
use tower::service_fn;

use secrecy::SecretString;

use super::{Credentials, Fixture};
use crate::{AcquisitionError, OnBehalfOf, Rejection};

#[derive(Clone)]
struct Peer {
    calls: Arc<AtomicUsize>,
    authorizations: Arc<Mutex<Vec<String>>>,
    timeouts: Arc<Mutex<Vec<String>>>,
    started: Arc<Notify>,
    release: Arc<Notify>,
    released: Arc<std::sync::atomic::AtomicBool>,
}

impl Peer {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            authorizations: Arc::new(Mutex::new(Vec::new())),
            timeouts: Arc::new(Mutex::new(Vec::new())),
            started: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn record(&self, authorization: Option<String>) {
        if let Some(authorization) = authorization {
            self.authorizations.lock().unwrap().push(authorization);
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    async fn wait_to_answer(&self) {
        self.started.notify_waiters();
        let notified = self.release.notified();
        if !self.released.load(Ordering::Acquire) {
            notified.await;
        }
    }
}

impl tonic::server::UnaryService<UnaryRequest> for Peer {
    type Response = UnaryResponse;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Self::Response>, Status>> + Send>>;

    fn call(&mut self, request: Request<UnaryRequest>) -> Self::Future {
        let peer = self.clone();
        Box::pin(async move {
            let authorization = request
                .metadata()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let message = request.into_inner().message;
            if message == "delayed-unauthenticated" {
                peer.record(authorization);
                peer.wait_to_answer().await;
                return Err(Status::unauthenticated("resource rejected credentials"));
            }
            peer.record(authorization);
            match message.as_str() {
                "unauthenticated" => Err(Status::unauthenticated("resource rejected credentials")),
                "permission-denied" => Err(Status::permission_denied("resource denied access")),
                other => Ok(Response::new(UnaryResponse {
                    message: other.to_owned(),
                })),
            }
        })
    }
}

struct Resource {
    address: SocketAddr,
    peer: Peer,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Resource {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = Peer::new();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve(listener, peer.clone(), receiver));
        Self {
            address,
            peer,
            shutdown,
            task,
        }
    }

    fn client(
        &self,
        credentials: &Credentials,
    ) -> EchoServiceClient<crate::grpc::AuthenticatedClient> {
        let channel = Client::new(
            &format!("http://{}", self.address),
            ClientSecurity::Plaintext,
        )
        .unwrap();
        EchoServiceClient::new(credentials.grpc(channel))
    }

    fn calls(&self) -> usize {
        self.peer.calls.load(Ordering::SeqCst)
    }

    fn authorizations(&self) -> Vec<String> {
        self.peer.authorizations.lock().unwrap().clone()
    }

    fn release(&self) {
        self.peer.released.store(true, Ordering::Release);
        self.peer.release.notify_waiters();
    }

    async fn started(&self) {
        let notified = self.peer.started.notified();
        if self.peer.calls.load(Ordering::Acquire) == 0 {
            notified.await;
        }
    }

    async fn finish(self) {
        self.release();
        self.shutdown.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn serve(listener: TcpListener, peer: Peer, mut shutdown: oneshot::Receiver<()>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                result.unwrap();
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                connections.spawn(serve_connection(stream, peer.clone()));
            }
        }
    }
    connections.abort_all();
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            assert!(error.is_cancelled(), "resource peer failed: {error}");
        }
    }
}

async fn serve_connection(stream: tokio::net::TcpStream, peer: Peer) {
    let service = service_fn(move |request| {
        let peer = peer.clone();
        async move { Ok::<_, Infallible>(route(request, peer).await) }
    });
    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service))
        .await;
}

async fn route(request: HyperRequest<Incoming>, peer: Peer) -> HyperResponse<Body> {
    if let Some(status) = request.headers().get("x-fixture-http-status") {
        peer.record(header_string(request.headers(), "authorization"));
        let mut response = HyperResponse::builder()
            .status(http::StatusCode::from_bytes(status.as_bytes()).unwrap())
            .header("content-type", "application/grpc");
        if let Some(code) = request.headers().get("x-fixture-grpc-status") {
            response = response.header("grpc-status", code);
        }
        return response.body(Body::empty()).unwrap();
    }
    if request.uri().path() == "/example.v1.EchoService/Unary" {
        if let Some(timeout) = header_string(request.headers(), "grpc-timeout") {
            peer.timeouts.lock().unwrap().push(timeout);
        }
        return tonic::server::Grpc::new(
            tonic_prost::ProstCodec::<UnaryResponse, UnaryRequest>::default(),
        )
        .unary(peer, request)
        .await;
    }
    let (parts, ()) = Status::unimplemented("resource method unavailable")
        .into_http::<()>()
        .into_parts();
    HyperResponse::from_parts(parts, Body::empty())
}

fn header_string(headers: &http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn rpc<T>(message: T, after: Duration) -> Request<T> {
    let mut request = Request::new(message);
    request.set_timeout(after);
    request
}

#[tokio::test]
async fn caller_authorization_is_invalid_argument_before_any_io() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let mut request = rpc(
        UnaryRequest {
            message: "conflict".to_owned(),
        },
        Duration::from_secs(10),
    );
    request
        .metadata_mut()
        .insert("authorization", "Bearer caller-token".parse().unwrap());
    let error = resource
        .client(&tokens.credentials(&[], None))
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
async fn token_acquisition_spends_grpc_timeout_without_dispatch() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let gate = tokens.block_tokens();
    let credentials = tokens.credentials(&[], None);
    let mut client = resource.client(&credentials);
    let mut call = Box::pin(client.unary(rpc(
        UnaryRequest {
            message: "deadline".to_owned(),
        },
        Duration::from_secs(1),
    )));
    tokio::select! {
        () = tokens.token_received() => {}
        result = &mut call => panic!("call finished before the token arrived: {result:?}"),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(call.await.unwrap_err().code(), Code::DeadlineExceeded);
    tokio::time::resume();
    gate.add_permits(1);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn acquisition_failure_prevents_dispatch_and_reports_whether_it_may_pass_later() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    for (status, body, code, reason) in [
        (
            "500 Internal Server Error",
            serde_json::json!({"error": "private"}),
            Code::Unavailable,
            AcquisitionError::Unavailable,
        ),
        (
            "400 Bad Request",
            serde_json::json!({"error": "invalid_client", "error_description": "private"}),
            Code::Unauthenticated,
            AcquisitionError::Rejected(Rejection::InvalidClient),
        ),
    ] {
        tokens.token_json(status, &body);
        let error = resource
            .client(&tokens.credentials(&[], None))
            .unary(rpc(
                UnaryRequest {
                    message: "failure".to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code(), code);
        assert!(!error.message().contains("private"));
        // The closed reason is the status source; it is never sent to a peer.
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<AcquisitionError>());
        assert_eq!(source, Some(&reason));
    }
    assert_eq!(tokens.token_requests().len(), 2);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn one_cached_bearer_is_sent_and_reused() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut client = resource.client(&credentials);
    let first = client
        .unary(rpc(
            UnaryRequest {
                message: "one".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    tokens.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "other", "token_type": "Bearer", "expires_in": 60}),
    );
    let second = client
        .unary(rpc(
            UnaryRequest {
                message: "two".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first.message, "one");
    assert_eq!(second.message, "two");
    assert_eq!(tokens.token_requests().len(), 1);
    assert_eq!(
        resource.authorizations(),
        ["Bearer fixture-token", "Bearer fixture-token"]
    );
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_token_past_its_reuse_cutoff_is_refreshed_before_dispatch() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "cached", "token_type": "Bearer", "expires_in": 60}),
    );
    let resource = Resource::new().await;
    let mut client = resource.client(&tokens.credentials(&[], None));
    for message in ["warm", "reused"] {
        client
            .unary(rpc(
                UnaryRequest {
                    message: message.to_owned(),
                },
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
    }
    assert_eq!(tokens.token_requests().len(), 1);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(51)).await;
    tokio::time::resume();
    client
        .unary(rpc(
            UnaryRequest {
                message: "refreshed".to_owned(),
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
async fn token_wait_is_subtracted_from_the_propagated_grpc_timeout() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let gate = tokens.block_tokens();
    let mut client = resource.client(&tokens.credentials(&[], None));
    let mut call = Box::pin(client.unary(rpc(
        UnaryRequest {
            message: "budget".to_owned(),
        },
        Duration::from_secs(1),
    )));
    tokio::select! {
        () = tokens.token_received() => {}
        result = &mut call => panic!("call finished before the token arrived: {result:?}"),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_millis(600)).await;
    tokio::time::resume();
    gate.add_permits(1);
    call.await.unwrap();
    let timeouts = resource.peer.timeouts.lock().unwrap().clone();
    let [timeout] = timeouts.as_slice() else {
        panic!("expected one propagated timeout: {timeouts:?}");
    };
    let millis = timeout.strip_suffix('m').unwrap().parse::<u64>().unwrap();
    assert!((1..=400).contains(&millis), "propagated {timeout}");
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_reused_token_forwards_the_callers_grpc_timeout_unchanged() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let mut client = resource.client(&tokens.credentials(&[], None));
    for _ in 0..4 {
        client
            .unary(rpc(
                UnaryRequest {
                    message: "budget".to_owned(),
                },
                Duration::from_secs(1),
            ))
            .await
            .unwrap();
    }
    let timeouts = resource.peer.timeouts.lock().unwrap().clone();
    // Tonic encodes one second as microseconds. A reuse that a scheduler pause
    // stretches past a millisecond is rewritten, so one unchanged value proves it.
    assert!(
        timeouts[1..].iter().any(|timeout| timeout == "1000000u"),
        "{timeouts:?}"
    );
    resource.finish().await;
    tokens.finish().await;
}

/// Acquires the service token and makes it old enough to be evicted.
async fn warm_and_age(credentials: &Credentials) {
    credentials
        .service_token(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    super::advance(crate::EVICTION_MIN_AGE).await;
}

#[tokio::test]
async fn trailers_only_unauthenticated_evicts_without_replay_and_permission_denied_keeps_the_token()
{
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    for (message, expected, extra_fetches) in [
        ("unauthenticated", Code::Unauthenticated, 1),
        ("permission-denied", Code::PermissionDenied, 0),
    ] {
        let credentials = tokens.credentials(&[], None);
        let mut client = resource.client(&credentials);
        let before_tokens = tokens.token_requests().len();
        // Old enough for an unauthenticated answer to evict it.
        warm_and_age(&credentials).await;
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
                    message: "again".to_owned(),
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
async fn a_late_rejection_does_not_evict_a_newer_cached_token() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let resource = Resource::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut client = resource.client(&credentials);
    // Old enough that only its replacement keeps the late answer from evicting.
    warm_and_age(&credentials).await;
    let mut delayed = Box::pin(client.unary(rpc(
        UnaryRequest {
            message: "delayed-unauthenticated".to_owned(),
        },
        Duration::from_secs(10),
    )));
    tokio::select! {
        () = resource.started() => {}
        result = &mut delayed => panic!("delayed call finished before the gate: {result:?}"),
    }
    *credentials.cached() = crate::Cached::default();
    tokens.token_json(
        "200 OK",
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    credentials
        .service_token(Instant::now() + Duration::from_secs(10))
        .await
        .unwrap();
    resource.release();
    assert_eq!(delayed.await.unwrap_err().code(), Code::Unauthenticated);
    resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "kept".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap();
    assert_eq!(tokens.token_requests().len(), 2);
    assert_eq!(resource.authorizations().last().unwrap(), "Bearer second");
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn http_401_without_grpc_status_evicts_and_another_status_does_not() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    for (grpc_status, extra_fetches) in [(None, 1), (Some("7"), 0)] {
        let credentials = tokens.credentials(&[], None);
        let before_tokens = tokens.token_requests().len();
        let before_calls = resource.calls();
        // Old enough for an unauthenticated answer to evict it.
        warm_and_age(&credentials).await;
        let mut request = rpc(UnaryRequest::default(), Duration::from_secs(10));
        request
            .metadata_mut()
            .insert("x-fixture-http-status", "401".parse().unwrap());
        if let Some(status) = grpc_status {
            request
                .metadata_mut()
                .insert("x-fixture-grpc-status", status.parse().unwrap());
        }
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            resource.client(&credentials).unary(request),
        )
        .await
        .expect("http fixture completes")
        .unwrap_err();
        if grpc_status == Some("7") {
            assert_eq!(error.code(), Code::PermissionDenied);
        }
        assert_eq!(tokens.token_requests().len(), before_tokens + 1);
        assert_eq!(resource.calls(), before_calls + 1);
        resource
            .client(&credentials)
            .unary(rpc(
                UnaryRequest {
                    message: "again".to_owned(),
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
async fn on_behalf_of_dispatches_the_exchanged_token_instead_of_the_service_token() {
    let tokens = Fixture::new().await;
    tokens.token_json("200 OK", &super::exchange_response("exchanged-token"));
    let resource = Resource::new().await;
    let credentials = tokens.credentials(&[], None);
    let mut request = rpc(
        UnaryRequest {
            message: "one".to_owned(),
        },
        Duration::from_secs(10),
    );
    request
        .extensions_mut()
        .insert(OnBehalfOf::new(SecretString::from("subject-token")));
    resource.client(&credentials).unary(request).await.unwrap();
    assert_eq!(resource.authorizations(), ["Bearer exchanged-token"]);
    assert_eq!(tokens.token_requests().len(), 1);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_client_requiring_a_subject_is_invalid_argument_without_one_before_any_io() {
    let tokens = Fixture::new().await;
    tokens.token_json("200 OK", &super::exchange_response("exchanged-token"));
    let resource = Resource::new().await;
    let credentials = tokens.credentials(&[], None);
    let channel = Client::new(
        &format!("http://{}", resource.address),
        ClientSecurity::Plaintext,
    )
    .unwrap();
    let mut client = EchoServiceClient::new(credentials.grpc(channel).require_on_behalf_of());
    let message = || UnaryRequest {
        message: "one".to_owned(),
    };
    let error = client
        .unary(rpc(message(), Duration::from_secs(10)))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert!(tokens.token_requests().is_empty());
    assert_eq!(resource.calls(), 0);

    let mut request = rpc(message(), Duration::from_secs(10));
    request
        .extensions_mut()
        .insert(OnBehalfOf::new(SecretString::from("subject-token")));
    client.unary(request).await.unwrap();
    assert_eq!(resource.authorizations(), ["Bearer exchanged-token"]);
    resource.finish().await;
    tokens.finish().await;
}
