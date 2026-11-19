use std::assert_matches;
use std::{
    convert::Infallible,
    future::poll_fn,
    net::SocketAddr,
    num::NonZeroUsize,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use grpc_contracts::example::v1::{
    UnaryRequest, UnaryResponse, echo_service_client::EchoServiceClient,
};
use http::StatusCode;
use hyper::{Request as HyperRequest, Response as HyperResponse, body::Incoming};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use infra_outbound_grpc::{Client, ClientSecurity, ClientTimeout, ClientTlsMaterial};
use operation_context::OperationContext;
use tls_test_support::TlsMaterial;
use tokio::{
    net::TcpListener,
    sync::{Notify, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, crypto::aws_lc_rs, pki_types::PrivatePkcs8KeyDer},
};
use tonic::{Code, Request, Response, Status, body::Body};
use tower::{Service as _, service_fn};

use super::{Fixture, acquisition_deadline, nonzero, prepare_registration, subject, token_context};
use crate::{AcquisitionError, ExchangeTokenTarget, Rejection, ServiceTokenTarget, ServiceTokens};

#[derive(Clone)]
struct Peer {
    calls: Arc<AtomicUsize>,
    authorizations: Arc<Mutex<Vec<String>>>,
    timeouts: Arc<Mutex<Vec<Duration>>>,
    started: Arc<Notify>,
    released: watch::Sender<bool>,
}

impl Peer {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            authorizations: Arc::new(Mutex::new(Vec::new())),
            timeouts: Arc::new(Mutex::new(Vec::new())),
            started: Arc::new(Notify::new()),
            released: watch::Sender::new(false),
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
        self.released
            .subscribe()
            .wait_for(|released| *released)
            .await
            .expect("peer owns the release gate");
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
    root_pem: String,
    peer: Peer,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Resource {
    fn origin(&self) -> http::Uri {
        format!("https://{}", self.address).parse().unwrap()
    }

    fn credentials(&self, tokens: &Fixture) -> ServiceTokens {
        tokens.service_tokens_with(tokens.options(), &[], self.origin())
    }

    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let material = TlsMaterial::new("127.0.0.1");
        let mut config =
            ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![material.cert.into()],
                    PrivatePkcs8KeyDer::from(material.key).into(),
                )
                .unwrap();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let peer = Peer::new();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(serve(
            listener,
            TlsAcceptor::from(Arc::new(config)),
            peer.clone(),
            receiver,
        ));
        Self {
            address,
            root_pem: material.root_pem,
            peer,
            shutdown,
            task,
        }
    }

    fn client(
        &self,
        credentials: &ServiceTokens,
    ) -> EchoServiceClient<crate::grpc::AuthenticatedClient> {
        EchoServiceClient::new(self.transport(credentials))
    }

    fn transport(&self, credentials: &ServiceTokens) -> crate::grpc::AuthenticatedClient {
        let channel = self.channel(ClientTimeout::FullRpc(Duration::from_secs(10)));
        credentials.grpc(channel).unwrap()
    }

    fn channel(&self, timeout: ClientTimeout) -> Client {
        self.channel_with_limit(timeout, None)
    }

    fn channel_with_limit(&self, timeout: ClientTimeout, limit: Option<NonZeroUsize>) -> Client {
        Client::builder(
            &format!("https://{}", self.address),
            ClientSecurity::Tls(ClientTlsMaterial {
                ca_certificate_pem: Some(&self.root_pem),
                identity: None,
            }),
            timeout,
        )
        .opening_limit(limit)
        .build()
        .unwrap()
    }

    fn calls(&self) -> usize {
        self.peer.calls.load(Ordering::SeqCst)
    }

    fn authorizations(&self) -> Vec<String> {
        self.peer.authorizations.lock().unwrap().clone()
    }

    fn release(&self) {
        self.peer.released.send_replace(true);
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

async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    peer: Peer,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                result.unwrap();
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.unwrap();
                connections.spawn(serve_connection(stream, acceptor.clone(), peer.clone()));
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

async fn serve_connection(stream: tokio::net::TcpStream, acceptor: TlsAcceptor, peer: Peer) {
    let Ok(stream) = acceptor.accept(stream).await else {
        return;
    };
    let service = service_fn(move |request| {
        let peer = peer.clone();
        async move { Ok::<_, Infallible>(route(request, peer).await) }
    });
    let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(service))
        .await;
}

struct GatedTrailers {
    release: Pin<Box<dyn Future<Output = ()> + Send>>,
    status: http::HeaderValue,
    finished: bool,
}

impl hyper::body::Body for GatedTrailers {
    type Data = bytes::Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        if self.finished {
            return Poll::Ready(None);
        }
        if self.release.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        self.finished = true;
        let mut trailers = http::HeaderMap::new();
        if self.status.is_empty() {
            trailers.insert("x-fixture-terminal", http::HeaderValue::from_static("done"));
        } else {
            trailers.insert("grpc-status", self.status.clone());
        }
        Poll::Ready(Some(Ok(hyper::body::Frame::trailers(trailers))))
    }
}

async fn route(request: HyperRequest<Incoming>, peer: Peer) -> HyperResponse<Body> {
    if request.uri().path() == "/budget-stream" {
        peer.record(header_string(request.headers(), "authorization"));
        if let Some(timeout) = grpc_timeout(request.headers()) {
            peer.timeouts.lock().unwrap().push(timeout);
        }
        let status = request
            .headers()
            .get("x-fixture-grpc-status")
            .cloned()
            .unwrap_or_else(|| http::HeaderValue::from_static("0"));
        return HyperResponse::builder()
            .header("content-type", "application/grpc")
            .body(Body::new(GatedTrailers {
                release: Box::pin(async move { peer.wait_to_answer().await }),
                status,
                finished: false,
            }))
            .unwrap();
    }
    if let Some(status) = request.headers().get("x-fixture-http-status") {
        peer.record(header_string(request.headers(), "authorization"));
        let mut response = HyperResponse::builder()
            .status(http::StatusCode::from_bytes(status.as_bytes()).unwrap())
            .header("content-type", "application/grpc");
        if let Some(code) = request.headers().get("x-fixture-grpc-status") {
            response = response.header("grpc-status", code);
        }
        if let Some(code) = request.headers().get("x-fixture-grpc-trailer") {
            return response
                .body(Body::new(GatedTrailers {
                    release: Box::pin(std::future::ready(())),
                    finished: false,
                    status: code.clone(),
                }))
                .unwrap();
        }
        return response.body(Body::empty()).unwrap();
    }
    if request.uri().path() == "/example.v1.EchoService/Unary" {
        if let Some(timeout) = grpc_timeout(request.headers()) {
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

fn grpc_timeout(headers: &http::HeaderMap) -> Option<Duration> {
    headers
        .get(shared_rules::grpc::TIMEOUT_HEADER)
        .and_then(|value| shared_rules::grpc::parse_timeout(value.as_bytes()))
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
async fn plaintext_cannot_be_bound_to_oauth_credentials() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    let channel = Client::builder(
        &format!("http://{}", resource.address),
        ClientSecurity::Plaintext,
        Duration::from_secs(10),
    )
    .build()
    .unwrap();
    assert_eq!(
        credentials.grpc(channel.clone()).unwrap_err().key,
        "grpc_resource"
    );
    assert_eq!(
        tokens
            .exchange_tokens(&[], None, nonzero(1))
            .grpc(channel)
            .unwrap_err()
            .key,
        "grpc_resource"
    );
    assert_eq!(tokens.token_request_count(), 0);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn bearer_is_withheld_when_the_resource_certificate_is_untrusted() {
    let tokens = Fixture::new().await;
    let mut resource = Resource::new().await;
    let other = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    assert_eq!(
        credentials
            .grpc(other.channel(ClientTimeout::FullRpc(Duration::from_secs(10))))
            .unwrap_err()
            .key,
        "resource_origin"
    );
    assert_eq!(other.calls(), 0);
    assert_eq!(tokens.token_request_count(), 0);
    other.finish().await;
    resource.root_pem = tls_test_support::new_issuer().pem();
    let error = resource
        .client(&credentials)
        .unary(rpc(UnaryRequest::default(), Duration::from_secs(10)))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(tokens.token_request_count(), 1);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn caller_authorization_and_competing_bound_contexts_fail_before_io() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let transport = resource.transport(&resource.credentials(&tokens));
    let mut client = EchoServiceClient::new(transport.clone());
    for (cancelled, expected) in [(false, Code::Internal), (true, Code::Cancelled)] {
        let mut request = rpc(UnaryRequest::default(), Duration::from_secs(10));
        request
            .metadata_mut()
            .insert("authorization", "Bearer caller-token".parse().unwrap());
        if cancelled {
            let parent = OperationContext::unbounded();
            parent.cancel();
            request.extensions_mut().insert(parent);
        }
        assert_eq!(client.unary(request).await.unwrap_err().code(), expected);
    }
    let bound = transport.with_context(&OperationContext::unbounded());
    let mut client = EchoServiceClient::new(bound);
    let mut request = Request::new(UnaryRequest::default());
    request
        .extensions_mut()
        .insert(OperationContext::unbounded());
    let conflict = client.unary(request).await.unwrap_err();
    assert_eq!(conflict.code(), Code::Internal);
    assert_eq!(
        conflict.message(),
        "request operation context conflicts with client context"
    );
    assert_eq!(tokens.token_request_count(), 0);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn oauth_opening_limit_refuses_before_acquiring_credentials_or_dispatching() {
    const WAIT: Duration = Duration::from_secs(5);
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let gate = tokens.block_tokens();
    let channel = resource.channel_with_limit(
        ClientTimeout::FullRpc(Duration::from_secs(20)),
        Some(NonZeroUsize::MIN),
    );
    let mut first =
        EchoServiceClient::new(resource.credentials(&tokens).grpc(channel.clone()).unwrap());
    let mut other = EchoServiceClient::new(resource.credentials(&tokens).grpc(channel).unwrap());
    let mut opening = Box::pin(first.unary(UnaryRequest::default()));
    tokio::time::timeout(WAIT, async {
        tokio::select! {
            () = tokens.token_received() => {}
            result = &mut opening => panic!("the provider must hold the opening: {result:?}"),
        }
    })
    .await
    .unwrap();

    let error = tokio::time::timeout(WAIT, other.unary(UnaryRequest::default()))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
    assert_eq!(tokens.token_request_count(), 1);
    assert_eq!(resource.calls(), 0);

    gate.add_permits(2);
    tokio::time::timeout(WAIT, opening).await.unwrap().unwrap();
    tokio::time::timeout(WAIT, other.unary(UnaryRequest::default()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokens.token_request_count(), 2);
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn token_acquisition_spends_the_bound_deadline_without_dispatch() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let gate = tokens.block_tokens();
    let credentials = resource.credentials(&tokens);
    let parent = OperationContext::with_timeout(Duration::from_secs(1));
    let transport = resource.transport(&credentials).with_context(&parent);
    let mut client = EchoServiceClient::new(transport);
    let mut call = Box::pin(client.unary(UnaryRequest::default()));
    tokio::select! {
        () = tokens.token_received() => {}
        result = &mut call => panic!("call finished before the token arrived: {result:?}"),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    let result = poll_fn(|cx| Poll::Ready(call.as_mut().poll(cx))).await;
    assert_matches!(result, Poll::Ready(Err(error)) if error.code() == Code::DeadlineExceeded);
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
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": "private"}),
            Code::Unavailable,
            AcquisitionError::Unavailable,
        ),
        (
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "invalid_client", "error_description": "private"}),
            Code::Unauthenticated,
            AcquisitionError::Rejected(Rejection::InvalidClient),
        ),
    ] {
        tokens.token_json(status, &body);
        let error = resource
            .client(&resource.credentials(&tokens))
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
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<AcquisitionError>());
        assert_eq!(source, Some(&reason));
    }
    assert_eq!(tokens.token_request_count(), 2);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_cancelled_acquisition_is_cancelled_with_typed_source() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    let gate = tokens.block_tokens();
    let parent = OperationContext::unbounded();
    let transport = resource.transport(&credentials).with_context(&parent);
    let mut client = EchoServiceClient::new(transport);
    let mut call = Box::pin(client.unary(UnaryRequest::default()));
    tokio::select! {
        () = tokens.token_received() => {}
        result = &mut call => panic!("token must be gated: {result:?}"),
    }
    parent.cancel();
    let error = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), Code::Cancelled);
    assert_eq!(error.message(), "request cancelled");
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<AcquisitionError>());
    assert_eq!(source, Some(&AcquisitionError::Cancelled));
    assert_eq!(resource.calls(), 0);
    drop(gate);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn dropping_registration_stops_a_bound_grpc_call_during_token_acquisition() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let registration = prepare_registration(tokens.options());
    let (credentials, _driver) =
        registration.service_tokens(ServiceTokenTarget::default(), resource.origin());
    let transport = resource
        .transport(&credentials)
        .with_context(&OperationContext::unbounded());
    let mut client = EchoServiceClient::new(transport);
    let _gate = tokens.block_tokens();
    let mut call = Box::pin(client.unary(UnaryRequest::default()));
    tokio::select! {
        () = tokens.token_received() => {}
        result = &mut call => panic!("token must be gated: {result:?}"),
    }
    drop(registration);
    let error = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .unwrap()
        .unwrap_err();
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<AcquisitionError>());
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(source, Some(&AcquisitionError::Unavailable));
    assert_eq!(tokens.token_request_count(), 1);
    assert_eq!(resource.calls(), 0);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn provider_capacity_is_unavailable_with_typed_source_before_resource_dispatch() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let registration = prepare_registration(crate::Options {
        provider_concurrency: nonzero(1),
        ..tokens.options()
    });
    let (credentials, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), resource.origin());
    tokens.drive(driver);
    let http = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            tokens.resource_origin(),
            nonzero(1),
        )
        .unwrap()
        .http(tokens.resource_client())
        .unwrap();
    let gate = tokens.block_tokens();
    let active_subject = subject("active");
    let context = OperationContext::with_timeout(Duration::from_secs(10));
    let mut exchange = Box::pin(http.execute(&context, tokens.request(), &active_subject));
    tokio::select! { () = tokens.token_received() => {}, result = &mut exchange => panic!("exchange must be gated: {result:?}"), }
    let error = resource
        .client(&credentials)
        .unary(rpc(
            UnaryRequest {
                message: "capacity".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Unavailable);
    assert_eq!(error.message(), "client credentials unavailable");
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<AcquisitionError>());
    assert_eq!(source, Some(&AcquisitionError::AtCapacity));
    assert_eq!(tokens.token_request_count(), 1);
    assert_eq!(resource.calls(), 0);
    assert_eq!(tokens.resource_request_count(), 0);
    drop(exchange);
    drop(gate);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn one_cached_bearer_is_sent_and_reused() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    let context = OperationContext::with_timeout(Duration::from_secs(10));
    let transport = resource.transport(&credentials).with_context(&context);
    let mut client = EchoServiceClient::new(transport);
    let first = client
        .unary(UnaryRequest {
            message: "one".to_owned(),
        })
        .await
        .unwrap()
        .into_inner();
    tokens.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "other", "token_type": "Bearer", "expires_in": 60}),
    );
    let second = client
        .unary(UnaryRequest {
            message: "two".to_owned(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(first.message, "one");
    assert_eq!(second.message, "two");
    assert_eq!(tokens.token_request_count(), 1);
    assert_eq!(
        resource.authorizations(),
        ["Bearer fixture-token", "Bearer fixture-token"]
    );
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_dropped_refresh_driver_closes_cached_grpc_calls_before_dispatch() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let registration = prepare_registration(tokens.options());
    let (credentials, driver) =
        registration.service_tokens(ServiceTokenTarget::default(), resource.origin());
    let mut client = resource.client(&credentials);
    client
        .unary(rpc(
            UnaryRequest {
                message: "warm".to_owned(),
            },
            Duration::from_secs(10),
        ))
        .await
        .unwrap();
    let token_requests = tokens.token_request_count();
    let resource_calls = resource.calls();
    drop(driver);
    let cancelled = OperationContext::unbounded();
    cancelled.cancel();
    for (after, request_context, expected) in [
        (Duration::from_secs(10), None, Code::Unavailable),
        (Duration::ZERO, None, Code::DeadlineExceeded),
        (Duration::from_secs(10), Some(cancelled), Code::Cancelled),
    ] {
        let mut request = rpc(
            UnaryRequest {
                message: "closed".to_owned(),
            },
            after,
        );
        if let Some(context) = request_context {
            request.extensions_mut().insert(context);
        }
        let error = client.unary(request).await.unwrap_err();
        assert_eq!(error.code(), expected);
    }
    assert_eq!(tokens.token_request_count(), token_requests);
    assert_eq!(resource.calls(), resource_calls);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_token_past_its_reuse_cutoff_is_refreshed_before_dispatch() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "cached", "token_type": "Bearer", "expires_in": 60}),
    );
    let resource = Resource::new().await;
    let mut client = resource.client(&resource.credentials(&tokens));
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
    assert_eq!(tokens.token_request_count(), 1);
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
    assert_eq!(tokens.token_request_count(), 2);
    assert_eq!(resource.calls(), 3);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn token_wait_is_subtracted_from_the_propagated_grpc_timeout() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let gate = tokens.block_tokens();
    let context = OperationContext::with_timeout(Duration::from_secs(10));
    let transport = resource
        .transport(&resource.credentials(&tokens))
        .with_context(&context);
    let mut client = EchoServiceClient::new(transport);
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
    assert!(
        !timeout.is_zero() && *timeout <= Duration::from_millis(400),
        "propagated {timeout:?}"
    );
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_reused_token_preserves_the_budget_remaining_at_transport_handoff() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let mut client = resource.client(&resource.credentials(&tokens));
    let mut elapsed = Vec::new();
    for _ in 0..4 {
        let started = Instant::now();
        client
            .unary(rpc(
                UnaryRequest {
                    message: "budget".to_owned(),
                },
                Duration::from_secs(1),
            ))
            .await
            .unwrap();
        elapsed.push(started.elapsed());
    }
    let timeouts = resource.peer.timeouts.lock().unwrap().clone();
    assert_eq!(timeouts.len(), 4);
    assert_eq!(tokens.token_request_count(), 1);
    for (timeout, elapsed) in timeouts.iter().zip(elapsed).skip(1) {
        let minimum = Duration::from_secs(1).saturating_sub(elapsed + Duration::from_micros(1));
        assert!(
            (minimum..=Duration::from_secs(1)).contains(timeout),
            "propagated {timeout:?}, elapsed {elapsed:?}"
        );
    }
    resource.finish().await;
    tokens.finish().await;
}

async fn warm_and_age(credentials: &ServiceTokens) {
    credentials
        .service_token(acquisition_deadline(&token_context()))
        .await
        .unwrap();
    super::advance(crate::token::EVICTION_MIN_AGE).await;
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
        let credentials = resource.credentials(&tokens);
        let before_tokens = tokens.token_request_count();
        warm_and_age(&credentials).await;
        let before_calls = resource.calls();
        let parent = OperationContext::with_timeout(Duration::from_secs(10));
        let mut client =
            EchoServiceClient::new(resource.transport(&credentials).with_context(&parent));
        let error = client
            .unary(UnaryRequest {
                message: message.to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(tokens.token_request_count(), before_tokens + 1);
        assert_eq!(resource.calls(), before_calls + 1);
        client
            .unary(UnaryRequest {
                message: "again".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(
            tokens.token_request_count(),
            before_tokens + 1 + extra_fetches
        );
        assert_eq!(resource.calls(), before_calls + 2);
    }
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn unauthenticated_in_terminal_trailers_evicts_without_replay() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    warm_and_age(&credentials).await;
    let channel = resource.channel(ClientTimeout::FullRpc(Duration::from_secs(10)));
    let mut client = credentials.grpc(channel).unwrap();
    let mut response = client
        .call(
            http::Request::post("/budget-stream")
                .header("content-type", "application/grpc")
                .header("x-fixture-grpc-status", "16")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!response.headers().contains_key("grpc-status"));
    resource.release();
    let frame = tokio::time::timeout(
        Duration::from_secs(2),
        poll_fn(|cx| hyper::body::Body::poll_frame(Pin::new(response.body_mut()), cx)),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(frame.trailers_ref().unwrap()["grpc-status"], "16");
    assert_eq!(resource.calls(), 1);
    assert_eq!(tokens.token_request_count(), 1);

    tokens.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "replacement", "token_type": "Bearer", "expires_in": 60}),
    );
    resource
        .client(&credentials)
        .unary(rpc(UnaryRequest::default(), Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(resource.calls(), 2);
    assert_eq!(tokens.token_request_count(), 2);
    assert_eq!(
        resource.authorizations().last().unwrap(),
        "Bearer replacement"
    );
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn a_late_rejection_does_not_evict_a_newer_cached_token() {
    let tokens = Fixture::new().await;
    tokens.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "first", "token_type": "Bearer", "expires_in": 60}),
    );
    let resource = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    let mut client = resource.client(&credentials);
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
    credentials.clear_cached();
    tokens.token_json(
        StatusCode::OK,
        &serde_json::json!({"access_token": "second", "token_type": "Bearer", "expires_in": 60}),
    );
    credentials
        .service_token(acquisition_deadline(&token_context()))
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
    assert_eq!(tokens.token_request_count(), 2);
    assert_eq!(resource.authorizations().last().unwrap(), "Bearer second");
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn http_401_without_grpc_status_evicts_and_another_status_does_not() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    for (status_metadata, expected, extra_fetches) in [
        (None, Code::Unauthenticated, 1),
        (
            Some(("x-fixture-grpc-trailer", "")),
            Code::Unauthenticated,
            1,
        ),
        (
            Some(("x-fixture-grpc-status", "7")),
            Code::PermissionDenied,
            0,
        ),
        (
            Some(("x-fixture-grpc-trailer", "7")),
            Code::PermissionDenied,
            0,
        ),
    ] {
        let credentials = resource.credentials(&tokens);
        let before_tokens = tokens.token_request_count();
        let before_calls = resource.calls();
        warm_and_age(&credentials).await;
        let mut request = rpc(UnaryRequest::default(), Duration::from_secs(10));
        request
            .metadata_mut()
            .insert("x-fixture-http-status", "401".parse().unwrap());
        if let Some((header, code)) = status_metadata {
            request.metadata_mut().insert(header, code.parse().unwrap());
        }
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            resource.client(&credentials).unary(request),
        )
        .await
        .expect("http fixture completes")
        .unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(tokens.token_request_count(), before_tokens + 1);
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
            tokens.token_request_count(),
            before_tokens + 1 + extra_fetches
        );
        assert_eq!(resource.calls(), before_calls + 2);
    }
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn exchange_client_sends_exchanged_bearer_and_closes_when_registration_drops() {
    let tokens = Fixture::new().await;
    let resource = Resource::new().await;
    let credentials = resource.credentials(&tokens);
    let message = || UnaryRequest {
        message: "one".to_owned(),
    };
    resource
        .client(&credentials)
        .unary(rpc(message(), Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(tokens.token_request_count(), 1);
    assert_eq!(resource.calls(), 1);

    let channel = resource.channel(ClientTimeout::FullRpc(Duration::from_secs(10)));
    let registration = prepare_registration(tokens.options());
    let exchange = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            resource.origin(),
            nonzero(1),
        )
        .unwrap();
    let exchange_client = exchange.grpc(channel).unwrap();
    let mut client = EchoServiceClient::new(exchange_client.on_behalf_of(subject("subject-token")));

    tokens.token_json(StatusCode::OK, &super::exchange_response("exchanged-token"));
    client
        .unary(rpc(message(), Duration::from_secs(10)))
        .await
        .unwrap();
    assert_eq!(
        resource.authorizations(),
        ["Bearer fixture-token", "Bearer exchanged-token"]
    );
    drop(registration);
    assert_eq!(
        client
            .unary(rpc(message(), Duration::from_secs(10)))
            .await
            .unwrap_err()
            .code(),
        Code::Unavailable
    );
    assert_eq!(tokens.token_request_count(), 2);
    assert_eq!(resource.calls(), 2);
    resource.finish().await;
    tokens.finish().await;
}

#[tokio::test]
async fn credentials_and_stream_spend_the_selected_resource_interval() {
    for cached in [false, true] {
        for (opening_only, caller, expires) in [
            (false, None, true),
            (true, None, false),
            (true, Some(Duration::from_secs(10)), false),
            (true, Some(Duration::from_secs(3)), true),
        ] {
            let tokens = Fixture::new().await;
            let resource = Resource::new().await;
            let credentials = resource.credentials(&tokens);
            if cached {
                credentials
                    .service_token(acquisition_deadline(&token_context()))
                    .await
                    .unwrap();
            }
            let gate = (!cached).then(|| tokens.block_tokens());
            let local = Duration::from_secs(3);
            let policy = if opening_only {
                ClientTimeout::OpeningOnly(local)
            } else {
                ClientTimeout::FullRpc(local)
            };
            let mut client = credentials.grpc(resource.channel(policy)).unwrap();
            let mut request = http::Request::post("/budget-stream")
                .header("content-type", "application/grpc")
                .body(Body::empty())
                .unwrap();
            if let Some(caller) = caller {
                let mut metadata = Request::new(());
                metadata.set_timeout(caller);
                *request.headers_mut() = metadata.into_parts().0.into_headers();
            }
            let parent = OperationContext::unbounded();
            request.extensions_mut().insert(parent.clone());
            let mut call = client.call(request);
            if let Some(gate) = gate {
                tokio::select! {
                    () = tokens.token_received() => {},
                    result = &mut call => panic!("token must be gated: {result:?}"),
                }
                super::advance(Duration::from_secs(2)).await;
                gate.add_permits(1);
            }
            let response = tokio::time::timeout(Duration::from_secs(2), call)
                .await
                .unwrap()
                .unwrap();
            let mut body = response.into_body();
            super::advance(Duration::from_secs(if cached { 4 } else { 2 })).await;
            if expires {
                let frame = poll_fn(|cx| {
                    Poll::Ready(hyper::body::Body::poll_frame(Pin::new(&mut body), cx))
                })
                .await;
                assert_matches!(frame, Poll::Ready(Some(Ok(ref frame)))
                    if frame.trailers_ref().and_then(Status::from_header_map)
                        .is_some_and(|status| status.code() == Code::DeadlineExceeded), "{frame:?}");
            } else {
                let mut frame = Box::pin(poll_fn(|cx| {
                    hyper::body::Body::poll_frame(Pin::new(&mut body), cx)
                }));
                super::poll_pending(frame.as_mut()).await;
                if caller.is_some() {
                    super::advance(Duration::from_secs(7)).await;
                    let result = poll_fn(|cx| Poll::Ready(frame.as_mut().poll(cx))).await;
                    assert_matches!(result, Poll::Ready(Some(Ok(ref frame)))
                        if frame.trailers_ref().and_then(Status::from_header_map)
                            .is_some_and(|status| status.code() == Code::DeadlineExceeded), "{result:?}");
                } else {
                    parent.cancel();
                    let frame = tokio::time::timeout(Duration::from_secs(1), frame)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        Status::from_header_map(frame.trailers_ref().unwrap())
                            .unwrap()
                            .code(),
                        Code::Cancelled
                    );
                }
            }
            assert_eq!(tokens.token_request_count(), 1);
            assert_eq!(resource.calls(), 1);
            resource.finish().await;
            tokens.finish().await;
        }
    }
}

#[tokio::test]
async fn resource_opening_ceiling_starts_before_credentials_without_caller_metadata() {
    for policy in [
        ClientTimeout::FullRpc(Duration::from_secs(1)),
        ClientTimeout::OpeningOnly(Duration::from_secs(1)),
    ] {
        let tokens = Fixture::new().await;
        let resource = Resource::new().await;
        let gate = tokens.block_tokens();
        let mut client = resource
            .credentials(&tokens)
            .grpc(resource.channel(policy))
            .unwrap();
        let mut call = client.call(
            http::Request::post("/budget-stream")
                .body(Body::empty())
                .unwrap(),
        );
        tokio::select! {
            () = tokens.token_received() => {},
            result = &mut call => panic!("token must be gated: {result:?}"),
        }
        super::advance(Duration::from_secs(2)).await;
        let result = poll_fn(|cx| Poll::Ready(Pin::new(&mut call).poll(cx))).await;
        assert_matches!(result, Poll::Ready(Err(ref error))
            if error.code() == Code::DeadlineExceeded, "{result:?}");
        drop(call);
        assert_eq!(resource.calls(), 0);
        gate.add_permits(1);
        resource.finish().await;
        tokens.finish().await;
    }
}
