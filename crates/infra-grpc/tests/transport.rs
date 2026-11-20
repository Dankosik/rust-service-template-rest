#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]

use std::{
    assert_matches,
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

// template:begin oidc-introspection:grpc-transport-test-auth-std-imports
use std::time::{SystemTime, UNIX_EPOCH};
// template:end oidc-introspection:grpc-transport-test-auth-std-imports
// template:begin authn:grpc-transport-test-auth-atomic-types
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
// template:end authn:grpc-transport-test-auth-atomic-types

use futures_util::StreamExt as _;
use grpc_contracts::example::v1::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_client::EchoServiceClient,
    echo_service_server::{EchoService, EchoServiceServer},
};
use health::{Readiness, RefreshPolicy};
// template:begin oidc-introspection:grpc-transport-test-auth-imports
use infra_bearerauthn::{
    EndpointUrl, IntrospectionCacheOptions, IntrospectionOptions, IssuerUrl,
    test_support::{FixtureTransport, prepare_introspection_with_fixture},
};
// template:end oidc-introspection:grpc-transport-test-auth-imports
// template:begin authn:grpc-transport-test-verifier-import
use infra_bearerauthn::Verifier;
use infra_grpc::{DelegationCredential, PolicyKind, Principal};
// template:end authn:grpc-transport-test-verifier-import
use infra_grpc::{
    ERROR_DOMAIN, Error, Failure, Limits, RequestExt as _, ServerOptions, ServerTlsMaterial,
    Services, TlsMaterialError, server_options, server_tls_config,
};
use infra_http_transport::{BoundListener, ConnectionLimits, Drained, Server, TlsOptions};
use infra_outbound_grpc::{
    Client, ClientIdentity, ClientSecurity, ClientTimeout, ClientTlsMaterial,
    transport::PreparedCall,
};
// template:begin authn:grpc-scope-trace-test-imports
use axum::extract::State;
use http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use infra_http::extract::Path;
use infra_http::{HardenOptions, HttpContract, Problem};
use opentelemetry::Value;
use opentelemetry::trace::TracerProvider as _;
use tracing_subscriber::layer::SubscriberExt as _;
// template:end authn:grpc-scope-trace-test-imports
use parking_lot::Mutex;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{PrivateKeyDer, ServerName, pem::PemObject as _},
};
// template:begin oidc-introspection:grpc-transport-test-auth-secret
use secrecy::SecretString;
// template:end oidc-introspection:grpc-transport-test-auth-secret
use service_failure::Code as FailureCode;
use tokio::{
    io::AsyncReadExt as _,
    net::TcpStream,
    sync::{Notify, watch},
    time::timeout,
};
// template:begin oidc-introspection:grpc-transport-test-auth-listener
use tokio::net::TcpListener;
// template:end oidc-introspection:grpc-transport-test-auth-listener
// template:begin oidc-introspection:grpc-transport-test-auth-io
use tokio::io::AsyncWriteExt as _;
// template:end oidc-introspection:grpc-transport-test-auth-io
// template:begin oidc-introspection:grpc-transport-test-auth-tls-acceptor
use tokio_rustls::TlsAcceptor;
// template:end oidc-introspection:grpc-transport-test-auth-tls-acceptor
use tokio_rustls::TlsConnector;
use tonic::{Code, Request, Response, Status};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};
use tonic_reflection::pb::v1::{
    ServerReflectionRequest, server_reflection_client::ServerReflectionClient,
    server_reflection_request::MessageRequest, server_reflection_response::MessageResponse,
};
use tonic_reflection::pb::v1alpha;
use tonic_types::StatusExt as _;
use tower::ServiceExt as _;

// template:begin authn:grpc-transport-test-bearer-module
#[path = "support/bearer.rs"]
mod bearer;
use bearer::{BearerFixture, BearerMode};
// template:end authn:grpc-transport-test-bearer-module

// template:begin oidc-introspection:grpc-transport-test-introspection-tokens
const ACCEPTED: &str = "accepted";
const UNSCOPED: &str = "unscoped";
// template:end oidc-introspection:grpc-transport-test-introspection-tokens
// template:begin authn:grpc-transport-test-unary-path
const ORDERS_READ: &str = "orders.read";
const STREAM_METHODS: [&str; 3] = ["ClientStream", "ServerStream", "BidiStream"];
const SERVER_STREAM_PATH: &str = "/example.v1.EchoService/ServerStream";
// template:end authn:grpc-transport-test-unary-path

const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

const WAIT: Duration = Duration::from_secs(5);
const FILL: Duration = Duration::from_secs(30);
const ECHO_SERVICE: &str = "example.v1.EchoService";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Seen {
    Unary,
    ClientStream,
    ServerStream,
    BidiStream,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SeenCall {
    cardinality: Seen,
    // template:begin authn:grpc-transport-test-seen-identity-field
    had_identity: bool,
    had_checksum: bool,
    // template:end authn:grpc-transport-test-seen-identity-field
    had_authorization: bool,
}

struct Hold {
    entered: watch::Sender<usize>,
    released: watch::Sender<bool>,
}

impl Hold {
    fn new() -> Self {
        Self {
            entered: watch::Sender::new(0),
            released: watch::Sender::new(false),
        }
    }

    async fn enter_and_wait_for_release(&self) {
        self.entered.send_modify(|entered| *entered += 1);
        let _ = self
            .released
            .subscribe()
            .wait_for(|released| *released)
            .await;
    }

    async fn wait_for_entries(&self, expected: usize) {
        timeout(
            FILL,
            self.entered
                .subscribe()
                .wait_for(|entered| *entered >= expected),
        )
        .await
        .expect("handlers enter")
        .expect("the hold outlives its waiters");
    }

    fn release(&self) {
        self.released.send_replace(true);
    }
}

type UploadAborts = Arc<Mutex<Vec<Result<Option<String>, Code>>>>;

// template:begin authn:grpc-transport-test-admitted-identity
type AdmittedIdentity = (Principal, Option<DelegationCredential>);

#[derive(Clone, Default)]
struct Orders(Arc<AtomicUsize>);

impl Orders {
    fn read_order(&self, principal: &Principal, order: &str) -> Option<&'static str> {
        self.0.fetch_add(1, Ordering::Relaxed);
        (principal.subject() == Some("subject") && order == "order-42").then_some("order-42")
    }

    fn reads(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}
// template:end authn:grpc-transport-test-admitted-identity

#[derive(Clone)]
struct Echo {
    calls: Arc<Mutex<Vec<SeenCall>>>,
    hold: Arc<Hold>,
    stream_dropped: Arc<Notify>,
    upload_aborts: UploadAborts,
    upload_abort_recorded: Arc<Notify>,
    // template:begin authn:grpc-transport-test-principals-field
    identities: Arc<Mutex<Vec<AdmittedIdentity>>>,
    orders: Orders,
    // template:end authn:grpc-transport-test-principals-field
}

impl Echo {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            hold: Arc::new(Hold::new()),
            stream_dropped: Arc::new(Notify::new()),
            upload_aborts: Arc::new(Mutex::new(Vec::new())),
            upload_abort_recorded: Arc::new(Notify::new()),
            // template:begin authn:grpc-transport-test-principals-init
            identities: Arc::new(Mutex::new(Vec::new())),
            orders: Orders::default(),
            // template:end authn:grpc-transport-test-principals-init
        }
    }

    fn observe<T>(&self, request: &Request<T>, seen: Seen) {
        let opening = request
            .operation()
            .expect("admitted tonic handlers receive the opening context");
        assert!(opening.deadline().is_some());
        let response = request
            .response_context()
            .expect("response work has its own lifetime carrier");
        assert!(!response.operation().cancellation().is_cancelled());
        // template:begin authn:grpc-transport-test-seen-principal
        let verified = request.principal().ok().cloned();
        assert_eq!(
            request
                .extensions()
                .get::<Principal>()
                .map(Principal::issuer),
            verified.as_ref().map(Principal::issuer),
        );
        let had_identity = verified.is_some();
        if let Some(principal) = verified {
            let delegation = request.delegation_credential().cloned();
            self.identities.lock().push((principal, delegation));
        }
        // template:end authn:grpc-transport-test-seen-principal
        self.calls.lock().push(SeenCall {
            cardinality: seen,
            // template:begin authn:grpc-transport-test-seen-principal-value
            had_identity,
            had_checksum: request
                .metadata()
                .get("x-checksum")
                .is_some_and(|value| value == "123"),
            // template:end authn:grpc-transport-test-seen-principal-value
            had_authorization: request.metadata().get("authorization").is_some(),
        });
    }
}

#[tonic::async_trait]
impl EchoService for Echo {
    type ServerStreamStream = Pin<
        Box<
            dyn tonic::codegen::tokio_stream::Stream<Item = Result<ServerStreamResponse, Status>>
                + Send,
        >,
    >;
    type BidiStreamStream = Pin<
        Box<
            dyn tonic::codegen::tokio_stream::Stream<Item = Result<BidiStreamResponse, Status>>
                + Send,
        >,
    >;

    async fn unary(
        &self,
        request: Request<UnaryRequest>,
    ) -> Result<Response<UnaryResponse>, Status> {
        self.observe(&request, Seen::Unary);
        match request.get_ref().message.as_str() {
            // template:begin authn:grpc-scope-trace-test-handlers
            order if order.starts_with("order-") => {
                let principal = request.principal()?;
                let message = self
                    .orders
                    .read_order(principal, order)
                    .ok_or_else(|| Failure::new(FailureCode::Forbidden))?;
                Ok(Response::new(UnaryResponse {
                    message: message.to_owned(),
                }))
            }
            // template:end authn:grpc-scope-trace-test-handlers
            "panic" => panic!("panic detail: do not leak"),
            "classified" => Err(Failure::new(FailureCode::BadRequest)
                .field_violation("message", "is reserved")
                .into()),
            "hold" => {
                self.hold.enter_and_wait_for_release().await;
                Ok(Response::new(UnaryResponse {
                    message: "released".to_owned(),
                }))
            }
            "deadline" => {
                self.hold.enter_and_wait_for_release().await;
                std::future::pending().await
            }
            message => Ok(Response::new(UnaryResponse {
                message: message.to_owned(),
            })),
        }
    }

    async fn client_stream(
        &self,
        request: Request<tonic::Streaming<ClientStreamRequest>>,
    ) -> Result<Response<ClientStreamResponse>, Status> {
        self.observe(&request, Seen::ClientStream);
        let mut stream = request.into_inner();
        let mut messages = Vec::new();
        while let Some(message) = stream.message().await? {
            messages.push(message.message);
        }
        Ok(Response::new(ClientStreamResponse {
            message: messages.join(","),
        }))
    }

    async fn server_stream(
        &self,
        request: Request<ServerStreamRequest>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        self.observe(&request, Seen::ServerStream);
        let response_context = request.response_context().ok().cloned();
        let request_context = request.operation().ok().cloned();
        let message = request.into_inner().message;
        let first = futures_util::stream::iter([Ok(ServerStreamResponse {
            message: message.clone(),
        })]);
        Ok(Response::new(match message.as_str() {
            "hold-before-headers" => {
                self.hold.enter_and_wait_for_release().await;
                Box::pin(first.chain(futures_util::stream::pending()))
            }
            "hold-after-first" => {
                let hold = Arc::clone(&self.hold);
                let dropped = notify_on_drop(Arc::clone(&self.stream_dropped));
                Box::pin(first.chain(futures_util::stream::once(async move {
                    let _dropped = dropped;
                    hold.enter_and_wait_for_release().await;
                    let request_cancelled = request_context
                        .is_some_and(|context| context.cancellation().is_cancelled());
                    Ok(ServerStreamResponse {
                        message: if request_cancelled {
                            "request context cancelled while the stream was open".to_owned()
                        } else {
                            "released".to_owned()
                        },
                    })
                })))
            }
            "drain-aware" => {
                let dropped = notify_on_drop(Arc::clone(&self.stream_dropped));
                let context = response_context.expect("response context is present");
                Box::pin(
                    first.chain(
                        futures_util::stream::once(async move {
                            let _dropped = dropped;
                            let reason = context.operation().wait_stopped().await;
                            assert_eq!(reason, operation_context::Stopped::Cancelled);
                        })
                        .filter_map(|()| std::future::ready(None)),
                    ),
                )
            }
            "flood" => {
                let dropped = notify_on_drop(Arc::clone(&self.stream_dropped));
                Box::pin(async_stream::stream! {
                    let _dropped = dropped;
                    loop {
                        yield Ok(ServerStreamResponse {
                            message: "x".repeat(256 * 1024),
                        });
                    }
                })
            }
            "fail-after-first" => Box::pin(first.chain(futures_util::stream::iter([Err(
                Failure::new(FailureCode::Conflict).into(),
            )]))),
            "panic-after-first" => Box::pin(first.chain(futures_util::stream::poll_fn(
                |_| -> std::task::Poll<Option<Result<ServerStreamResponse, Status>>> {
                    panic!("stream panic detail: do not leak")
                },
            ))),
            _ => Box::pin(first),
        }))
    }

    async fn bidi_stream(
        &self,
        request: Request<tonic::Streaming<BidiStreamRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        self.observe(&request, Seen::BidiStream);
        let observe_abort = request.metadata().contains_key("x-observe-upload-abort");
        let input = request.into_inner();
        let dropped = notify_on_drop(Arc::clone(&self.stream_dropped));
        if observe_abort {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            let aborts = Arc::clone(&self.upload_aborts);
            let recorded = Arc::clone(&self.upload_abort_recorded);
            tokio::spawn(async move {
                let _dropped = dropped;
                let mut input = input;
                if let Ok(Some(first)) = input.message().await {
                    let _ = tx
                        .send(Ok(BidiStreamResponse {
                            message: first.message,
                        }))
                        .await;
                    let next = input
                        .message()
                        .await
                        .map(|opt| opt.map(|msg| msg.message))
                        .map_err(|status| status.code());
                    aborts.lock().push(next);
                    recorded.notify_one();
                }
            });
            let stream = futures_util::stream::unfold(rx, |mut rx| async {
                rx.recv().await.map(|v| (v, rx))
            });
            return Ok(Response::new(Box::pin(stream)));
        }
        Ok(Response::new(Box::pin(async_stream::stream! {
            let _dropped = dropped;
            let mut input = input;
            loop {
                match input.message().await {
                    Ok(Some(item)) => yield Ok(BidiStreamResponse {
                        message: item.message,
                    }),
                    Ok(None) => break,
                    Err(status) => yield Err(status),
                }
            }
        })))
    }
}

fn notify_on_drop(notify: Arc<Notify>) -> impl Drop {
    std::mem::DropGuard::new(notify, |notify| notify.notify_one())
}

struct Fixture {
    server: Option<Server>,
    readiness: health::ReadinessControl,
    pending_activation: Option<health::PreparedReadiness>,
    echo: Echo,
    address: SocketAddr,
    // template:begin authn:grpc-transport-test-auth-provider-field
    bearer: BearerFixture,
    verifier: Verifier,
    // template:end authn:grpc-transport-test-auth-provider-field
}

impl Fixture {
    async fn plaintext() -> Self {
        Self::open(true, None).await
    }

    async fn unseeded() -> Self {
        Self::open(false, None).await
    }

    async fn with_descriptor_set(
        contract: &prost_types::FileDescriptorSet,
        omitted_method: Option<&str>,
    ) -> Self {
        use prost::Message as _;

        Self::open_with_at(
            true,
            None,
            limits(),
            connection_limits(),
            loopback(),
            Box::leak(contract.encode_to_vec().into_boxed_slice()),
            omitted_method,
        )
        .await
    }

    async fn tls(pki: &Pki, mtls: bool) -> Self {
        Self::open(true, Some((pki, mtls))).await
    }

    async fn tls_at(pki: &Pki, address: SocketAddr) -> Self {
        Self::open_with_at(
            true,
            Some((pki, false)),
            limits(),
            connection_limits(),
            address,
            grpc_contracts::FILE_DESCRIPTOR_SET,
            None,
        )
        .await
    }

    async fn limited(max_in_flight: u32) -> Self {
        let limits = Limits {
            max_in_flight: NonZeroU32::new(max_in_flight),
            ..limits()
        };
        Self::open_with(true, None, limits).await
    }

    async fn open(seed: bool, tls: Option<(&Pki, bool)>) -> Self {
        Self::open_with(seed, tls, limits()).await
    }

    async fn open_with(seed: bool, tls: Option<(&Pki, bool)>, limits: Limits) -> Self {
        Self::open_with_at(
            seed,
            tls,
            limits,
            connection_limits(),
            loopback(),
            grpc_contracts::FILE_DESCRIPTOR_SET,
            None,
        )
        .await
    }

    async fn open_with_at(
        seed: bool,
        tls: Option<(&Pki, bool)>,
        limits: Limits,
        connections: ConnectionLimits,
        address: SocketAddr,
        descriptor_set: &'static [u8],
        #[allow(
            unused_variables,
            reason = "the bearer fixture registers the described methods"
        )]
        omitted_method: Option<&str>,
    ) -> Self {
        #[allow(unused_variables, reason = "the bearer profile replaces the test mode")]
        let authentication = infra_grpc::Authentication::None;
        // template:begin authn:grpc-transport-test-auth-fixture
        let (verifier, bearer) = BearerFixture::start().await;
        let authentication = infra_grpc::Authentication::Bearer(verifier.clone());
        // template:end authn:grpc-transport-test-auth-fixture
        let readiness = readiness();
        let control = readiness.control();
        let prepared = readiness.prepare().await.unwrap();
        let pending_activation = if seed {
            prepared.activate().unwrap();
            None
        } else {
            Some(prepared)
        };
        let echo = Echo::new();
        let mut services = Services::new();
        services
            .add_reflection()
            .expect("reflection registers once");
        services
            .describe(descriptor_set)
            .expect("the fixture contract decodes");
        #[allow(
            unused_mut,
            unused_variables,
            reason = "the bearer profile registers scope requirements on the handle"
        )]
        let mut echo_registration = services
            .add(EchoServiceServer::new(echo.clone()))
            .expect("echo registers once");
        // template:begin authn:grpc-transport-test-scope-requirement
        echo_registration
            .require_scopes_for_delegation("Unary", &[ORDERS_READ])
            .expect("unary scopes register once");
        for method in STREAM_METHODS {
            if Some(method) != omitted_method {
                echo_registration.require_scopes(method, &[]).unwrap();
            }
        }
        // template:end authn:grpc-transport-test-scope-requirement
        let app = services
            .build(
                control.reader(),
                authentication,
                ServerOptions {
                    limits,
                    trust_trace_context: false,
                },
            )
            .expect("router builds");
        let tls = tls.map(|(pki, mtls)| {
            let certificate = pem(&pki.server_certificate);
            let key = secrecy::SecretString::from(pem(&pki.server_key));
            let ca = pem(&pki.ca_certificate);
            let config = server_tls_config(ServerTlsMaterial {
                certificate_pem: &certificate,
                private_key_pem: &key,
                client_ca_pem: mtls.then_some(ca.as_str()),
            })
            .expect("server TLS config");
            TlsOptions {
                config,
                handshake_timeout: Duration::from_secs(5),
            }
        });
        let listener = BoundListener::bind(address).expect("listener binds");
        let bound = app
            .serve(listener, server_options(connections), tls)
            .unwrap();
        let address = bound.local_addr();
        Self {
            server: Some(bound),
            readiness: control,
            pending_activation,
            echo,
            address,
            // template:begin authn:grpc-transport-test-auth-provider-init
            bearer,
            verifier,
            // template:end authn:grpc-transport-test-auth-provider-init
        }
    }

    fn echo_client(&self) -> EchoServiceClient<Client> {
        EchoServiceClient::new(plaintext_client(self.address))
    }

    fn health(&self) -> HealthClient<Client> {
        HealthClient::new(plaintext_client(self.address))
    }

    fn take_server(&mut self) -> Server {
        self.server.take().expect("listener still owned")
    }

    async fn stop(mut self) {
        self.echo.hold.release();
        if let Some(server) = self.server.take() {
            let drained = timeout(WAIT, server.drain(Duration::from_secs(3)))
                .await
                .expect("listener drain finishes")
                .expect("listener drain succeeds");
            assert_eq!(drained, Drained::Complete);
        }
        // template:begin authn:grpc-transport-test-auth-provider-stop
        self.bearer.stop().await;
        // template:end authn:grpc-transport-test-auth-provider-stop
    }
}

fn limits() -> Limits {
    Limits {
        request_timeout: Duration::from_secs(8),
        max_in_flight: NonZeroU32::new(256),
        max_admission_wait: Duration::from_secs(8),
    }
}

fn connection_limits() -> ConnectionLimits {
    ConnectionLimits {
        max_connections: NonZeroU32::new(4096),
        max_connection_age: None,
    }
}

fn described() -> Services {
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .expect("the committed contract decodes");
    services
}

fn readiness() -> Readiness {
    Readiness::new(
        Vec::new(),
        RefreshPolicy::try_new(
            Duration::from_mins(1),
            Duration::from_secs(1),
            NonZeroU32::MIN,
        )
        .unwrap(),
    )
}

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn plaintext_client(address: SocketAddr) -> Client {
    Client::builder(
        &format!("http://{address}"),
        ClientSecurity::Plaintext,
        CLIENT_TIMEOUT,
    )
    .build()
    .expect("plaintext client")
}

fn unary(message: &str) -> UnaryRequest {
    UnaryRequest {
        message: message.to_owned(),
    }
}

// template:begin authn:grpc-transport-test-status-helper
fn grpc_status<B>(response: &http::Response<B>) -> Option<Status> {
    Status::from_header_map(response.headers())
}
// template:end authn:grpc-transport-test-status-helper

fn request<T>(message: T) -> Request<T> {
    let metadata = tonic::metadata::MetadataMap::new();
    // template:begin authn:grpc-transport-test-bearer
    let mut metadata = metadata;
    metadata.insert(
        "authorization",
        format!("Bearer {}", BearerMode::SELECTED.token(true))
            .parse()
            .unwrap(),
    );
    // template:end authn:grpc-transport-test-bearer
    Request::from_parts(metadata, http::Extensions::new(), message)
}

#[tokio::test(start_paused = true)]
async fn prepared_call_preserves_the_parent_budget_before_dispatch() {
    let client = Client::builder(
        "http://127.0.0.1:1",
        ClientSecurity::Plaintext,
        Duration::from_secs(2),
    )
    .opening_limit(NonZeroUsize::MIN)
    .build()
    .expect("client config is valid");
    let parent = operation_context::OperationContext::with_timeout(Duration::from_secs(1));
    let mut request = http::Request::new(tonic::body::Body::empty());
    request.extensions_mut().insert(parent.clone());
    let prepared = PreparedCall::prepare(&client, request).unwrap();
    assert_eq!(
        prepared.opening_context().remaining(),
        Some(Duration::from_secs(1))
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let mut expired = http::Request::new(tonic::body::Body::empty());
    expired.extensions_mut().insert(parent.clone());
    assert_eq!(
        PreparedCall::prepare(&client, expired).unwrap_err().code(),
        Code::DeadlineExceeded
    );
    assert_eq!(
        prepared.send().await.unwrap_err().code(),
        Code::DeadlineExceeded
    );
    assert!(
        !parent.cancellation().is_cancelled(),
        "the prepared call owns a child cancellation scope"
    );
    assert!(PreparedCall::prepare(&client, http::Request::new(tonic::body::Body::empty())).is_ok());
}

fn watch_request(service: &str) -> Request<HealthCheckRequest> {
    request(HealthCheckRequest {
        service: service.to_owned(),
    })
}

fn assert_seen(echo: &Echo, expected: &[Seen]) {
    let calls = echo.calls.lock();
    assert_eq!(calls.len(), expected.len());
    for (call, cardinality) in calls.iter().zip(expected) {
        assert_eq!(call.cardinality, *cardinality);
        // template:begin authn:grpc-transport-test-seen-identity-assert
        assert!(call.had_identity);
        // template:end authn:grpc-transport-test-seen-identity-assert
        assert!(!call.had_authorization);
    }
}

#[tokio::test]
async fn all_cardinalities_round_trip() {
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.echo_client();

    let unary = timeout(WAIT, client.unary(request(unary("one"))))
        .await
        .expect("unary")
        .unwrap();
    assert_eq!(unary.into_inner().message, "one");

    let client_stream = timeout(
        WAIT,
        client.client_stream(request(tokio_stream_once(ClientStreamRequest {
            message: "two".to_owned(),
        }))),
    )
    .await
    .expect("client stream")
    .unwrap();
    assert_eq!(client_stream.into_inner().message, "two");

    let mut server = timeout(
        WAIT,
        client.server_stream(request(ServerStreamRequest {
            message: "three".to_owned(),
        })),
    )
    .await
    .expect("server stream")
    .unwrap()
    .into_inner();
    assert_eq!(
        timeout(WAIT, server.message())
            .await
            .expect("server item")
            .unwrap()
            .unwrap()
            .message,
        "three"
    );

    let mut bidi = timeout(
        WAIT,
        client.bidi_stream(request(tokio_stream_once(BidiStreamRequest {
            message: "four".to_owned(),
        }))),
    )
    .await
    .expect("bidi")
    .unwrap()
    .into_inner();
    assert_eq!(
        timeout(WAIT, bidi.message())
            .await
            .expect("bidi item")
            .unwrap()
            .unwrap()
            .message,
        "four"
    );

    assert_seen(
        &fixture.echo,
        &[
            Seen::Unary,
            Seen::ClientStream,
            Seen::ServerStream,
            Seen::BidiStream,
        ],
    );
    drop(client);
    fixture.stop().await;
}

fn tokio_stream_once<T>(item: T) -> impl tonic::codegen::tokio_stream::Stream<Item = T> {
    tonic::codegen::tokio_stream::iter([item])
}

#[tokio::test]
async fn client_observation_fallback_preserves_raw_request_routing() {
    use http_body_util::BodyExt as _;
    let fixture = Fixture::plaintext().await;
    for mismatched_extension in [false, true] {
        let builder = http::Request::builder()
            .method("POST")
            .uri(UNARY_PATH)
            .header("content-type", "application/grpc")
            .header("te", "trailers");
        let authentication = request(()).into_parts().0.into_headers();
        let mut request = builder
            .body(tonic::body::Body::new(http_body_util::Full::new(
                grpc_frame("raw route"),
            )))
            .unwrap();
        request.headers_mut().extend(authentication);
        if mismatched_extension {
            request
                .extensions_mut()
                .insert(tonic::GrpcMethod::new("unrelated.Service", "Wrong"));
        }
        let response = timeout(WAIT, plaintext_client(fixture.address).oneshot(request))
            .await
            .unwrap()
            .unwrap();
        let body = timeout(WAIT, response.into_body().collect())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(body.trailers().unwrap().get("grpc-status").unwrap(), "0");
        assert_eq!(body.to_bytes(), grpc_frame("raw route"));
    }
    assert_seen(&fixture.echo, &[Seen::Unary, Seen::Unary]);
    fixture.stop().await;
}

fn reason(status: &Status) -> String {
    let info = status.get_error_details().error_info().cloned();
    let info = info.expect("error info");
    assert_eq!(info.domain, ERROR_DOMAIN);
    info.reason
}

fn retry_delay(status: &Status) -> Option<Duration> {
    status
        .get_error_details()
        .retry_info()
        .and_then(|info| info.retry_delay)
}

fn assert_at_capacity(status: &Status) {
    assert_eq!(status.code(), Code::ResourceExhausted);
    assert_eq!(status.message(), service_failure::AT_CAPACITY_DETAIL);
    assert_eq!(reason(status), "SERVICE_UNAVAILABLE");
    assert_eq!(retry_delay(status), Some(Duration::from_secs(1)));
}

const IMPATIENT: Duration = Duration::from_millis(100);

fn impatient<T>(mut call: Request<T>) -> Request<T> {
    call.set_timeout(IMPATIENT);
    call
}

// template:begin authn:grpc-transport-test-unauthenticated
#[tokio::test]
async fn missing_and_malformed_bearers_are_unauthenticated_and_health_is_public() {
    let fixture = Fixture::plaintext().await;
    let mut echo = fixture.echo_client();
    let missing = timeout(WAIT, echo.unary(Request::new(unary("missing"))))
        .await
        .expect("missing bearer")
        .unwrap_err();
    assert_eq!(missing.code(), Code::Unauthenticated);
    assert_eq!(missing.message(), "authentication failed");
    assert_eq!(reason(&missing), "AUTHENTICATION_REQUIRED");

    let mut malformed = Request::new(unary("malformed"));
    malformed
        .metadata_mut()
        .insert("authorization", "Bearer bad token".parse().unwrap());
    let malformed = timeout(WAIT, echo.unary(malformed))
        .await
        .expect("malformed bearer")
        .unwrap_err();
    assert_eq!(malformed.code(), Code::Unauthenticated);
    assert_eq!(malformed.message(), "authentication failed");
    assert_eq!(reason(&malformed), "AUTHENTICATION_MALFORMED");
    assert!(fixture.echo.calls.lock().is_empty());

    let mut reflection = ServerReflectionClient::new(plaintext_client(fixture.address));
    let reflection_status = timeout(
        WAIT,
        reflection.server_reflection_info(tonic::codegen::tokio_stream::empty()),
    )
    .await
    .expect("reflection rejects requests without a bearer")
    .unwrap_err();
    assert_eq!(reflection_status.code(), Code::Unauthenticated);
    drop(reflection);

    let mut health = fixture.health();
    let check = timeout(
        WAIT,
        health.check(Request::new(HealthCheckRequest {
            service: String::new(),
        })),
    )
    .await
    .expect("public health check")
    .unwrap();
    assert_eq!(check.into_inner().status, ServingStatus::Serving as i32);

    let watch = timeout(
        WAIT,
        health.watch(Request::new(HealthCheckRequest {
            service: String::new(),
        })),
    )
    .await
    .expect("public health watch")
    .unwrap()
    .into_inner()
    .message()
    .await
    .expect("watch message")
    .expect("first watch status");
    assert_eq!(watch.status, ServingStatus::Serving as i32);
    drop(echo);
    drop(health);
    fixture.stop().await;
}
// template:end authn:grpc-transport-test-unauthenticated

// template:begin authn:grpc-transport-test-insufficient-scope
fn unscoped<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", BearerMode::SELECTED.token(false))
            .parse()
            .unwrap(),
    );
    request
}

async fn verify_grpc_order_reads(fixture: &Fixture) {
    let mut echo = fixture.echo_client();

    let denied = timeout(WAIT, echo.unary(unscoped(unary("order-42"))))
        .await
        .expect("unscoped unary")
        .unwrap_err();
    assert_eq!(denied.code(), Code::PermissionDenied);
    assert_eq!(
        denied.message(),
        "the verified principal lacks the required scope"
    );
    assert_eq!(reason(&denied), "FORBIDDEN");
    assert!(fixture.echo.calls.lock().is_empty());
    assert_eq!(fixture.echo.orders.reads(), 0);

    let missing_bearer = timeout(
        WAIT,
        echo.server_stream(Request::new(ServerStreamRequest {
            message: "denied".to_owned(),
        })),
    )
    .await
    .expect("server stream without a bearer")
    .unwrap_err();
    assert_eq!(missing_bearer.code(), Code::Unauthenticated);
    assert!(fixture.echo.calls.lock().is_empty());

    let delegated = timeout(WAIT, echo.unary(request(unary("order-42"))))
        .await
        .expect("scoped unary")
        .unwrap()
        .into_inner();
    assert_eq!(delegated.message, "order-42");
    let (_, credential) = fixture.echo.identities.lock().pop().unwrap();
    let credential = credential.expect("the opted-in RPC receives the presented token");
    assert_eq!(
        secrecy::ExposeSecret::expose_secret(credential.access_token()),
        BearerMode::SELECTED.token(true)
    );
    assert!(!fixture.echo.calls.lock()[0].had_authorization);

    let mut admitted = timeout(
        WAIT,
        echo.server_stream(unscoped(ServerStreamRequest {
            message: "admitted".to_owned(),
        })),
    )
    .await
    .expect("unscoped server stream")
    .unwrap()
    .into_inner();
    assert_eq!(
        timeout(WAIT, admitted.message())
            .await
            .expect("server item")
            .unwrap()
            .unwrap()
            .message,
        "admitted"
    );
    assert!(fixture.echo.identities.lock().pop().unwrap().1.is_none());

    let denied = timeout(WAIT, echo.unary(request(unary("order-73"))))
        .await
        .expect("handler refusal")
        .unwrap_err();
    assert_eq!(denied.code(), Code::PermissionDenied);
    assert_eq!(reason(&denied), "FORBIDDEN");
    assert_seen(
        &fixture.echo,
        &[Seen::Unary, Seen::ServerStream, Seen::Unary],
    );
    assert_eq!(fixture.echo.orders.reads(), 2);
}

#[utoipa::path(
    get,
    path = "/orders/{id}",
    security(("bearerAuth" = [ORDERS_READ])),
    params(("id" = String, Path)),
    responses(
        (status = 200, description = "order", body = String),
        infra_http::authn::ProtectedOperationProblemResponses
    )
)]
async fn read_http_order(
    State(orders): State<Orders>,
    Path(id): Path<String>,
    infra_http::VerifiedPrincipal(principal): infra_http::VerifiedPrincipal,
    headers: http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;

    assert!(!headers.contains_key(AUTHORIZATION));
    match orders.read_order(&principal, &id) {
        Some(order) => order.into_response(),
        None => Problem::new(FailureCode::Forbidden).into_response(),
    }
}

async fn verify_http_order_reads(fixture: &Fixture) {
    use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
    use utoipa::openapi::{Components, OpenApiBuilder};

    let mut components = Components::new();
    components.add_security_scheme(
        "bearerAuth",
        SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
    );
    let document = OpenApiBuilder::new().components(Some(components)).build();
    let app = HttpContract::application(document)
        .unwrap()
        .merge(infra_http::http_route!(read_http_order).unwrap())
        .unwrap()
        .build(
            fixture.echo.orders.clone(),
            infra_http::Authentication::Bearer(fixture.verifier.clone()),
            &HardenOptions {
                max_body_bytes: 1024,
                request_timeout: WAIT,
                max_in_flight: None,
                admission_wait: Duration::ZERO,
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
            },
        )
        .unwrap();
    let read = |id, scoped| {
        app.clone().oneshot(
            http::Request::get(format!("/orders/{id}"))
                .header(
                    AUTHORIZATION,
                    format!("Bearer {}", BearerMode::SELECTED.token(scoped)),
                )
                .body(axum::body::Body::empty())
                .unwrap(),
        )
    };
    for (id, scoped, expected_reads, challenge) in [
        (
            "order-42",
            false,
            2,
            Some("Bearer error=\"insufficient_scope\""),
        ),
        ("order-73", true, 3, None),
    ] {
        let denied = read(id, scoped).await.unwrap();
        assert_eq!(denied.status(), http::StatusCode::FORBIDDEN);
        assert_eq!(
            denied.extensions().get::<Problem>().unwrap().code(),
            FailureCode::Forbidden
        );
        assert_eq!(
            denied
                .headers()
                .get(WWW_AUTHENTICATE)
                .map(|value| value.to_str().unwrap()),
            challenge
        );
        assert_eq!(fixture.echo.orders.reads(), expected_reads);
    }
    let admitted = read("order-42", true).await.unwrap();
    assert_eq!(admitted.status(), http::StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(admitted.into_body(), 1024)
            .await
            .unwrap(),
        "order-42"
    );
    assert_eq!(fixture.echo.orders.reads(), 4);
}

#[tokio::test]
async fn order_reads_distinguish_scope_refusals_from_handler_refusals_in_both_transports() {
    let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _registration_anchor = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(subscriber);

    let fixture = Fixture::plaintext().await;
    verify_grpc_order_reads(&fixture).await;
    verify_http_order_reads(&fixture).await;
    fixture.stop().await;

    let spans = exporter.get_finished_spans().unwrap();
    for (method, status_key, forbidden) in [
        (
            "example.v1.EchoService/Unary",
            "rpc.status_code",
            Value::from("PERMISSION_DENIED"),
        ),
        (
            "GET /orders/{id}",
            "http.response.status_code",
            Value::I64(403),
        ),
    ] {
        let denials: Vec<_> = spans
            .iter()
            .filter(|span| {
                span.name == method
                    && span.span_kind == opentelemetry::trace::SpanKind::Server
                    && span.attributes.iter().any(|attribute| {
                        attribute.key.as_str() == status_key && attribute.value == forbidden
                    })
            })
            .map(|span| {
                span.attributes
                    .iter()
                    .find(|attribute| attribute.key.as_str() == "authz.reason")
                    .map(|attribute| attribute.value.as_str())
            })
            .collect();
        assert_eq!(
            denials,
            [Some(std::borrow::Cow::Borrowed("insufficient_scope")), None],
            "{method}"
        );
    }
}

#[test]
fn a_handler_requiring_a_principal_reports_a_missing_identity_as_internal() {
    let status = Request::new(()).principal().unwrap_err();
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(reason(&status), "INTERNAL_ERROR");
}

#[tokio::test]
async fn a_bearer_router_rejects_a_registered_method_without_an_explicit_scope_rule() {
    let (verifier, fixture) = BearerFixture::start().await;
    let mut services = described();
    let mut registration = services.add(EchoServiceServer::new(Echo::new())).unwrap();
    for method in std::iter::once("Unary").chain(STREAM_METHODS) {
        if method != "ServerStream" {
            registration.require_scopes(method, &[]).unwrap();
        }
    }

    assert_matches!(
        services
            .build(
                readiness().control().reader(),
                infra_grpc::Authentication::Bearer(verifier),
                ServerOptions {
                    limits: limits(),
                    trust_trace_context: false,
                },
            )
            .unwrap_err(),
        Error::MissingPolicy {
            policy: PolicyKind::ScopeRequirement,
            path,
        } if path == SERVER_STREAM_PATH
    );
    fixture.stop().await;
}

#[test]
fn a_scope_requirement_needs_a_described_method_of_a_registered_service_and_is_declared_once() {
    let mut services = described();
    let mut registration = services
        .add(EchoServiceServer::new(Echo::new()))
        .expect("registration");
    for method in [
        "",
        "Unary/extra",
        "unary",
        "Unry",
        "/example.v1.EchoService/Unary",
    ] {
        let expected_path = format!("/{ECHO_SERVICE}/{method}");
        assert_matches!(
            registration.require_scopes(method, &["echo.read"]),
            Err(Error::UnregisteredMethod {
                policy: PolicyKind::ScopeRequirement,
                path,
            }) if path == expected_path,
            "{method}"
        );
    }
    registration
        .require_scopes("Unary", &["echo.read"])
        .expect("first requirement");
    assert_matches!(
        registration.require_scopes("Unary", &["echo.write"]),
        Err(Error::DuplicatePolicy {
            policy: PolicyKind::ScopeRequirement,
            path,
        }) if path == UNARY_PATH
    );
}

#[test]
fn malformed_scope_tokens_are_rejected_without_registering_a_requirement() {
    let mut services = described();
    let mut registration = services.add(EchoServiceServer::new(Echo::new())).unwrap();

    assert_matches!(
        registration.require_scopes("Unary", &["echo.read", "echo.read echo.write"]),
        Err(Error::InvalidScopeToken { path, index: 1 }) if path == UNARY_PATH
    );
    assert_matches!(
        registration.require_scopes_for_delegation("Unary", &[""]),
        Err(Error::InvalidScopeToken { path, index: 0 }) if path == UNARY_PATH
    );
    registration
        .require_scopes_for_delegation("Unary", &["echo.read", "echo.write"])
        .unwrap();
}
// template:end authn:grpc-transport-test-insufficient-scope

// template:begin authn:grpc-transport-test-rejection-flood
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_calls_do_not_close_the_shared_connection() {
    let fixture = Fixture::plaintext().await;
    let echo = fixture.echo_client();
    let mut callers = Vec::new();
    for _ in 0..16 {
        let mut echo = echo.clone();
        callers.push(tokio::spawn(async move {
            let mut codes = Vec::new();
            for _ in 0..1000 {
                let rejected = echo
                    .unary(Request::new(unary("missing")))
                    .await
                    .unwrap_err();
                codes.push(rejected.code());
            }
            codes
        }));
    }
    timeout(FILL, async {
        for caller in callers {
            let codes = caller.await.expect("caller joins");
            assert!(
                codes.iter().all(|code| *code == Code::Unauthenticated),
                "{codes:?}"
            );
        }
    })
    .await
    .expect("rejected calls finish");
    drop(echo);
    fixture.stop().await;
}
// template:end authn:grpc-transport-test-rejection-flood

// template:begin oidc-introspection:grpc-transport-test-provider-deadline
fn impatient_raw(mut call: http::Request<axum::body::Body>) -> http::Request<axum::body::Body> {
    call.headers_mut()
        .insert("grpc-timeout", "100m".parse().unwrap());
    call
}

#[tokio::test]
async fn the_deadline_bounds_authentication_too() {
    let fixture = Fixture::plaintext().await;
    fixture
        .bearer
        .provider
        .as_ref()
        .expect("the introspection profile selected its provider")
        .silent
        .store(true, Ordering::Release);
    let status = timeout(WAIT, raw_call(fixture.address, UNARY_PATH))
        .await
        .expect("the deadline answers while the provider is silent");
    assert_eq!(status, "4");
    assert!(fixture.echo.calls.lock().is_empty());
    fixture.stop().await;
}

// template:end oidc-introspection:grpc-transport-test-provider-deadline

// template:begin authn:grpc-transport-test-refused-upload
#[tokio::test]
async fn a_refused_upload_drains_within_the_call_budget() {
    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 4);
    let call = pending_upload(
        opening_request("/example.v1.EchoService/BidiStream", None, "refused"),
        "50m",
    );
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let response = timeout(WAIT, app.oneshot(call)).await.unwrap().unwrap();
    let waited = started.elapsed();
    tokio::time::resume();
    assert!(waited < Duration::from_millis(50), "waited {waited:?}");
    let status = grpc_status(&response).unwrap();
    assert_eq!(status.code(), Code::Unauthenticated, "{status:?}");
    assert!(echo.calls.lock().is_empty());
    bearer.stop().await;
}

fn opening_app(verifier: Verifier, echo: &Echo, capacity: u32) -> infra_grpc::Application {
    opening_app_waiting(verifier, echo, capacity, limits().max_admission_wait)
}

fn opening_app_waiting(
    verifier: Verifier,
    echo: &Echo,
    capacity: u32,
    max_admission_wait: Duration,
) -> infra_grpc::Application {
    let mut services = described();
    let mut registration = services.add(EchoServiceServer::new(echo.clone())).unwrap();
    for method in std::iter::once("Unary").chain(STREAM_METHODS) {
        registration.require_scopes(method, &[]).unwrap();
    }
    services
        .build(
            readiness().control().reader(),
            infra_grpc::Authentication::Bearer(verifier),
            ServerOptions {
                limits: Limits {
                    max_in_flight: NonZeroU32::new(capacity),
                    max_admission_wait,
                    ..limits()
                },
                trust_trace_context: false,
            },
        )
        .unwrap()
}

async fn hold_capacity(app: &infra_grpc::Application) -> axum::response::Response {
    app.clone()
        .oneshot(opening_request(
            "/example.v1.EchoService/ServerStream",
            Some(BearerMode::SELECTED.token(true)),
            "hold-after-first",
        ))
        .await
        .unwrap()
}

fn opening_request(
    path: &str,
    bearer: Option<&str>,
    message: &str,
) -> http::Request<axum::body::Body> {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc");
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    request
        .body(axum::body::Body::from(grpc_frame(message)))
        .unwrap()
}

fn pending_upload(
    mut request: http::Request<axum::body::Body>,
    grpc_timeout: &'static str,
) -> http::Request<axum::body::Body> {
    request
        .headers_mut()
        .insert("grpc-timeout", http::HeaderValue::from_static(grpc_timeout));
    *request.body_mut() = axum::body::Body::from_stream(futures_util::stream::pending::<
        Result<bytes::Bytes, std::convert::Infallible>,
    >());
    request
}

// template:end authn:grpc-transport-test-refused-upload

// template:begin oidc-introspection:grpc-transport-test-provider-admission-followers
#[tokio::test]
async fn opening_admission_counts_auth_followers_and_recovers_after_cancellation() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (verifier, provider) = verifier_fixture().await;
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 2);
    let gate = Arc::new(Hold::new());
    *provider.gate.lock() = Some(Arc::clone(&gate));
    let mut leader = Box::pin(app.clone().oneshot(opening_request(
        UNARY_PATH,
        Some(ACCEPTED),
        "first",
    )));
    timeout(WAIT, async {
        tokio::select! {
            () = gate.wait_for_entries(1) => {},
            result = &mut leader => panic!("authentication must wait: {result:?}"),
        }
    })
    .await
    .unwrap();
    let mut follower = Box::pin(app.clone().oneshot(opening_request(
        UNARY_PATH,
        Some(ACCEPTED),
        "follower",
    )));
    assert!(futures_util::poll!(follower.as_mut()).is_pending());

    for bearer in [None, Some("malformed token"), Some("distinct")] {
        let response = timeout(
            WAIT,
            app.clone().oneshot(impatient_raw(opening_request(
                UNARY_PATH, bearer, "overflow",
            ))),
        )
        .await
        .unwrap()
        .unwrap();
        assert_at_capacity(&grpc_status(&response).unwrap());
    }
    assert_eq!(provider.calls.load(Ordering::Acquire), 1);
    assert!(echo.calls.lock().is_empty());
    assert!(
        metrics
            .render()
            .contains("grpc_server_shed_requests_total 3")
    );

    for method in ["Check", "Watch"] {
        let response = timeout(
            WAIT,
            app.clone().oneshot(opening_request(
                &format!("/grpc.health.v1.Health/{method}"),
                None,
                "",
            )),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(grpc_status(&response).is_none());
    }
    tokio::time::pause();
    let response = app
        .clone()
        .oneshot(pending_upload(
            opening_request(UNARY_PATH, None, "draining"),
            "50m",
        ))
        .await
        .unwrap();
    tokio::time::resume();
    assert_at_capacity(&grpc_status(&response).unwrap());
    assert!(
        metrics
            .render()
            .contains("grpc_server_shed_requests_total 4")
    );

    drop(follower);
    let response = app
        .clone()
        .oneshot(opening_request(UNARY_PATH, None, "missing"))
        .await
        .unwrap();
    assert_eq!(
        grpc_status(&response).unwrap().code(),
        Code::Unauthenticated
    );
    drop(leader);
    *provider.gate.lock() = None;
    gate.release();
    let response = timeout(
        WAIT,
        app.oneshot(opening_request(UNARY_PATH, Some("recovered"), "recovered")),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(grpc_status(&response).is_none());
    assert_eq!(echo.calls.lock().len(), 1);
    provider.stop().await;
}

// template:end oidc-introspection:grpc-transport-test-provider-admission-followers

// template:begin authn:grpc-transport-test-unpolled-unary
#[tokio::test]
async fn an_unpolled_unary_response_releases_capacity_at_the_caller_deadline() {
    use http_body_util::BodyExt as _;

    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 1);
    let call = || opening_request(UNARY_PATH, Some(BearerMode::SELECTED.token(true)), "echo");
    let warm = app.clone().oneshot(call()).await.unwrap();
    assert!(grpc_status(&warm).is_none());
    drop(warm);

    tokio::time::pause();
    let mut limited = call();
    limited
        .headers_mut()
        .insert("grpc-timeout", "1S".parse().unwrap());
    let mut held = app.clone().oneshot(limited).await.unwrap();
    assert!(grpc_status(&held).is_none());
    let waiting = app.oneshot(call());
    tokio::pin!(waiting);
    assert!(
        timeout(Duration::from_millis(900), &mut waiting)
            .await
            .is_err()
    );
    let admitted = timeout(Duration::from_millis(200), waiting)
        .await
        .expect("expiry admits the waiting call while the first body is unpolled")
        .unwrap();
    assert!(grpc_status(&admitted).is_none());
    let frame = held.body_mut().frame().await.unwrap().unwrap();
    let status = Status::from_header_map(frame.trailers_ref().unwrap()).unwrap();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    tokio::time::resume();
    bearer.stop().await;
}

// template:end authn:grpc-transport-test-unpolled-unary

// template:begin oidc-introspection:grpc-transport-test-provider-admission-timeout
#[tokio::test]
async fn opening_and_terminal_capacity_have_independent_lifetimes() {
    let (verifier, provider) = verifier_fixture().await;
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 2);
    let stream_path = "/example.v1.EchoService/ServerStream";
    let first = app
        .clone()
        .oneshot(opening_request(
            stream_path,
            Some(BearerMode::SELECTED.token(true)),
            "hold-after-first",
        ))
        .await
        .unwrap();
    assert!(grpc_status(&first).is_none());
    let gate = Arc::new(Hold::new());
    *provider.gate.lock() = Some(Arc::clone(&gate));
    let mut slow = opening_request(UNARY_PATH, Some("slow"), "slow");
    slow.headers_mut()
        .insert("grpc-timeout", "1500m".parse().unwrap());
    let mut authenticating = Box::pin(app.clone().oneshot(slow));
    timeout(WAIT, async {
        tokio::select! {
            () = gate.wait_for_entries(1) => {},
            result = &mut authenticating => panic!("authentication must wait: {result:?}"),
        }
    })
    .await
    .unwrap();
    let second = timeout(
        WAIT,
        app.clone().oneshot(opening_request(
            stream_path,
            Some(ACCEPTED),
            "hold-after-first",
        )),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(grpc_status(&second).is_none());
    let missing = app
        .clone()
        .oneshot(opening_request(UNARY_PATH, None, "missing"))
        .await
        .unwrap();
    assert_eq!(grpc_status(&missing).unwrap().code(), Code::Unauthenticated);
    gate.release();
    let response = timeout(WAIT, authenticating).await.unwrap().unwrap();
    assert_eq!(
        grpc_status(&response).unwrap().code(),
        Code::ResourceExhausted
    );
    assert_eq!(echo.calls.lock().len(), 2);
    drop(first);
    drop(second);
    let recovered = app
        .oneshot(opening_request(UNARY_PATH, Some(ACCEPTED), "recovered"))
        .await
        .unwrap();
    assert!(grpc_status(&recovered).is_none());
    provider.stop().await;
}

#[tokio::test]
async fn opening_timeout_releases_capacity_and_zero_disables_both_counts() {
    for capacity in [1, 0] {
        let (verifier, provider) = verifier_fixture().await;
        let echo = Echo::new();
        let app = opening_app(verifier, &echo, capacity);
        let gate = Arc::new(Hold::new());
        *provider.gate.lock() = Some(Arc::clone(&gate));
        let mut request = opening_request(UNARY_PATH, Some(ACCEPTED), "slow");
        request
            .headers_mut()
            .insert("grpc-timeout", "100m".parse().unwrap());
        let mut slow = Box::pin(app.clone().oneshot(request));
        timeout(WAIT, async {
            tokio::select! {
                () = gate.wait_for_entries(1) => {},
                result = &mut slow => panic!("authentication must wait: {result:?}"),
            }
        })
        .await
        .unwrap();
        let missing = opening_request(UNARY_PATH, None, "missing");
        let response = app
            .clone()
            .oneshot(if capacity == 0 {
                missing
            } else {
                impatient_raw(missing)
            })
            .await
            .unwrap();
        assert_eq!(
            grpc_status(&response).unwrap().code(),
            if capacity == 0 {
                Code::Unauthenticated
            } else {
                Code::ResourceExhausted
            }
        );
        let response = timeout(WAIT, slow).await.unwrap().unwrap();
        assert_eq!(
            grpc_status(&response).unwrap().code(),
            Code::DeadlineExceeded
        );
        assert_eq!(provider.calls.load(Ordering::Acquire), 1);
        *provider.gate.lock() = None;
        let mut held = Vec::new();
        for _ in 0..=usize::from(capacity == 0) {
            let response = timeout(
                WAIT,
                app.clone().oneshot(opening_request(
                    "/example.v1.EchoService/ServerStream",
                    Some("recovered"),
                    "hold-after-first",
                )),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(grpc_status(&response).is_none());
            held.push(response);
        }
        drop(held);
        provider.stop().await;
    }
}

// template:end oidc-introspection:grpc-transport-test-provider-admission-timeout

// template:begin authn:grpc-transport-test-admission
#[tokio::test]
async fn a_call_over_the_limit_waits_for_a_permit_and_is_served() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 1);
    let held = hold_capacity(&app).await;
    assert!(grpc_status(&held).is_none());
    let mut waiting = Box::pin(app.clone().oneshot(opening_request(
        UNARY_PATH,
        Some(BearerMode::SELECTED.token(true)),
        "waited",
    )));
    assert!(
        timeout(Duration::from_millis(200), &mut waiting)
            .await
            .is_err(),
        "the call waits instead of being refused"
    );
    assert!(
        metrics.render().contains("grpc_server_admission_waiting 1"),
        "the waiting call is gauged"
    );
    drop(held);
    let response = timeout(WAIT, waiting).await.unwrap().unwrap();
    assert!(grpc_status(&response).is_none());
    assert_eq!(echo.calls.lock().len(), 2);
    let rendered = metrics.render();
    assert!(rendered.contains("grpc_server_shed_requests_total 0"));
    assert!(rendered.contains("grpc_server_admission_wait_seconds_count 1"));
    assert!(rendered.contains("grpc_server_admission_waiting 0"));
    bearer.stop().await;
}

#[tokio::test]
async fn a_waiting_call_with_an_open_upload_is_refused_with_retry_info() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 1);
    let held = hold_capacity(&app).await;
    let call = pending_upload(
        opening_request(
            "/example.v1.EchoService/BidiStream",
            Some(BearerMode::SELECTED.token(true)),
            "refused",
        ),
        "500m",
    );
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let response = timeout(WAIT, app.clone().oneshot(call))
        .await
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "the call waited for its budget less the reserve"
    );
    tokio::time::resume();
    assert_at_capacity(&grpc_status(&response).unwrap());
    let rendered = metrics.render();
    assert!(rendered.contains("grpc_server_shed_requests_total 1"));
    assert!(rendered.contains("grpc_server_admission_wait_seconds_count 1"));
    drop(held);
    bearer.stop().await;
}

#[tokio::test]
async fn a_call_waits_no_longer_than_the_admission_wait() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app_waiting(verifier, &echo, 1, Duration::from_millis(200));
    let held = hold_capacity(&app).await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let response = timeout(
        WAIT,
        app.clone().oneshot(opening_request(
            UNARY_PATH,
            Some(BearerMode::SELECTED.token(true)),
            "refused",
        )),
    )
    .await
    .expect("the refusal is answered long before the call budget")
    .unwrap();
    assert!(
        (Duration::from_millis(200)..Duration::from_millis(210)).contains(&started.elapsed()),
        "waited {:?}",
        started.elapsed()
    );
    tokio::time::resume();
    assert_at_capacity(&grpc_status(&response).unwrap());
    let rendered = metrics.render();
    assert!(rendered.contains("grpc_server_shed_requests_total 1"));
    assert!(rendered.contains("grpc_server_admission_wait_seconds_count 1"));
    assert!(rendered.contains("grpc_server_admission_waiting 0"));
    drop(held);
    bearer.stop().await;
}

// template:end authn:grpc-transport-test-admission

// template:begin authn:grpc-transport-test-shared-queue-limit
#[tokio::test]
async fn both_admission_queues_share_the_call_wait_limit() {
    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app_waiting(verifier, &echo, 1, Duration::from_secs(1));
    let mut holding = Box::pin(app.clone().oneshot(opening_request(
        "/example.v1.EchoService/ServerStream",
        Some(BearerMode::SELECTED.token(true)),
        "hold-before-headers",
    )));
    timeout(WAIT, async {
        tokio::select! {
            () = echo.hold.wait_for_entries(1) => {},
            result = &mut holding => panic!("the stream must hold its opening: {result:?}"),
        }
    })
    .await
    .unwrap();

    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let mut waiting = Box::pin(app.oneshot(opening_request(
        UNARY_PATH,
        Some(BearerMode::SELECTED.token(true)),
        "queued",
    )));
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    tokio::time::advance(Duration::from_millis(750)).await;

    echo.hold.release();
    let held = timeout(Duration::from_millis(100), holding)
        .await
        .expect("the held stream returns headers")
        .unwrap();
    assert!(grpc_status(&held).is_none());

    let response = timeout(Duration::from_millis(300), waiting)
        .await
        .expect("the second queue uses the remaining wait budget")
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_at_capacity(&grpc_status(&response).unwrap());
    assert_seen(&echo, &[Seen::ServerStream]);
    drop(held);
    tokio::time::resume();
    bearer.stop().await;
}

// template:end authn:grpc-transport-test-shared-queue-limit

// template:begin authn:grpc-transport-test-zero-admission
#[tokio::test]
async fn a_zero_admission_wait_refuses_a_call_over_the_limit_at_once() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let (verifier, bearer) = BearerFixture::start().await;
    let echo = Echo::new();
    let app = opening_app_waiting(verifier, &echo, 1, Duration::ZERO);
    let held = hold_capacity(&app).await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let response = timeout(
        WAIT,
        app.clone().oneshot(opening_request(
            UNARY_PATH,
            Some(BearerMode::SELECTED.token(true)),
            "refused",
        )),
    )
    .await
    .expect("the refusal is answered")
    .unwrap();
    assert_eq!(started.elapsed(), Duration::ZERO);
    tokio::time::resume();
    assert_at_capacity(&grpc_status(&response).unwrap());
    let rendered = metrics.render();
    assert!(rendered.contains("grpc_server_shed_requests_total 1"));
    assert!(!rendered.contains("grpc_server_admission_wait_seconds_count"));
    drop(held);
    bearer.stop().await;
}

#[tokio::test]
async fn rejected_ready_empty_frames_cooperate_with_other_tasks() {
    let (verifier, bearer) = BearerFixture::start().await;
    let app = opening_app(verifier, &Echo::new(), 1);
    let progressed = Arc::new(AtomicBool::new(false));
    let other = tokio::spawn({
        let progressed = Arc::clone(&progressed);
        async move {
            progressed.store(true, Ordering::Release);
        }
    });
    let mut request = opening_request(UNARY_PATH, None, "");
    *request.body_mut() = axum::body::Body::from_stream(futures_util::stream::iter(
        std::iter::repeat_n(Ok::<_, std::convert::Infallible>(bytes::Bytes::new()), 1024),
    ));
    let response = timeout(WAIT, app.oneshot(request)).await.unwrap().unwrap();
    assert_eq!(
        grpc_status(&response).unwrap().code(),
        Code::Unauthenticated
    );
    assert!(
        progressed.load(Ordering::Acquire),
        "ready empty frames must yield before completing drainage"
    );
    timeout(WAIT, other).await.unwrap().unwrap();
    bearer.stop().await;
}

// template:end authn:grpc-transport-test-zero-admission

// template:begin authn:grpc-transport-test-verified-principal
#[tokio::test]
async fn verified_principal_exposes_identity_and_spoofed_extensions_cannot_bypass_auth() {
    let (verifier, bearer) = BearerFixture::start().await;
    let header = format!("Bearer {}", BearerMode::SELECTED.token(false));
    let (_, previously_verified) = verifier
        .authenticate_presented(
            &operation_context::OperationContext::unbounded(),
            [header.as_bytes()],
            infra_bearerauthn::Transport::Grpc,
        )
        .await
        .unwrap();
    let forged = previously_verified.into_delegation_credential();
    let echo = Echo::new();
    let app = opening_app(verifier, &echo, 4);

    let mut request = opening_request(
        UNARY_PATH,
        Some(BearerMode::SELECTED.token(true)),
        "genuine",
    )
    .map(|body| {
        use http_body_util::BodyExt as _;
        let mut trailers = http::HeaderMap::new();
        trailers.insert(http::header::AUTHORIZATION, "redacted".parse().unwrap());
        trailers.insert("x-checksum", "123".parse().unwrap());
        axum::body::Body::new(body.with_trailers(async move { Some(Ok(trailers)) }))
    });
    request.extensions_mut().insert(forged.clone());
    let genuine = timeout(WAIT, app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap();
    assert!(grpc_status(&genuine).is_none());

    let (captured, credential) = echo
        .identities
        .lock()
        .pop()
        .expect("handler observed Principal");
    assert!(credential.is_none());
    assert_eq!(captured.issuer(), "https://issuer.example");
    assert_eq!(captured.subject(), Some("subject"));
    assert_eq!(captured.scopes(), ["orders.read"]);
    let calls = echo.calls.lock().clone();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].had_checksum);
    assert!(!calls[0].had_authorization);

    let mut spoofed = opening_request(UNARY_PATH, None, "spoofed");
    spoofed.extensions_mut().insert(captured);
    spoofed.extensions_mut().insert(forged);
    let rejected = timeout(WAIT, app.oneshot(spoofed)).await.unwrap().unwrap();
    let status = grpc_status(&rejected).unwrap();
    assert_eq!(status.code(), Code::Unauthenticated);
    assert_eq!(reason(&status), "AUTHENTICATION_REQUIRED");
    assert!(echo.identities.lock().is_empty());
    assert_eq!(echo.calls.lock().len(), 1);

    bearer.stop().await;
}
// template:end authn:grpc-transport-test-verified-principal

#[tokio::test]
async fn the_configured_limit_sheds_the_next_call_without_starving_health() {
    let fixture = Fixture::limited(2).await;
    timeout(WAIT, fixture.echo_client().unary(request(unary("warm"))))
        .await
        .expect("warm call")
        .unwrap();

    let held = fixture.echo_client();
    let calls = Vec::from_fn(2, |_| {
        let mut client = held.clone();
        tokio::spawn(async move { client.unary(request(unary("hold"))).await })
    });
    fixture.echo.hold.wait_for_entries(2).await;

    let exhausted = timeout(
        WAIT,
        fixture
            .echo_client()
            .unary(impatient(request(unary("overflow")))),
    )
    .await
    .expect("overflow call")
    .unwrap_err();
    assert_at_capacity(&exhausted);

    let serving = timeout(
        WAIT,
        fixture.health().check(Request::new(HealthCheckRequest {
            service: String::new(),
        })),
    )
    .await
    .expect("health during capacity")
    .unwrap();
    assert_eq!(serving.into_inner().status, ServingStatus::Serving as i32);

    fixture.echo.hold.release();
    timeout(WAIT, async {
        for call in calls {
            let response = call.await.expect("held call joins").unwrap();
            assert_eq!(response.into_inner().message, "released");
        }
    })
    .await
    .expect("held calls finish");

    let followed = timeout(WAIT, fixture.echo_client().unary(request(unary("after"))))
        .await
        .expect("call after release")
        .unwrap();
    assert_eq!(followed.into_inner().message, "after");
    drop(held);
    fixture.stop().await;
}

#[tokio::test]
async fn one_connection_serves_concurrent_rpcs_and_capacity_returns_after_it_closes() {
    let fixture = Fixture::open_with_at(
        true,
        None,
        Limits {
            max_in_flight: NonZeroU32::new(2),
            ..limits()
        },
        ConnectionLimits {
            max_connections: Some(NonZeroU32::MIN),
            ..connection_limits()
        },
        loopback(),
        grpc_contracts::FILE_DESCRIPTOR_SET,
        None,
    )
    .await;
    let mut first = fixture.echo_client();
    let mut second = first.clone();
    let (first_reply, second_reply, ()) = timeout(WAIT, async {
        tokio::join!(
            first.unary(request(unary("hold"))),
            second.unary(request(unary("hold"))),
            async {
                fixture.echo.hold.wait_for_entries(2).await;
                let mut excess = TcpStream::connect(fixture.address).await.unwrap();
                match excess.read(&mut [0]).await {
                    Ok(bytes) => assert_eq!(bytes, 0, "excess peer received HTTP/2 bytes"),
                    Err(error) => assert!(matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset
                    )),
                }
                fixture.echo.hold.release();
            }
        )
    })
    .await
    .expect("one connection serves both calls and refuses the excess peer");
    assert_eq!(first_reply.unwrap().into_inner().message, "released");
    assert_eq!(second_reply.unwrap().into_inner().message, "released");
    drop((first, second));

    let recovered = timeout(WAIT, async {
        loop {
            match fixture
                .echo_client()
                .unary(request(unary("recovered")))
                .await
            {
                Err(error) if error.code() == Code::Unavailable => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                result => break result,
            }
        }
    })
    .await
    .expect("the closed connection releases capacity")
    .expect("a new connection serves an RPC");
    assert_eq!(recovered.into_inner().message, "recovered");
    fixture.stop().await;
}

#[tokio::test]
async fn opened_stream_holds_capacity_until_terminal_release() {
    let fixture = Fixture::limited(1).await;
    let mut client = fixture.echo_client();
    let mut stream = timeout(
        WAIT,
        client.server_stream(request(ServerStreamRequest {
            message: "hold-after-first".to_owned(),
        })),
    )
    .await
    .expect("stream opens")
    .unwrap()
    .into_inner();
    assert_eq!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .message,
        "hold-after-first"
    );
    fixture.echo.hold.wait_for_entries(1).await;
    let second = timeout(WAIT, client.unary(impatient(request(unary("must shed")))))
        .await
        .expect("overload is answered");
    fixture.echo.hold.release();
    assert_eq!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .message,
        "released"
    );
    assert!(
        timeout(WAIT, stream.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let recovered = timeout(WAIT, client.unary(request(unary("admitted"))))
        .await
        .expect("capacity recovers")
        .unwrap();
    fixture.stop().await;
    assert_eq!(second.unwrap_err().code(), Code::ResourceExhausted);
    assert_eq!(recovered.into_inner().message, "admitted");
}

#[tokio::test]
async fn duplex_progresses_before_request_eof_and_drop_stops_upload_and_recovers_capacity() {
    let fixture = Fixture::limited(1).await;
    let mut client = fixture.echo_client();
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let upload_dropped = Arc::new(Notify::new());
    let dropped = notify_on_drop(Arc::clone(&upload_dropped));
    let upload = async_stream::stream! {
        let _dropped = dropped;
        let mut receiver = receiver;
        while let Some(item) = receiver.recv().await {
            yield item;
        }
    };
    let mut response = timeout(WAIT, client.bidi_stream(request(upload)))
        .await
        .expect("duplex opens before upload EOF")
        .unwrap()
        .into_inner();
    for message in ["first", "second", "third"] {
        timeout(
            WAIT,
            sender.send(BidiStreamRequest {
                message: message.to_owned(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            timeout(WAIT, response.message())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .message,
            message
        );
    }
    let shed = timeout(WAIT, client.unary(impatient(request(unary("occupied")))))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(shed.code(), Code::ResourceExhausted);
    drop(response);
    timeout(WAIT, upload_dropped.notified())
        .await
        .expect("client drops upload source");
    timeout(WAIT, sender.closed())
        .await
        .expect("upload producer observes cancellation");
    timeout(WAIT, fixture.echo.stream_dropped.notified())
        .await
        .expect("server drops request/response work");
    let admitted = timeout(WAIT, client.unary(request(unary("recovered"))))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admitted.into_inner().message, "recovered");
    drop(sender);
    fixture.stop().await;
}

#[tokio::test]
async fn client_clones_share_opening_capacity_until_cancellation_or_response_headers() {
    let fixture = Fixture::plaintext().await;
    let new_client = || {
        EchoServiceClient::new(
            Client::builder(
                &format!("http://{}", fixture.address),
                ClientSecurity::Plaintext,
                ClientTimeout::OpeningOnly(CLIENT_TIMEOUT),
            )
            .opening_limit(NonZeroUsize::MIN)
            .build()
            .unwrap(),
        )
    };
    let mut client = new_client();
    let mut clone = client.clone();
    let parent = operation_context::OperationContext::unbounded();
    let mut held_request = request(unary("hold"));
    held_request.extensions_mut().insert(parent.clone());
    let mut opening = Box::pin(client.unary(held_request));
    timeout(WAIT, async {
        tokio::select! {
            () = fixture.echo.hold.wait_for_entries(1) => {},
            result = &mut opening => panic!("the opening must remain held: {result:?}"),
        }
    })
    .await
    .unwrap();

    let error = timeout(WAIT, clone.unary(request(unary("shed"))))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), Code::ResourceExhausted);
    let mut cancelled = request(unary("cancelled"));
    let stopped = operation_context::OperationContext::unbounded();
    stopped.cancel();
    cancelled.extensions_mut().insert(stopped);
    assert_eq!(
        clone.unary(cancelled).await.unwrap_err().code(),
        Code::Cancelled
    );
    assert_seen(&fixture.echo, &[Seen::Unary]);

    let separate = timeout(WAIT, new_client().unary(request(unary("independent"))))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(separate.into_inner().message, "independent");
    parent.cancel();
    assert_eq!(
        timeout(WAIT, opening).await.unwrap().unwrap_err().code(),
        Code::Cancelled
    );

    let mut response = timeout(
        WAIT,
        clone.server_stream(request(ServerStreamRequest {
            message: "hold-after-first".to_owned(),
        })),
    )
    .await
    .unwrap()
    .unwrap()
    .into_inner();
    assert_eq!(
        timeout(WAIT, response.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .message,
        "hold-after-first"
    );
    fixture.echo.hold.wait_for_entries(2).await;
    let second = timeout(WAIT, client.unary(request(unary("after-headers"))))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.into_inner().message, "after-headers");
    fixture.echo.hold.release();
    assert_eq!(
        timeout(WAIT, response.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .message,
        "released"
    );
    assert_seen(
        &fixture.echo,
        &[Seen::Unary, Seen::Unary, Seen::ServerStream, Seen::Unary],
    );
    fixture.stop().await;
}

#[tokio::test]
async fn opened_stream_without_caller_deadline_outlives_both_opening_caps() {
    let fixture = Fixture::open_with(
        true,
        None,
        Limits {
            request_timeout: Duration::from_millis(100),
            max_in_flight: NonZeroU32::new(1),
            ..limits()
        },
    )
    .await;
    let adapter = Client::builder(
        &format!("http://{}", fixture.address),
        ClientSecurity::Plaintext,
        ClientTimeout::OpeningOnly(Duration::from_millis(100)),
    )
    .build()
    .unwrap();
    let mut client = EchoServiceClient::new(adapter);
    let mut response = timeout(
        WAIT,
        client.server_stream(request(ServerStreamRequest {
            message: "hold-after-first".to_owned(),
        })),
    )
    .await
    .unwrap()
    .unwrap()
    .into_inner();
    assert!(
        timeout(WAIT, response.message())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    fixture.echo.hold.wait_for_entries(1).await;
    assert!(
        timeout(Duration::from_millis(200), response.message())
            .await
            .is_err()
    );
    fixture.echo.hold.release();
    assert_eq!(
        timeout(WAIT, response.message())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .message,
        "released"
    );
    assert!(
        timeout(WAIT, response.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    fixture.stop().await;
}

#[tokio::test]
async fn receiver_gets_only_the_remaining_rpc_lifetime_after_admission_and_metadata_edits() {
    use ClientTimeout::{FullRpc, OpeningOnly};
    let (received, mut wire) = tokio::sync::mpsc::unbounded_channel();
    let peer = move |request: axum::extract::Request| {
        received.send(request.into_parts().0.headers).unwrap();
        async { Status::new(Code::Ok, "").into_http::<axum::body::Body>() }
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the loopback peer records the timeout it received over HTTP/2"
    )]
    let app = axum::Router::new().route(UNARY_PATH, axum::routing::post(peer));
    let server = BoundListener::bind(loopback()).unwrap().serve_h2(
        app,
        server_options(connection_limits()),
        None,
    );
    let destination = format!("http://{}", server.local_addr());
    let local = Duration::from_secs(5);
    let admission = Duration::from_secs(1);
    for (policy, original, parent_seconds, lifetime_seconds) in [
        (FullRpc(local), None, None, Some(5)),
        (FullRpc(local), Some("30S"), Some(10), Some(5)),
        (FullRpc(local), Some("3S"), Some(10), Some(3)),
        (FullRpc(local), Some("30S"), Some(1), Some(1)),
        (FullRpc(local), Some("30S"), Some(3), Some(3)),
        (OpeningOnly(local), None, None, None),
        (OpeningOnly(local), Some("30S"), None, Some(30)),
        (OpeningOnly(local), Some("30S"), Some(10), Some(10)),
    ] {
        let client = Client::builder(&destination, ClientSecurity::Plaintext, policy)
            .build()
            .unwrap();
        let mut request = http::Request::builder()
            .method("POST")
            .uri(UNARY_PATH)
            .header("content-type", "application/grpc")
            .body(tonic::body::Body::empty())
            .unwrap();
        if let Some(original) = original {
            request
                .headers_mut()
                .insert("grpc-timeout", http::HeaderValue::from_static(original));
        }
        tokio::time::pause();
        let origin = tokio::time::Instant::now();
        if let Some(seconds) = parent_seconds {
            request
                .extensions_mut()
                .insert(operation_context::OperationContext::with_timeout(
                    Duration::from_secs(seconds),
                ));
        }
        let (prepared, ()) = PreparedCall::prepare(&client, request)
            .unwrap()
            .try_step(|_, headers| {
                headers.insert("grpc-timeout", http::HeaderValue::from_static("1H"));
                Ok(())
            })
            .unwrap();
        tokio::time::advance(admission).await;
        tokio::time::resume();
        let result = timeout(WAIT, prepared.send()).await.unwrap();
        let lifetime = lifetime_seconds.map(Duration::from_secs);
        if lifetime.is_some_and(|budget| budget <= admission) {
            assert_eq!(result.unwrap_err().code(), Code::DeadlineExceeded);
            assert!(matches!(
                wire.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
            continue;
        }
        let response = result.unwrap();
        assert_eq!(response.headers()["grpc-status"], "0");
        let headers = wire.try_recv().unwrap();
        if let Some(lifetime) = lifetime {
            let forwarded = headers
                .get(shared_rules::grpc::TIMEOUT_HEADER)
                .and_then(|value| shared_rules::grpc::parse_timeout(value.as_bytes()))
                .unwrap();
            let precision = Duration::from_micros(1);
            let earliest = lifetime.saturating_sub(origin.elapsed() + precision);
            let latest = lifetime.saturating_sub(admission) + precision;
            assert!(
                (earliest..=latest).contains(&forwarded),
                "{policy:?} {original:?} {parent_seconds:?}: {forwarded:?}"
            );
        } else {
            assert!(!headers.contains_key("grpc-timeout"));
        }
    }
    assert_eq!(
        server.drain(Duration::from_secs(1)).await.unwrap(),
        Drained::Complete
    );
}

#[tokio::test]
async fn supplied_caller_deadline_expires_open_stream_and_recovers_capacity() {
    let fixture = Fixture::limited(1).await;
    let adapter = Client::builder(
        &format!("http://{}", fixture.address),
        ClientSecurity::Plaintext,
        ClientTimeout::OpeningOnly(Duration::from_secs(1)),
    )
    .build()
    .unwrap();
    let mut client = EchoServiceClient::new(adapter);
    let mut call = request(ServerStreamRequest {
        message: "hold-after-first".to_owned(),
    });
    call.set_timeout(Duration::from_millis(200));
    let mut response = timeout(WAIT, client.server_stream(call))
        .await
        .unwrap()
        .unwrap()
        .into_inner();
    assert!(
        timeout(WAIT, response.message())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    let error = timeout(WAIT, response.message())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
    timeout(WAIT, fixture.echo.stream_dropped.notified())
        .await
        .expect("deadline releases server stream");
    let recovered = timeout(WAIT, client.unary(request(unary("recovered"))))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.into_inner().message, "recovered");
    fixture.stop().await;
}

#[tokio::test]
async fn unread_flow_controlled_response_releases_server_work_at_caller_deadline() {
    let fixture = Fixture::limited(1).await;
    let stream = TcpStream::connect(fixture.address).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(stream),
    )
    .await
    .unwrap();
    let driver = tokio::spawn(connection);
    let builder = http::Request::builder()
        .method("POST")
        .uri(format!(
            "http://{}/example.v1.EchoService/ServerStream",
            fixture.address
        ))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-timeout", "300m");
    let mut outgoing = builder
        .body(axum::body::Body::from(grpc_frame("flood")))
        .unwrap();
    outgoing
        .headers_mut()
        .extend(request(()).into_parts().0.into_headers());
    let response = timeout(WAIT, sender.send_request(outgoing))
        .await
        .unwrap()
        .unwrap();
    timeout(WAIT, fixture.echo.stream_dropped.notified())
        .await
        .expect("unread server stream is released");
    let recovered = timeout(
        WAIT,
        fixture.echo_client().unary(request(unary("recovered"))),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.into_inner().message, "recovered");
    drop(response);
    drop(sender);
    driver.abort();
    let _ = driver.await;
    fixture.stop().await;
}

#[tokio::test]
async fn unary_deadline_is_deadline_exceeded_on_the_raw_http2_response() {
    let fixture = Fixture::plaintext().await;
    let address = fixture.address;
    let call = tokio::spawn(async move { raw_call(address, UNARY_PATH).await });
    fixture.echo.hold.wait_for_entries(1).await;
    let status = timeout(WAIT, call)
        .await
        .expect("raw deadline response")
        .expect("raw deadline task");
    assert_eq!(status, "4");
    fixture.stop().await;
}

async fn raw_call(address: SocketAddr, path: &str) -> String {
    let stream = TcpStream::connect(address).await.expect("connect");
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(stream),
    )
    .await
    .expect("http2 handshake");
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    let builder = http::Request::builder()
        .method("POST")
        .uri(format!("http://{address}{path}"))
        .header("host", address.to_string())
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-timeout", "100m");
    // template:begin authn:grpc-transport-test-deadline-bearer
    let builder = builder.header(
        "authorization",
        format!("Bearer {}", BearerMode::SELECTED.token(true)),
    );
    // template:end authn:grpc-transport-test-deadline-bearer
    let request = builder
        .body(axum::body::Body::from(grpc_frame("deadline")))
        .unwrap();
    let pending = tokio::spawn(async move { sender.send_request(request).await });
    let response = pending.await.expect("request task").expect("response");
    let status = response
        .headers()
        .get("grpc-status")
        .expect("server status")
        .to_str()
        .unwrap()
        .to_owned();
    driver.abort();
    status
}

const UNARY_PATH: &str = "/example.v1.EchoService/Unary";

fn grpc_frame(message: &str) -> bytes::Bytes {
    use bytes::{BufMut as _, BytesMut};
    use prost::Message as _;

    let payload = unary(message);
    let len = payload.encoded_len();
    let mut frame = BytesMut::with_capacity(5 + len);
    frame.put_u8(0);
    frame.put_u32(u32::try_from(len).unwrap());
    payload.encode(&mut frame).unwrap();
    frame.freeze()
}

#[tokio::test]
async fn metadata_up_to_16_kib_is_served_and_larger_metadata_is_refused() {
    use http_body_util::{BodyExt as _, Full};

    let fixture = Fixture::plaintext().await;
    let stream = TcpStream::connect(fixture.address).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http2::handshake(
        hyper_util::rt::TokioExecutor::new(),
        hyper_util::rt::TokioIo::new(stream),
    )
    .await
    .unwrap();
    let driver = tokio::spawn(connection);
    let frame = grpc_frame("padded");
    let padded = |budget: usize| {
        let mut call = http::Request::post(format!("http://{}{UNARY_PATH}", fixture.address))
            .header("content-type", "application/grpc")
            .header("content-length", frame.len())
            .header("te", "trailers")
            .header("grpc-timeout", "5S")
            .header("x-padding", "")
            .body(Full::new(frame.clone()))
            .unwrap();
        call.headers_mut()
            .extend(request(()).into_parts().0.into_headers());
        // RFC 9113 §6.5.2, SETTINGS_MAX_HEADER_LIST_SIZE
        let size: usize = [
            (":method", call.method().as_str().len()),
            (":scheme", call.uri().scheme_str().unwrap().len()),
            (":authority", call.uri().authority().unwrap().as_str().len()),
            (":path", call.uri().path().len()),
        ]
        .into_iter()
        .chain(
            call.headers()
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes().len())),
        )
        .map(|(name, value_len)| name.len() + value_len + 32)
        .sum();
        call.headers_mut()
            .insert("x-padding", "a".repeat(budget - size).parse().unwrap());
        call
    };
    let served = timeout(WAIT, sender.send_request(padded(16 * 1024 - 1)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(served.status(), http::StatusCode::OK);
    let body = timeout(WAIT, served.into_body().collect())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body.trailers().unwrap()["grpc-status"], "0");
    assert_eq!(body.to_bytes(), frame);
    let refused = timeout(WAIT, sender.send_request(padded(16 * 1024 + 1)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        refused.status(),
        http::StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
    assert_seen(&fixture.echo, &[Seen::Unary]);
    drop(sender);
    driver.abort();
    fixture.stop().await;
}

#[tokio::test]
async fn handler_panic_is_internal_and_the_server_keeps_serving() {
    let fixture = Fixture::plaintext().await;
    let error = timeout(WAIT, fixture.echo_client().unary(request(unary("panic"))))
        .await
        .expect("panic call")
        .unwrap_err();
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(error.message(), "request failed");
    assert_eq!(reason(&error), "INTERNAL_ERROR");
    let followed = timeout(
        WAIT,
        fixture.echo_client().unary(request(unary("after-panic"))),
    )
    .await
    .expect("call after panic")
    .unwrap();
    assert_eq!(followed.into_inner().message, "after-panic");
    fixture.stop().await;
}

#[tokio::test]
async fn a_handler_failure_carries_its_reason_and_field_violations() {
    let fixture = Fixture::plaintext().await;
    let error = timeout(
        WAIT,
        fixture.echo_client().unary(request(unary("classified"))),
    )
    .await
    .expect("classified call")
    .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    assert_eq!(reason(&error), "BAD_REQUEST");
    let details = error.get_error_details();
    let violations = &details.bad_request().expect("bad request").field_violations;
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].field, "message");
    assert_eq!(violations[0].description, "is reserved");
    fixture.stop().await;
}

#[tokio::test]
async fn health_watch_honors_the_caller_timeout_after_its_initial_status() {
    let fixture = Fixture::unseeded().await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{}", fixture.address))
        .unwrap()
        .connect_lazy();
    let mut health = HealthClient::new(channel);
    let mut request = Request::new(HealthCheckRequest {
        service: ECHO_SERVICE.to_owned(),
    });
    request.set_timeout(Duration::from_secs(2));
    let mut stream = timeout(WAIT, health.watch(request))
        .await
        .unwrap()
        .unwrap()
        .into_inner();
    let first = timeout(WAIT, stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.status, ServingStatus::NotServing as i32);
    let status = timeout(WAIT, stream.message()).await.unwrap().unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    drop(stream);
    drop(health);
    fixture.stop().await;
}

#[test]
fn the_builtin_health_name_cannot_be_registered_as_a_business_service() {
    let mut services = Services::new();
    services
        .describe(tonic_health::pb::FILE_DESCRIPTOR_SET)
        .unwrap();
    let (_, health) = tonic_health::server::health_reporter();
    assert_matches!(
        services.add(health).unwrap_err(),
        Error::DuplicateService("grpc.health.v1.Health")
    );
}

#[tokio::test]
async fn health_watch_ends_on_drain_and_the_listener_drain_completes() {
    let mut fixture = Fixture::unseeded().await;
    let mut health = fixture.health();
    let before = timeout(
        WAIT,
        health.check(Request::new(HealthCheckRequest {
            service: ECHO_SERVICE.to_owned(),
        })),
    )
    .await
    .expect("check before seed")
    .unwrap();
    assert_eq!(before.into_inner().status, ServingStatus::NotServing as i32);
    let unknown = timeout(
        WAIT,
        health.check(Request::new(HealthCheckRequest {
            service: "missing".to_owned(),
        })),
    )
    .await
    .expect("unknown check")
    .unwrap_err();
    assert_eq!(unknown.code(), Code::NotFound);

    fixture
        .pending_activation
        .take()
        .unwrap()
        .activate()
        .unwrap();
    let mut known = timeout(WAIT, health.watch(watch_request(ECHO_SERVICE)))
        .await
        .expect("known watch")
        .unwrap()
        .into_inner();
    assert_eq!(
        timeout(WAIT, known.message())
            .await
            .expect("known watch item")
            .unwrap()
            .unwrap()
            .status,
        ServingStatus::Serving as i32
    );
    let mut unknown_watch = timeout(WAIT, health.watch(watch_request("missing")))
        .await
        .expect("unknown watch")
        .unwrap()
        .into_inner();
    assert_eq!(
        timeout(WAIT, unknown_watch.message())
            .await
            .expect("unknown watch item")
            .unwrap()
            .unwrap()
            .status,
        ServingStatus::ServiceUnknown as i32
    );

    let server = fixture.take_server();
    let drain = tokio::spawn(async move { server.drain(Duration::from_secs(3)).await });
    fixture.readiness.start_drain();
    assert_eq!(
        timeout(WAIT, known.message())
            .await
            .expect("drain status")
            .unwrap()
            .unwrap()
            .status,
        ServingStatus::NotServing as i32
    );
    assert!(
        timeout(WAIT, known.message())
            .await
            .expect("known watch end")
            .unwrap()
            .is_none()
    );
    assert!(
        timeout(WAIT, unknown_watch.message())
            .await
            .expect("unknown watch end")
            .unwrap()
            .is_none()
    );
    drop(known);
    drop(unknown_watch);
    drop(health);
    let drained = timeout(WAIT, drain)
        .await
        .expect("drain task")
        .expect("drain joins")
        .expect("drain succeeds");
    assert_eq!(drained, Drained::Complete);
    fixture.stop().await;
}

#[tokio::test]
async fn cooperative_stream_closes_on_listener_drain_while_inflight_unary_completes() {
    let mut fixture = Fixture::plaintext().await;
    let mut stream_client = fixture.echo_client();
    let mut stream = timeout(
        WAIT,
        stream_client.server_stream(request(ServerStreamRequest {
            message: "drain-aware".to_owned(),
        })),
    )
    .await
    .expect("stream opens")
    .unwrap()
    .into_inner();
    assert_eq!(
        timeout(WAIT, stream.message())
            .await
            .expect("initial message")
            .unwrap()
            .unwrap()
            .message,
        "drain-aware"
    );

    let mut unary_client = stream_client.clone();
    let inflight_unary =
        tokio::spawn(async move { unary_client.unary(request(unary("hold"))).await });
    fixture.echo.hold.wait_for_entries(1).await;

    let server = fixture.take_server();
    let drain_started = tokio::time::Instant::now();
    let drain = tokio::spawn(async move { server.drain(Duration::from_secs(5)).await });

    assert!(
        timeout(Duration::from_secs(1), stream.message())
            .await
            .expect("open stream closes promptly when listener drain starts")
            .unwrap()
            .is_none()
    );
    drop(stream);
    drop(stream_client);

    fixture.echo.hold.release();
    let unary_reply = timeout(WAIT, inflight_unary)
        .await
        .expect("in-flight unary finishes during drain")
        .expect("unary task joins")
        .expect("in-flight unary succeeds");
    assert_eq!(unary_reply.into_inner().message, "released");

    let drained = timeout(Duration::from_secs(2), drain)
        .await
        .expect("listener drain completes well within 5s budget")
        .expect("drain task joins")
        .expect("drain succeeds");
    assert_eq!(drained, Drained::Complete);
    assert!(drain_started.elapsed() < Duration::from_secs(2));
    fixture.stop().await;
}

#[tokio::test]
async fn client_bidi_abort_mid_upload_surfaces_stream_error_not_eof() {
    for use_timeout in [false, true] {
        let fixture = Fixture::plaintext().await;
        let client_budget = if use_timeout {
            Duration::from_millis(150)
        } else {
            CLIENT_TIMEOUT
        };
        let adapter = Client::builder(
            &format!("http://{}", fixture.address),
            ClientSecurity::Plaintext,
            client_budget,
        )
        .build()
        .unwrap();
        let mut client = EchoServiceClient::new(adapter);
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let upload =
            futures_util::stream::unfold(rx, |mut rx| async { rx.recv().await.map(|v| (v, rx)) });
        let mut outgoing = request(upload);
        outgoing
            .metadata_mut()
            .insert("x-observe-upload-abort", "1".parse().unwrap());
        let cancel = tokio_util::sync::CancellationToken::new();
        if !use_timeout {
            outgoing
                .extensions_mut()
                .insert(operation_context::OperationContext::new(
                    None,
                    cancel.clone(),
                ));
        }
        let mut response = timeout(WAIT, client.bidi_stream(outgoing))
            .await
            .expect("bidi stream opens")
            .unwrap()
            .into_inner();
        timeout(
            WAIT,
            tx.send(BidiStreamRequest {
                message: "hello".to_owned(),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            timeout(WAIT, response.message())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .message,
            "hello"
        );
        if !use_timeout {
            cancel.cancel();
        }
        let err = timeout(WAIT, response.message())
            .await
            .expect("client stream aborts")
            .unwrap_err();
        assert_eq!(
            err.code(),
            if use_timeout {
                Code::DeadlineExceeded
            } else {
                Code::Cancelled
            }
        );
        drop(response);
        timeout(WAIT, fixture.echo.upload_abort_recorded.notified())
            .await
            .expect("server handler observes upload termination");
        let observed = fixture
            .echo
            .upload_aborts
            .lock()
            .pop()
            .expect("upload outcome recorded");
        assert!(
            observed.is_err(),
            "mid-upload abort must surface Err from RST_STREAM, got {observed:?} (use_timeout={use_timeout})"
        );
        drop(tx);
        drop(client);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn tls13_with_a_trusted_ca_succeeds_and_tls12_is_refused() {
    let pki = Pki::new(&["127.0.0.1", "localhost"]);
    let fixture = Fixture::tls(&pki, false).await;
    let ca = pem(&pki.ca_certificate);
    let mut client = EchoServiceClient::new(tls_client(
        fixture.address,
        ClientTlsMaterial {
            ca_certificate_pem: Some(&ca),
            identity: None,
        },
    ));
    let response = timeout(WAIT, client.unary(request(unary("tls13"))))
        .await
        .expect("tls unary")
        .unwrap();
    assert_eq!(response.into_inner().message, "tls13");
    assert_tls_denied(fixture.address, &pki, None, true).await;
    drop(client);
    fixture.stop().await;
}

#[tokio::test]
async fn a_new_dial_uses_the_replaced_localhost_address_without_changing_tls_identity() {
    let pki = Pki::new(&["localhost"]);
    let first = Fixture::tls_at(&pki, "127.0.0.1:0".parse().unwrap()).await;
    let port = first.address.port();
    let ca = pem(&pki.ca_certificate);
    let client = Client::builder(
        &format!("https://localhost:{port}"),
        ClientSecurity::Tls(ClientTlsMaterial {
            ca_certificate_pem: Some(&ca),
            identity: None,
        }),
        CLIENT_TIMEOUT,
    )
    .build()
    .unwrap();
    let mut client = EchoServiceClient::new(client);
    let response = timeout(WAIT, client.unary(request(unary("ipv4"))))
        .await
        .expect("first localhost dial")
        .unwrap();
    assert_eq!(response.into_inner().message, "ipv4");
    assert_seen(&first.echo, &[Seen::Unary]);
    first.stop().await;

    let replacement =
        Fixture::tls_at(&pki, format!("[::1]:{port}").parse::<SocketAddr>().unwrap()).await;
    let response = timeout(WAIT, client.unary(request(unary("ipv6"))))
        .await
        .expect("same client opens a new localhost dial")
        .unwrap();
    assert_eq!(response.into_inner().message, "ipv6");
    assert_seen(&replacement.echo, &[Seen::Unary]);
    replacement.stop().await;
}

#[tokio::test]
async fn rebuilding_a_client_is_required_to_adopt_replacement_tls_material() {
    let stale = Pki::new(&["localhost"]);
    let replacement = Pki::new(&["localhost"]);
    let fixture = Fixture::tls_at(&replacement, "127.0.0.1:0".parse().unwrap()).await;
    let destination = format!("https://localhost:{}", fixture.address.port());
    let stale_ca = pem(&stale.ca_certificate);
    let mut stale_client = EchoServiceClient::new(
        Client::builder(
            &destination,
            ClientSecurity::Tls(ClientTlsMaterial {
                ca_certificate_pem: Some(&stale_ca),
                identity: None,
            }),
            CLIENT_TIMEOUT,
        )
        .build()
        .unwrap(),
    );
    let status = timeout(WAIT, stale_client.unary(request(unary("stale"))))
        .await
        .expect("stale-root client is bounded")
        .unwrap_err();
    assert_eq!(status.code(), Code::Unavailable);

    let replacement_ca = pem(&replacement.ca_certificate);
    let mut rebuilt_client = EchoServiceClient::new(
        Client::builder(
            &destination,
            ClientSecurity::Tls(ClientTlsMaterial {
                ca_certificate_pem: Some(&replacement_ca),
                identity: None,
            }),
            CLIENT_TIMEOUT,
        )
        .build()
        .unwrap(),
    );
    let response = timeout(WAIT, rebuilt_client.unary(request(unary("replacement"))))
        .await
        .expect("rebuilt client reaches the replacement TLS material")
        .unwrap();
    assert_eq!(response.into_inner().message, "replacement");
    assert_seen(&fixture.echo, &[Seen::Unary]);
    fixture.stop().await;
}

#[tokio::test]
async fn mtls_refuses_a_client_without_a_trusted_certificate() {
    let pki = Pki::new(&["127.0.0.1", "localhost"]);
    let fixture = Fixture::tls(&pki, true).await;
    assert_tls_denied(fixture.address, &pki, None, false).await;
    let wrong = Pki::new(&["127.0.0.1"]);
    assert_tls_denied(
        fixture.address,
        &pki,
        Some((&wrong.client_certificate, &wrong.client_key)),
        false,
    )
    .await;
    assert_tls_denied(fixture.address, &pki, None, true).await;

    let ca = pem(&pki.ca_certificate);
    let client_cert = pem(&pki.client_certificate);
    let client_key = secrecy::SecretString::from(pem(&pki.client_key));
    let mut client = EchoServiceClient::new(tls_client(
        fixture.address,
        ClientTlsMaterial {
            ca_certificate_pem: Some(&ca),
            identity: Some(ClientIdentity {
                certificate_pem: &client_cert,
                private_key_pem: &client_key,
            }),
        },
    ));
    let response = timeout(WAIT, client.unary(request(unary("mtls"))))
        .await
        .expect("mtls unary")
        .unwrap();
    assert_eq!(response.into_inner().message, "mtls");
    drop(client);
    fixture.stop().await;
}

#[test]
fn server_tls_refuses_a_chain_entry_that_is_not_a_certificate() {
    let pki = Pki::new(&["localhost"]);
    let key = secrecy::SecretString::from(pem(&pki.server_key));
    let garbage =
        "-----BEGIN CERTIFICATE-----\nbm90IGEgY2VydGlmaWNhdGU=\n-----END CERTIFICATE-----\n";
    let leaf = pem(&pki.server_certificate);
    for chain in [format!("{leaf}{garbage}"), format!("{garbage}{leaf}")] {
        assert_eq!(
            server_tls_config(ServerTlsMaterial {
                certificate_pem: &chain,
                private_key_pem: &key,
                client_ca_pem: None,
            })
            .unwrap_err(),
            TlsMaterialError::InvalidCertificate
        );
    }
}

#[tokio::test]
async fn both_reflection_versions_list_registered_services_and_close_idle_streams_on_drain() {
    use prost::Message as _;

    const UNREGISTERED: &str = "example.v1.Unregistered";
    let mut contract =
        prost_types::FileDescriptorSet::decode(grpc_contracts::FILE_DESCRIPTOR_SET).unwrap();
    let file = contract
        .file
        .iter_mut()
        .find(|file| file.package() == "example.v1")
        .unwrap();
    let mut unregistered = file
        .service
        .iter()
        .find(|service| service.name() == "EchoService")
        .unwrap()
        .clone();
    unregistered.name = Some("Unregistered".to_owned());
    file.service.push(unregistered);
    let fixture = Fixture::with_descriptor_set(&contract, None).await;
    let expected_names = [
        ECHO_SERVICE,
        "grpc.health.v1.Health",
        "grpc.reflection.v1.ServerReflection",
        "grpc.reflection.v1alpha.ServerReflection",
    ];
    let mut client = ServerReflectionClient::new(plaintext_client(fixture.address));
    let ask = |message_request| ServerReflectionRequest {
        host: String::new(),
        message_request: Some(message_request),
    };
    let asks = tonic::codegen::tokio_stream::iter([
        ask(MessageRequest::ListServices(String::new())),
        ask(MessageRequest::FileContainingSymbol(
            ECHO_SERVICE.to_owned(),
        )),
        ask(MessageRequest::FileContainingSymbol(
            UNREGISTERED.to_owned(),
        )),
    ])
    .chain(futures_util::stream::pending());
    let mut answers = client
        .server_reflection_info(request(asks))
        .await
        .expect("reflection opens")
        .into_inner();
    let mut next = async || {
        timeout(WAIT, answers.message())
            .await
            .expect("reflection answers in time")
            .expect("reflection stream stays open")
            .and_then(|answer| answer.message_response)
            .expect("reflection answers each request")
    };
    let MessageResponse::ListServicesResponse(listed) = next().await else {
        panic!("list services is answered with a service list");
    };
    let mut names: Vec<&str> = listed.service.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, expected_names);

    for _ in 0..2 {
        let MessageResponse::FileDescriptorResponse(files) = next().await else {
            panic!("a described symbol is answered with its file descriptor");
        };
        assert_matches!(files.file_descriptor_proto.as_slice(), [_, ..]);
    }

    let mut older = v1alpha::server_reflection_client::ServerReflectionClient::new(
        plaintext_client(fixture.address),
    );
    let asks = tonic::codegen::tokio_stream::iter([v1alpha::ServerReflectionRequest {
        host: String::new(),
        message_request: Some(
            v1alpha::server_reflection_request::MessageRequest::ListServices(String::new()),
        ),
    }])
    .chain(futures_util::stream::pending());
    let mut older_answers = older
        .server_reflection_info(request(asks))
        .await
        .expect("v1alpha reflection opens")
        .into_inner();
    let answer = timeout(WAIT, older_answers.message())
        .await
        .expect("v1alpha reflection answers in time")
        .expect("v1alpha reflection stream stays open")
        .and_then(|answer| answer.message_response);
    let Some(v1alpha::server_reflection_response::MessageResponse::ListServicesResponse(listed)) =
        answer
    else {
        panic!("list services is answered with a service list");
    };
    let mut names: Vec<&str> = listed.service.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, expected_names);
    let ((), latest, alpha) = timeout(WAIT, async {
        tokio::join!(fixture.stop(), answers.message(), older_answers.message())
    })
    .await
    .expect("reflection streams and the listener finish while both uploads remain open");
    assert!(latest.unwrap().is_none());
    assert!(alpha.unwrap().is_none());
}

#[tokio::test]
async fn a_generated_method_missing_from_the_descriptor_cannot_dispatch() {
    use prost::Message as _;

    let mut contract =
        prost_types::FileDescriptorSet::decode(grpc_contracts::FILE_DESCRIPTOR_SET).unwrap();
    let echo = contract
        .file
        .iter_mut()
        .find(|file| file.package() == "example.v1")
        .unwrap()
        .service
        .iter_mut()
        .find(|service| service.name() == "EchoService")
        .unwrap();
    let original_count = echo.method.len();
    echo.method.retain(|method| method.name() != "ServerStream");
    assert_eq!(echo.method.len() + 1, original_count);
    let fixture = Fixture::with_descriptor_set(&contract, Some("ServerStream")).await;
    let mut client = fixture.echo_client();

    let omitted = timeout(
        WAIT,
        client.server_stream(request(ServerStreamRequest {
            message: "must not reach the handler".to_owned(),
        })),
    )
    .await
    .expect("the omitted method answers")
    .unwrap_err();
    assert_eq!(omitted.code(), Code::Unimplemented);
    assert!(fixture.echo.calls.lock().is_empty());

    let included = timeout(WAIT, client.unary(request(unary("described"))))
        .await
        .expect("the described method answers")
        .unwrap()
        .into_inner();
    assert_eq!(included.message, "described");
    assert_seen(&fixture.echo, &[Seen::Unary]);
    fixture.stop().await;
}

#[test]
fn bytes_that_are_not_a_descriptor_set_are_rejected() {
    let error = Services::new()
        .describe(b"\xffprivate descriptor fixture")
        .unwrap_err();
    assert_matches!(&error, Error::DecodeDescriptorSet(_));
    let source = std::error::Error::source(&error).unwrap();
    assert!(source.is::<prost::DecodeError>());
    assert!(
        error
            .to_string()
            .contains("decode gRPC file descriptor set")
    );
    assert!(!format!("{error:?} {error} {source}").contains("private descriptor fixture"));
}

#[test]
fn conflicting_descriptors_for_the_same_service_are_rejected() {
    use prost::Message as _;

    fn encode(set: &prost_types::FileDescriptorSet) -> &'static [u8] {
        Box::leak(set.encode_to_vec().into_boxed_slice())
    }

    let mut services = described();
    let mut descriptor =
        prost_types::FileDescriptorSet::decode(grpc_contracts::FILE_DESCRIPTOR_SET).unwrap();
    let echo = descriptor
        .file
        .iter_mut()
        .flat_map(|file| &mut file.service)
        .find(|service| service.name() == "EchoService")
        .unwrap();
    echo.method.retain(|method| method.name() != "Unary");
    let unnamed_file = prost_types::FileDescriptorProto {
        service: vec![prost_types::ServiceDescriptorProto {
            method: vec![prost_types::MethodDescriptorProto::default()],
            ..Default::default()
        }],
        ..Default::default()
    };
    descriptor.file.insert(0, unnamed_file);
    assert_matches!(
        services.describe(encode(&descriptor)),
        Err(Error::ConflictingServiceDescription(name)) if name == "example.v1.EchoService"
    );
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .expect("the rejected set did not replace the original description");
    descriptor.file.truncate(1);
    descriptor.file[0].service[0]
        .method
        .push(prost_types::MethodDescriptorProto {
            name: Some("Ping".to_owned()),
            ..Default::default()
        });
    services
        .describe(encode(&descriptor))
        .expect("the later conflict did not register an earlier service");
    let file = &mut descriptor.file[0];
    file.package = Some(String::new());
    let service = &mut file.service[0];
    service.name = Some(String::new());
    service.method.remove(0);
    assert_matches!(
        services.describe(encode(&descriptor)),
        Err(Error::ConflictingServiceDescription(name)) if name.is_empty()
    );
    descriptor.file[0].service[0]
        .method
        .push(prost_types::MethodDescriptorProto {
            name: Some(String::new()),
            ..Default::default()
        });
    services
        .describe(encode(&descriptor))
        .expect("explicit default names and reordered methods describe the same service");
}

#[test]
fn reflection_added_twice_is_rejected() {
    let mut services = Services::new();
    services.add_reflection().expect("first registration");
    assert_matches!(
        services.add_reflection(),
        Err(Error::DuplicateService(
            "grpc.reflection.v1.ServerReflection"
        ))
    );
}

#[test]
fn a_service_is_described_before_it_is_added_and_added_once() {
    assert_matches!(
        Services::new().add(EchoServiceServer::new(Echo::new())),
        Err(Error::UndescribedService("example.v1.EchoService"))
    );
    let mut services = described();
    services
        .add(EchoServiceServer::new(Echo::new()))
        .expect("first registration");
    assert_matches!(
        services.add(EchoServiceServer::new(Echo::new())),
        Err(Error::DuplicateService("example.v1.EchoService"))
    );
}

#[tokio::test]
async fn calls_are_counted_by_described_method_whatever_ended_them() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.echo_client();

    timeout(WAIT, client.unary(request(unary("answered"))))
        .await
        .expect("answered call")
        .unwrap();

    let classified = timeout(WAIT, client.unary(request(unary("classified"))))
        .await
        .expect("classified call")
        .unwrap_err();
    assert_eq!(classified.code(), Code::InvalidArgument);

    let abandoned = tokio::spawn({
        let mut client = client.clone();
        async move { client.unary(request(unary("hold"))).await }
    });
    fixture.echo.hold.wait_for_entries(1).await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());

    assert_eq!(raw_call(fixture.address, UNARY_PATH).await, "4");
    assert_eq!(
        raw_call(fixture.address, "/example.v1.EchoService/Invented").await,
        "12"
    );

    let unary = r#"grpc_type="unary",grpc_service="example.v1.EchoService",grpc_method="Unary""#;
    let unknown = r#"grpc_type="unknown",grpc_service="unknown",grpc_method="unknown""#;
    let expected = [
        format!("grpc_server_started_total{{{unary}}} 4"),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="OK"}} 1"#),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="InvalidArgument"}} 1"#),
        format!(r#"grpc_server_failures_total{{{unary},failure_code="bad_request"}} 1"#),
        format!(r#"grpc_server_failures_total{{{unary},failure_code="gateway_timeout"}} 1"#),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="Canceled"}} 1"#),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="DeadlineExceeded"}} 1"#),
        format!("grpc_server_started_total{{{unknown}}} 1"),
        format!(r#"grpc_server_handled_total{{{unknown},grpc_code="Unimplemented"}} 1"#),
    ];
    let rendered = timeout(WAIT, async {
        loop {
            let rendered = handle.render();
            if expected.iter().all(|line| rendered.contains(line)) {
                return rendered;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("metrics settle: {}", handle.render()));
    assert!(!rendered.contains("Invented"), "{rendered}");
    assert_eq!(
        rendered.matches("grpc_server_failures_total{").count(),
        2,
        "{rendered}"
    );
    fixture.stop().await;
}

#[tokio::test]
async fn a_stream_is_counted_with_the_status_that_ended_it() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.echo_client();

    for (message, code, detail) in [
        ("fail-after-first", Code::Aborted, "request conflict"),
        ("panic-after-first", Code::Internal, "request failed"),
    ] {
        let mut stream = timeout(
            WAIT,
            client.server_stream(request(ServerStreamRequest {
                message: message.to_owned(),
            })),
        )
        .await
        .expect("the stream opens")
        .unwrap()
        .into_inner();
        let ended = loop {
            match timeout(WAIT, stream.message())
                .await
                .expect("the stream ends")
            {
                Ok(Some(item)) => assert_eq!(item.message, message),
                Ok(None) => panic!("{message} ends without its failure"),
                Err(status) => break status,
            }
        };
        assert_eq!(ended.code(), code, "{message}");
        assert_eq!(ended.message(), detail, "{message}");
    }

    let unread = timeout(
        WAIT,
        client.server_stream(request(ServerStreamRequest {
            message: "hold-after-first".to_owned(),
        })),
    )
    .await
    .expect("the unread stream opens")
    .unwrap();
    drop(unread);

    let followed = timeout(WAIT, client.unary(request(unary("after"))))
        .await
        .expect("call after the streams")
        .unwrap();
    assert_eq!(followed.into_inner().message, "after");

    let stream = r#"grpc_type="server_stream",grpc_service="example.v1.EchoService",grpc_method="ServerStream""#;
    let expected = [
        format!("grpc_server_started_total{{{stream}}} 3"),
        format!(r#"grpc_server_handled_total{{{stream},grpc_code="Aborted"}} 1"#),
        format!(r#"grpc_server_handled_total{{{stream},grpc_code="Internal"}} 1"#),
        format!(r#"grpc_server_handled_total{{{stream},grpc_code="Canceled"}} 1"#),
        format!(r#"grpc_server_failures_total{{{stream},failure_code="conflict"}} 1"#),
        format!(r#"grpc_server_failures_total{{{stream},failure_code="internal_error"}} 1"#),
    ];
    timeout(WAIT, async {
        while !expected.iter().all(|line| handle.render().contains(line)) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("metrics settle: {}", handle.render()));
    drop(client);
    fixture.stop().await;
}

fn stalled_unary_response(
    send_data: bool,
    peer_polled: Arc<Notify>,
    dropped: impl Send + 'static,
) -> http::Response<axum::body::Body> {
    let first = send_data
        .then(|| Ok::<_, std::convert::Infallible>(http_body::Frame::data(grpc_frame("reply"))));
    let frames = futures_util::stream::iter(first).chain(futures_util::stream::once(async move {
        let _dropped = dropped;
        peer_polled.notify_one();
        std::future::pending().await
    }));
    http::Response::builder()
        .header("content-type", "application/grpc")
        .body(axum::body::Body::new(http_body_util::StreamBody::new(
            frames,
        )))
        .unwrap()
}

#[tokio::test]
async fn client_budget_includes_unary_data_and_trailers_after_peer_headers() {
    for send_data in [false, true] {
        let peer_polled = Arc::new(Notify::new());
        let body_dropped = Arc::new(Notify::new());
        let handler_1 = {
            let peer_polled = Arc::clone(&peer_polled);
            let body_dropped = Arc::clone(&body_dropped);
            move || {
                let peer_polled = Arc::clone(&peer_polled);
                let dropped = notify_on_drop(Arc::clone(&body_dropped));
                async move { stalled_unary_response(send_data, peer_polled, dropped) }
            }
        };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let app = axum::Router::new().route(UNARY_PATH, axum::routing::post(handler_1));
        let server = BoundListener::bind(loopback()).unwrap().serve_h2(
            app,
            server_options(connection_limits()),
            None,
        );
        let adapter = Client::builder(
            &format!("http://{}", server.local_addr()),
            ClientSecurity::Plaintext,
            Duration::from_millis(200),
        )
        .build()
        .unwrap();
        let call = tokio::spawn(async move {
            EchoServiceClient::new(adapter)
                .unary(Request::new(unary("request")))
                .await
        });
        timeout(WAIT, peer_polled.notified())
            .await
            .expect("peer has opened and stalled its body");
        let status = timeout(WAIT, call).await.unwrap().unwrap().unwrap_err();
        assert_eq!(
            status.code(),
            Code::DeadlineExceeded,
            "send_data={send_data}"
        );
        timeout(WAIT, body_dropped.notified())
            .await
            .expect("peer sees cancellation");
        assert_eq!(
            timeout(WAIT, server.drain(Duration::from_secs(1)))
                .await
                .unwrap()
                .unwrap(),
            Drained::Complete
        );
    }
}

#[tokio::test]
async fn a_call_whose_deadline_runs_out_is_deadline_exceeded_not_unavailable() {
    let listener = tokio::net::TcpListener::bind(loopback())
        .await
        .expect("silent peer binds");
    let address = listener.local_addr().expect("silent peer address");
    let silent = tokio::spawn(async move {
        let _connection = listener.accept().await;
        std::future::pending::<()>().await;
    });
    let mut client = EchoServiceClient::new(plaintext_client(address));
    let mut call = Request::new(unary("unanswered"));
    call.set_timeout(Duration::from_millis(100));
    let status = timeout(WAIT, client.unary(call))
        .await
        .expect("the deadline ends the call")
        .unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(status.message(), "request deadline exceeded");

    let bounded = Client::builder(
        &format!("http://{address}"),
        ClientSecurity::Plaintext,
        Duration::from_millis(100),
    )
    .build()
    .expect("bounded client");
    let status = timeout(
        WAIT,
        EchoServiceClient::new(bounded).unary(Request::new(unary("unanswered"))),
    )
    .await
    .expect("the client timeout ends the call")
    .unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    silent.abort();
    assert!(silent.await.unwrap_err().is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn stalled_tls_dial_is_released_and_the_same_client_recovers() {
    tokio::time::resume();
    for shorter_caller in [false, true] {
        let pki = Pki::new(&["127.0.0.1"]);
        let healthy = Fixture::tls(&pki, false).await;
        let listener = tokio::net::TcpListener::bind(loopback()).await.unwrap();
        let ca = pem(&pki.ca_certificate);
        let adapter = tls_client(
            listener.local_addr().unwrap(),
            ClientTlsMaterial {
                ca_certificate_pem: Some(&ca),
                identity: None,
            },
        );
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "constructing the shared client must not connect"
        );
        let mut call = tokio::spawn(async move {
            let mut client = EchoServiceClient::new(adapter);
            let mut message = request(unary("stalled"));
            if shorter_caller {
                message.set_timeout(Duration::from_secs(1));
            }
            let result = client.unary(message).await;
            (client, result)
        });
        let (mut stalled, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        let mut handshake = [0; 4096];
        assert!(
            timeout(WAIT, stalled.read(&mut handshake))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(4)).await;
        tokio::time::resume();
        if shorter_caller {
            let (mut client, result) = timeout(WAIT, &mut call).await.unwrap().unwrap();
            assert_eq!(result.unwrap_err().code(), Code::DeadlineExceeded);
            call = tokio::spawn(async move {
                let result = client.unary(request(unary("still stalled"))).await;
                (client, result)
            });
        } else {
            assert!(!call.is_finished(), "the full RPC budget still has time");
        }
        loop {
            match stalled.try_read(&mut handshake) {
                Ok(0) => panic!("the dial ended before its ceiling"),
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("stalled TLS socket: {error}"),
            }
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::time::resume();
        timeout(Duration::from_secs(1), stalled.read_to_end(&mut Vec::new()))
            .await
            .expect("active readiness polling releases the expired dial's peer socket")
            .unwrap();
        let (mut client, result) = timeout(WAIT, call).await.unwrap().unwrap();
        assert_eq!(result.unwrap_err().code(), Code::Unavailable);
        let address = healthy.address;
        let relay = tokio::spawn(async move {
            let (mut incoming, _) = timeout(WAIT, listener.accept()).await.unwrap().unwrap();
            let mut outgoing = TcpStream::connect(address).await.unwrap();
            tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await
        });
        let response = timeout(WAIT, client.unary(request(unary("recovered"))))
            .await
            .expect("the same client redials after the stalled attempt")
            .unwrap();
        assert_eq!(response.into_inner().message, "recovered");
        assert_seen(&healthy.echo, &[Seen::Unary]);
        drop(client);
        relay.abort();
        if let Err(error) = relay.await {
            assert!(error.is_cancelled());
        }
        healthy.stop().await;
    }
}

fn pem(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("rcgen emits ASCII PEM")
}

fn tls_client(address: SocketAddr, material: ClientTlsMaterial<'_>) -> Client {
    Client::builder(
        &format!("https://127.0.0.1:{}", address.port()),
        ClientSecurity::Tls(material),
        CLIENT_TIMEOUT,
    )
    .build()
    .expect("tls client")
}

// template:begin oidc-introspection:grpc-transport-test-auth-provider
struct ProviderFixture {
    silent: Arc<AtomicBool>,
    gate: Arc<Mutex<Option<Arc<Hold>>>>,
    calls: Arc<AtomicUsize>,
    cancel: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ProviderFixture {
    async fn stop(self) {
        self.cancel.cancel();
        timeout(WAIT, self.task)
            .await
            .expect("provider stops")
            .expect("provider joins");
    }
}

async fn verifier_fixture() -> (Verifier, ProviderFixture) {
    let pki = Pki::new(&["provider.test"]);
    let listener = TcpListener::bind(loopback()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut tls = provider_tls_config(&pki);
    tls.alpn_protocols.clear();
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let silent = Arc::new(AtomicBool::new(false));
    let gate: Arc<Mutex<Option<Arc<Hold>>>> = Arc::default();
    let calls = Arc::new(AtomicUsize::new(0));
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn({
        let silent = Arc::clone(&silent);
        let gate = Arc::clone(&gate);
        let calls = Arc::clone(&calls);
        let cancel = cancel.clone();
        async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let accepted = tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    joined = connections.join_next(), if !connections.is_empty() => {
                        joined.unwrap().expect("provider connection joins");
                        continue;
                    }
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let silent = Arc::clone(&silent);
                let gate = Arc::clone(&gate);
                let calls = Arc::clone(&calls);
                connections.spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    answer_introspection(stream, &silent, &gate, &calls).await;
                });
            }
            connections.shutdown().await;
        }
    });
    let fixture =
        FixtureTransport::try_new("provider.test", address, &pki.ca_der, cancel.child_token())
            .unwrap();
    let verifier = prepare_introspection_with_fixture(
        IntrospectionOptions {
            issuer: IssuerUrl::parse("https://issuer.example").unwrap(),
            audiences: vec!["api".to_owned()],
            endpoint: EndpointUrl::parse(&format!(
                "https://provider.test:{}/introspect",
                address.port()
            ))
            .unwrap(),
            client_id: "fixture".to_owned(),
            client_secret: SecretString::from("fixture"),
            provider_concurrency: const { std::num::NonZeroUsize::new(32).expect("32 is nonzero") },
            cache: Some(IntrospectionCacheOptions::try_new(16, Duration::from_mins(1)).unwrap()),
        },
        fixture,
    )
    .unwrap();
    (
        verifier,
        ProviderFixture {
            silent,
            gate,
            calls,
            cancel,
            task,
        },
    )
}

async fn answer_introspection(
    stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    silent: &AtomicBool,
    gate: &Mutex<Option<Arc<Hold>>>,
    calls: &AtomicUsize,
) {
    use http_body_util::BodyExt as _;

    let io = hyper_util::rt::TokioIo::new(stream);
    let service =
        hyper::service::service_fn(|request: http::Request<hyper::body::Incoming>| async move {
            let body = request.into_body().collect().await?.to_bytes();
            let unscoped = url::form_urlencoded::parse(&body)
                .any(|(name, value)| name == "token" && value == UNSCOPED);
            calls.fetch_add(1, Ordering::Release);
            let held = gate.lock().clone();
            if let Some(held) = held {
                held.enter_and_wait_for_release().await;
            }
            if silent.load(Ordering::Acquire) {
                std::future::pending::<()>().await;
            }
            Ok::<_, hyper::Error>(introspection_response(unscoped))
        });
    let _ = hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .half_close(true)
        .header_read_timeout(None)
        .serve_connection(io, service)
        .await;
}

fn introspection_response(unscoped: bool) -> http::Response<http_body_util::Full<bytes::Bytes>> {
    let scope = if unscoped { "" } else { ORDERS_READ };
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    let body = format!(
        r#"{{"active":true,"iss":"https://issuer.example","aud":"api","exp":{expiry},"sub":"subject","scope":"{scope}"}}"#
    );
    http::Response::builder()
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(http_body_util::Full::new(body.into()))
        .unwrap()
}

async fn introspection_exchange(prefix: &str, suffix: &str) -> (usize, String) {
    let (mut client, server) = tokio::io::duplex(8192);
    let silent = AtomicBool::new(false);
    let gate = Mutex::new(None);
    let calls = AtomicUsize::new(0);
    let mut connection = std::pin::pin!(answer_introspection(server, &silent, &gate, &calls));
    client.write_all(prefix.as_bytes()).await.unwrap();
    if !suffix.is_empty() {
        assert!(futures_util::poll!(connection.as_mut()).is_pending());
        assert_eq!(calls.load(Ordering::Acquire), 0, "partial form counted");
        client.write_all(suffix.as_bytes()).await.unwrap();
    }
    client.shutdown().await.unwrap();
    timeout(WAIT, connection).await.expect("provider finished");
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    (calls.load(Ordering::Acquire), response)
}

#[tokio::test]
async fn introspection_classifies_the_complete_decoded_token() {
    for (suffix, scope) in [
        ("unscoped", ""),
        ("%75nscoped&other=1", ""),
        ("unscoped-longer", "orders.read"),
        ("accepted&other_token=unscoped", "orders.read"),
    ] {
        let length = "token=".len() + suffix.len();
        let prefix = format!(
            "POST / HTTP/1.1\r\nHost: provider.test\r\nContent-Length: {length}\r\n\r\ntoken="
        );
        let (calls, response) = introspection_exchange(&prefix, suffix).await;
        assert_eq!(calls, 1);
        assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
        assert!(
            response.contains(&format!(r#""scope":"{scope}""#)),
            "{response}"
        );
    }
    let (calls, response) = introspection_exchange(
        "POST / HTTP/1.1\r\nHost: provider.test\r\nContent-Length: 14\r\n\r\ntoken=",
        "",
    )
    .await;
    assert_eq!(calls, 0);
    assert!(!response.starts_with("HTTP/1.1 200 "));
}

fn provider_tls_config(pki: &Pki) -> rustls::ServerConfig {
    let certificates = rustls::pki_types::CertificateDer::pem_slice_iter(&pki.server_certificate)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(&pki.server_key).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certificates, key)
    .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}
// template:end oidc-introspection:grpc-transport-test-auth-provider

struct Pki {
    ca_certificate: Vec<u8>,
    // template:begin oidc-introspection:grpc-transport-test-auth-ca-der
    ca_der: Vec<u8>,
    // template:end oidc-introspection:grpc-transport-test-auth-ca-der
    server_certificate: Vec<u8>,
    server_key: Vec<u8>,
    client_certificate: Vec<u8>,
    client_key: Vec<u8>,
}

impl Pki {
    fn new(names: &[&str]) -> Self {
        let mut ca = CertificateParams::default();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
        let (server_certificate, server_key) =
            leaf(names, vec![ExtendedKeyUsagePurpose::ServerAuth], &issuer);
        let (client_certificate, client_key) = leaf(
            &["client"],
            vec![ExtendedKeyUsagePurpose::ClientAuth],
            &issuer,
        );
        Self {
            ca_certificate: issuer.pem().into_bytes(),
            // template:begin oidc-introspection:grpc-transport-test-auth-ca-der-init
            ca_der: issuer.der().to_vec(),
            // template:end oidc-introspection:grpc-transport-test-auth-ca-der-init
            server_certificate,
            server_key,
            client_certificate,
            client_key,
        }
    }
}

fn leaf(
    names: &[&str],
    usages: Vec<ExtendedKeyUsagePurpose>,
    issuer: &CertifiedIssuer<'_, KeyPair>,
) -> (Vec<u8>, Vec<u8>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    params.extended_key_usages = usages;
    let certificate = params.signed_by(&key, issuer).unwrap();
    (
        certificate.pem().into_bytes(),
        key.serialize_pem().into_bytes(),
    )
}

async fn assert_tls_denied(
    address: SocketAddr,
    trusted: &Pki,
    identity: Option<(&Vec<u8>, &Vec<u8>)>,
    tls12: bool,
) {
    let mut roots = RootCertStore::empty();
    for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(&trusted.ca_certificate) {
        roots.add(certificate.unwrap()).unwrap();
    }
    let versions: &[&rustls::SupportedProtocolVersion] = if tls12 {
        &[&rustls::version::TLS12]
    } else {
        &[&rustls::version::TLS13]
    };
    let builder =
        ClientConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_protocol_versions(versions)
            .unwrap()
            .with_root_certificates(roots);
    let config = match identity {
        Some((certificate, key)) => builder
            .with_client_auth_cert(
                rustls::pki_types::CertificateDer::pem_slice_iter(certificate)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap(),
                PrivateKeyDer::from_pem_slice(key).unwrap(),
            )
            .unwrap(),
        None => builder.with_no_client_auth(),
    };
    let stream = TcpStream::connect(address).await.unwrap();
    let result = timeout(
        WAIT,
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("127.0.0.1").unwrap(), stream),
    )
    .await;
    let denied = match result {
        Ok(Err(_)) => true,
        Ok(Ok(mut stream)) => {
            let mut byte = [0_u8; 1];
            matches!(
                timeout(WAIT, stream.read(&mut byte)).await,
                Ok(Err(_) | Ok(0))
            )
        }
        Err(_) => false,
    };
    assert!(
        denied,
        "server did not reject TLS peer (tls12={tls12}, identity={})",
        identity.is_some()
    );
}
