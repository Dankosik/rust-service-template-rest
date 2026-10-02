#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]

//! Loopback proof of the tonic router on the shared HTTP listener.

use std::{
    net::SocketAddr,
    num::NonZeroU32,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

// template:begin authn:grpc-transport-test-auth-time
use std::time::{SystemTime, UNIX_EPOCH};
// template:end authn:grpc-transport-test-auth-time

use futures_util::StreamExt as _;
use grpc_contracts::example::v1::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_client::EchoServiceClient,
    echo_service_server::{EchoService, EchoServiceServer},
};
use health::{Readiness, RefreshPolicy};
// template:begin authn:grpc-transport-test-auth-imports
use infra_bearerauthn::{
    EndpointUrl, IntrospectionCacheOptions, IntrospectionOptions, IssuerUrl, Verifier,
    test_support::{FixtureTransport, prepare_introspection_with_fixture},
};
// template:end authn:grpc-transport-test-auth-imports
use infra_grpc::{
    ClientIdentity, ClientSecurity, ClientTlsMaterial, ERROR_DOMAIN, Error, Failure, Limits,
    ServerTlsMaterial, Services, server_options, server_tls_config,
};
use infra_http::{Drained, Server};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{PrivateKeyDer, ServerName, pem::PemObject as _},
};
// template:begin authn:grpc-transport-test-auth-secret
use secrecy::SecretString;
// template:end authn:grpc-transport-test-auth-secret
use service_failure::Code as FailureCode;
use tokio::{io::AsyncReadExt as _, net::TcpStream, sync::Notify, time::timeout};
// template:begin authn:grpc-transport-test-auth-listener
use tokio::net::TcpListener;
// template:end authn:grpc-transport-test-auth-listener
// template:begin authn:grpc-transport-test-auth-io
use tokio::io::AsyncWriteExt as _;
// template:end authn:grpc-transport-test-auth-io
// template:begin authn:grpc-transport-test-auth-tls-acceptor
use tokio_rustls::TlsAcceptor;
// template:end authn:grpc-transport-test-auth-tls-acceptor
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

// template:begin authn:grpc-transport-test-accepted-token
const ACCEPTED: &str = "accepted";
/// A token the provider reports active but without any scope.
const UNSCOPED: &str = "unscoped";
const UNARY_PATH: &str = "/example.v1.EchoService/Unary";
// template:end authn:grpc-transport-test-accepted-token

/// The longest a test client waits for response headers.
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
    // template:end authn:grpc-transport-test-seen-identity-field
    had_authorization: bool,
}

struct Hold {
    release: Notify,
    released: AtomicBool,
    entered: AtomicUsize,
    entered_notify: Notify,
}

impl Hold {
    fn new() -> Self {
        Self {
            release: Notify::new(),
            released: AtomicBool::new(false),
            entered: AtomicUsize::new(0),
            entered_notify: Notify::new(),
        }
    }

    async fn wait(&self) {
        let notified = self.release.notified();
        self.entered.fetch_add(1, Ordering::Release);
        self.entered_notify.notify_waiters();
        if !self.released.load(Ordering::Acquire) {
            notified.await;
        }
    }

    async fn wait_for(&self, expected: usize) {
        timeout(FILL, async {
            loop {
                let notified = self.entered_notify.notified();
                if self.entered.load(Ordering::Acquire) >= expected {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("handlers enter");
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.release.notify_waiters();
    }
}

#[derive(Clone)]
struct Echo {
    calls: Arc<Mutex<Vec<SeenCall>>>,
    hold: Arc<Hold>,
}

impl Echo {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            hold: Arc::new(Hold::new()),
        }
    }

    fn observe<T>(&self, request: &Request<T>, seen: Seen) {
        self.calls.lock().expect("observations").push(SeenCall {
            cardinality: seen,
            // template:begin authn:grpc-transport-test-seen-principal
            had_identity: request
                .extensions()
                .get::<infra_bearerauthn::Principal>()
                .is_some(),
            // template:end authn:grpc-transport-test-seen-principal
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
        match request.into_inner().message.as_str() {
            "panic" => panic!("panic detail: do not leak"),
            "classified" => Err(Failure::new(FailureCode::BadRequest)
                .field_violation("message", "is reserved")
                .into()),
            "hold" => {
                self.hold.wait().await;
                Ok(Response::new(UnaryResponse {
                    message: "released".to_owned(),
                }))
            }
            "deadline" => {
                self.hold.wait().await;
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
        let message = request.into_inner().message;
        let first = futures_util::stream::iter([Ok(ServerStreamResponse {
            message: message.clone(),
        })]);
        Ok(Response::new(match message.as_str() {
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
        let mut input = request.into_inner();
        let message = input
            .message()
            .await?
            .expect("client sends one message")
            .message;
        Ok(Response::new(Box::pin(tonic::codegen::tokio_stream::iter(
            [Ok(BidiStreamResponse { message })],
        ))))
    }
}

struct Fixture {
    server: Option<Server>,
    readiness: Readiness,
    echo: Echo,
    address: SocketAddr,
    // template:begin authn:grpc-transport-test-auth-provider-field
    provider: ProviderFixture,
    // template:end authn:grpc-transport-test-auth-provider-field
}

impl Fixture {
    async fn plaintext() -> Self {
        Self::open(true, None).await
    }

    async fn unseeded() -> Self {
        Self::open(false, None).await
    }

    /// A TLS listener for `pki`; `mtls` requires a client certificate.
    async fn tls(pki: &Pki, mtls: bool) -> Self {
        Self::open(true, Some((pki, mtls))).await
    }

    /// A plaintext listener that sheds above `max_in_flight` business calls.
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
        // template:begin authn:grpc-transport-test-auth-fixture
        let (verifier, provider) = verifier_fixture().await;
        // template:end authn:grpc-transport-test-auth-fixture
        let readiness = Readiness::new(
            Vec::new(),
            RefreshPolicy {
                interval: Duration::from_secs(60),
                probe_budget: Duration::from_secs(1),
                failure_threshold: 1,
            },
        );
        if seed {
            seed_ready(&readiness).await;
        }
        let echo = Echo::new();
        let mut services = Services::new();
        // Before the contract is described: reflection serves it all the same.
        services
            .add_reflection()
            .expect("reflection registers once");
        services
            .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
            .expect("the committed contract decodes");
        services
            .add(EchoServiceServer::new(echo.clone()))
            .expect("echo registers once");
        // template:begin authn:grpc-transport-test-scope-requirement
        services
            .require_scopes(UNARY_PATH, &["echo.read"])
            .expect("unary scopes register once");
        // template:end authn:grpc-transport-test-scope-requirement
        let app = infra_grpc::router(
            services,
            readiness.reader(),
            // template:begin authn:grpc-transport-test-router-verifier
            verifier,
            // template:end authn:grpc-transport-test-router-verifier
            limits,
        )
        .expect("router builds");
        let bound = match tls {
            Some((pki, mtls)) => {
                let certificate = pem(&pki.server_certificate);
                let key = secrecy::SecretString::from(pem(&pki.server_key));
                let ca = pem(&pki.ca_certificate);
                let config = server_tls_config(ServerTlsMaterial {
                    certificate_pem: &certificate,
                    private_key_pem: &key,
                    client_ca_pem: mtls.then_some(ca.as_str()),
                })
                .expect("server TLS config");
                Server::bind_tls(loopback(), app, server_options(limits), config)
                    .await
                    .expect("tls listener binds")
            }
            None => Server::bind(loopback(), app, server_options(limits))
                .await
                .expect("listener binds"),
        };
        let address = bound.local_addr();
        Self {
            server: Some(bound),
            readiness,
            echo,
            address,
            // template:begin authn:grpc-transport-test-auth-provider-init
            provider,
            // template:end authn:grpc-transport-test-auth-provider-init
        }
    }

    fn echo_client(&self) -> EchoServiceClient<infra_grpc::Client> {
        EchoServiceClient::new(plaintext_client(self.address))
    }

    fn health(&self) -> HealthClient<infra_grpc::Client> {
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
        self.provider.stop().await;
        // template:end authn:grpc-transport-test-auth-provider-stop
    }
}

/// The configuration defaults, without a connection age.
fn limits() -> Limits {
    Limits {
        request_timeout: Duration::from_secs(8),
        max_in_flight: NonZeroU32::new(256),
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

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn plaintext_client(address: SocketAddr) -> infra_grpc::Client {
    infra_grpc::Client::new(
        &format!("http://{address}"),
        ClientSecurity::Plaintext,
        CLIENT_TIMEOUT,
    )
    .expect("plaintext client")
}

fn request<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    // template:begin authn:grpc-transport-test-bearer
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {ACCEPTED}").parse().unwrap(),
    );
    // template:end authn:grpc-transport-test-bearer
    request
}

fn watch_request(service: &str) -> Request<HealthCheckRequest> {
    let mut request = Request::new(HealthCheckRequest {
        service: service.to_owned(),
    });
    // template:begin authn:grpc-transport-test-watch-bearer
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {ACCEPTED}").parse().unwrap(),
    );
    // template:end authn:grpc-transport-test-watch-bearer
    request
}

async fn seed_ready(readiness: &Readiness) {
    readiness.refresh().await;
}

fn assert_seen(echo: &Echo, expected: &[Seen]) {
    let calls = echo.calls.lock().expect("observations");
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

    let unary = timeout(
        WAIT,
        client.unary(request(UnaryRequest {
            message: "one".to_owned(),
        })),
    )
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

/// The `google.rpc.ErrorInfo` reason of a catalog failure.
fn reason(status: &Status) -> String {
    let info = status.get_error_details().error_info().cloned();
    let info = info.expect("error info");
    assert_eq!(info.domain, ERROR_DOMAIN);
    info.reason
}

// template:begin authn:grpc-transport-test-unauthenticated
#[tokio::test]
async fn missing_and_malformed_bearers_are_unauthenticated_and_health_is_public() {
    let fixture = Fixture::plaintext().await;
    let mut echo = fixture.echo_client();
    let missing = timeout(
        WAIT,
        echo.unary(Request::new(UnaryRequest {
            message: "missing".to_owned(),
        })),
    )
    .await
    .expect("missing bearer")
    .unwrap_err();
    assert_eq!(missing.code(), Code::Unauthenticated);
    assert_eq!(missing.message(), "authentication failed");
    assert_eq!(reason(&missing), "AUTHENTICATION_REQUIRED");

    let mut malformed = Request::new(UnaryRequest {
        message: "malformed".to_owned(),
    });
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
    assert!(fixture.echo.calls.lock().expect("observations").is_empty());

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
        format!("Bearer {UNSCOPED}").parse().unwrap(),
    );
    request
}

#[tokio::test]
async fn a_principal_without_a_declared_scope_is_denied_before_the_handler() {
    let fixture = Fixture::plaintext().await;
    let mut echo = fixture.echo_client();

    let denied = timeout(
        WAIT,
        echo.unary(unscoped(UnaryRequest {
            message: "denied".to_owned(),
        })),
    )
    .await
    .expect("unscoped unary")
    .unwrap_err();
    assert_eq!(denied.code(), Code::PermissionDenied);
    assert_eq!(
        denied.message(),
        "the verified principal lacks the required scope"
    );
    assert_eq!(reason(&denied), "FORBIDDEN");
    assert!(fixture.echo.calls.lock().expect("observations").is_empty());

    // A method with no declared requirement admits any authenticated caller.
    let mut undeclared = timeout(
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
        timeout(WAIT, undeclared.message())
            .await
            .expect("server item")
            .unwrap()
            .unwrap()
            .message,
        "admitted"
    );
    drop(echo);
    fixture.stop().await;
}

#[test]
fn a_scope_requirement_needs_a_described_method_of_a_registered_service_and_is_declared_once() {
    let mut services = described();
    assert_eq!(
        services.require_scopes(UNARY_PATH, &["echo.read"]),
        Err(Error::UnregisteredMethodPath)
    );
    services
        .add(EchoServiceServer::new(Echo::new()))
        .expect("registration");
    for path in [
        "example.v1.EchoService/Unary",
        "/example.v1.EchoService",
        "/example.v1.EchoService/",
        "/example.v1.EchoService/Unary/extra",
        "/example.v1.EchoService/unary",
        "/example.v1.EchoService/Unry",
        "/example.v1.Other/Unary",
    ] {
        assert_eq!(
            services.require_scopes(path, &["echo.read"]),
            Err(Error::UnregisteredMethodPath),
            "{path}"
        );
    }
    services
        .require_scopes(UNARY_PATH, &["echo.read"])
        .expect("first requirement");
    assert_eq!(
        services.require_scopes(UNARY_PATH, &["echo.write"]),
        Err(Error::DuplicateScopeRequirement)
    );
}
// template:end authn:grpc-transport-test-insufficient-scope

// template:begin authn:grpc-transport-test-rejection-flood
/// Each rejected call is answered before its request DATA arrives. Without
/// reading that DATA first, h2 resets every such stream and closes the
/// connection after 1024 resets, failing the calls still on it.
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
                    .unary(Request::new(UnaryRequest {
                        message: "missing".to_owned(),
                    }))
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

// template:begin authn:grpc-transport-test-authentication-deadline
/// The provider never answers, so the call's 100 ms are spent before any
/// handler: the deadline is the caller's, whichever step uses it up.
#[tokio::test]
async fn the_deadline_bounds_authentication_too() {
    let fixture = Fixture::plaintext().await;
    fixture.provider.silent.store(true, Ordering::Release);
    let status = timeout(WAIT, raw_call(fixture.address, ECHO_SERVICE_UNARY))
        .await
        .expect("the deadline answers while the provider is silent");
    assert_eq!(status, "4");
    assert!(fixture.echo.calls.lock().expect("observations").is_empty());
    fixture.stop().await;
}
// template:end authn:grpc-transport-test-authentication-deadline

#[tokio::test]
async fn the_configured_limit_sheds_the_next_call_without_starving_health() {
    let fixture = Fixture::limited(2).await;
    timeout(
        WAIT,
        fixture.echo_client().unary(request(UnaryRequest {
            message: "warm".to_owned(),
        })),
    )
    .await
    .expect("warm call")
    .unwrap();

    let held = fixture.echo_client();
    let calls: Vec<_> = (0..2)
        .map(|_| {
            let mut client = held.clone();
            tokio::spawn(async move {
                client
                    .unary(request(UnaryRequest {
                        message: "hold".to_owned(),
                    }))
                    .await
            })
        })
        .collect();
    fixture.echo.hold.wait_for(2).await;

    let exhausted = timeout(
        WAIT,
        fixture.echo_client().unary(request(UnaryRequest {
            message: "overflow".to_owned(),
        })),
    )
    .await
    .expect("overflow call")
    .unwrap_err();
    assert_eq!(exhausted.code(), Code::ResourceExhausted);
    assert_eq!(exhausted.message(), service_failure::AT_CAPACITY_DETAIL);
    assert_eq!(reason(&exhausted), "SERVICE_UNAVAILABLE");

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

    let followed = timeout(
        WAIT,
        fixture.echo_client().unary(request(UnaryRequest {
            message: "after".to_owned(),
        })),
    )
    .await
    .expect("call after release")
    .unwrap();
    assert_eq!(followed.into_inner().message, "after");
    drop(held);
    fixture.stop().await;
}

#[tokio::test]
async fn unary_deadline_is_deadline_exceeded_on_the_raw_http2_response() {
    let fixture = Fixture::plaintext().await;
    let address = fixture.address;
    let call = tokio::spawn(async move { raw_call(address, ECHO_SERVICE_UNARY).await });
    fixture.echo.hold.wait_for(1).await;
    let status = timeout(WAIT, call)
        .await
        .expect("raw deadline response")
        .expect("raw deadline task");
    assert_eq!(status, "4");
    fixture.stop().await;
}

/// The `grpc-status` response header of a unary `deadline` request with a
/// 100 ms `grpc-timeout`, sent to `path` without a client-side timer.
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
    let mut timeout_request = Request::new(());
    timeout_request.set_timeout(Duration::from_millis(100));
    let grpc_timeout = timeout_request
        .metadata()
        .get("grpc-timeout")
        .expect("set_timeout header")
        .to_str()
        .unwrap()
        .to_owned();
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("http://{address}{path}"))
        .header("host", address.to_string())
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("grpc-timeout", grpc_timeout);
    // template:begin authn:grpc-transport-test-deadline-bearer
    builder = builder.header("authorization", format!("Bearer {ACCEPTED}"));
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

const ECHO_SERVICE_UNARY: &str = "/example.v1.EchoService/Unary";

fn grpc_frame(message: &str) -> Vec<u8> {
    let text = message.as_bytes();
    let mut payload = Vec::with_capacity(2 + text.len());
    payload.push(0x0a);
    payload.push(u8::try_from(text.len()).unwrap());
    payload.extend_from_slice(text);
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(0);
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&payload);
    frame
}

#[tokio::test]
async fn handler_panic_is_internal_and_the_server_keeps_serving() {
    let fixture = Fixture::plaintext().await;
    let error = timeout(
        WAIT,
        fixture.echo_client().unary(request(UnaryRequest {
            message: "panic".to_owned(),
        })),
    )
    .await
    .expect("panic call")
    .unwrap_err();
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(error.message(), "request failed");
    assert_eq!(reason(&error), "INTERNAL_ERROR");
    let followed = timeout(
        WAIT,
        fixture.echo_client().unary(request(UnaryRequest {
            message: "after-panic".to_owned(),
        })),
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
        fixture.echo_client().unary(request(UnaryRequest {
            message: "classified".to_owned(),
        })),
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

    seed_ready(&fixture.readiness).await;
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
async fn tls13_with_a_trusted_ca_succeeds_and_tls12_is_refused() {
    let pki = Pki::new(&["127.0.0.1", "localhost"]);
    let fixture = Fixture::tls(&pki, false).await;
    let mut client = EchoServiceClient::new(tls_client(
        fixture.address,
        ClientTlsMaterial {
            ca_certificate_pem: Some(pem(&pki.ca_certificate)),
            identity: None,
        },
    ));
    let response = timeout(
        WAIT,
        client.unary(request(UnaryRequest {
            message: "tls13".to_owned(),
        })),
    )
    .await
    .expect("tls unary")
    .unwrap();
    assert_eq!(response.into_inner().message, "tls13");
    assert_tls_denied(fixture.address, &pki, None, true).await;
    drop(client);
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

    let mut client = EchoServiceClient::new(tls_client(
        fixture.address,
        ClientTlsMaterial {
            ca_certificate_pem: Some(pem(&pki.ca_certificate)),
            identity: Some(ClientIdentity {
                certificate_pem: pem(&pki.client_certificate),
                private_key_pem: pem(&pki.client_key).into(),
            }),
        },
    ));
    let response = timeout(
        WAIT,
        client.unary(request(UnaryRequest {
            message: "mtls".to_owned(),
        })),
    )
    .await
    .expect("mtls unary")
    .unwrap();
    assert_eq!(response.into_inner().message, "mtls");
    drop(client);
    fixture.stop().await;
}

#[test]
fn a_destination_scheme_that_disagrees_with_security_is_rejected() {
    let tls = ClientSecurity::Tls(ClientTlsMaterial::default());
    assert_eq!(
        infra_grpc::Client::new("http://127.0.0.1:1", tls, CLIENT_TIMEOUT).unwrap_err(),
        Error::DestinationSecurityMismatch
    );
    assert_eq!(
        infra_grpc::Client::new(
            "https://127.0.0.1:1",
            ClientSecurity::Plaintext,
            CLIENT_TIMEOUT
        )
        .unwrap_err(),
        Error::DestinationSecurityMismatch
    );
}

#[tokio::test]
async fn reflection_describes_the_committed_contract_and_health() {
    let fixture = Fixture::plaintext().await;
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
    ]);
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
    let names: Vec<&str> = listed.service.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&ECHO_SERVICE), "{names:?}");
    assert!(names.contains(&"grpc.health.v1.Health"), "{names:?}");
    for version in ["v1", "v1alpha"] {
        let reflection = format!("grpc.reflection.{version}.ServerReflection");
        assert!(names.contains(&reflection.as_str()), "{names:?}");
    }

    let MessageResponse::FileDescriptorResponse(files) = next().await else {
        panic!("a known symbol is answered with its file descriptor");
    };
    assert_ne!(files.file_descriptor_proto, [] as [Vec<u8>; 0]);

    // Older tools ask only for the version `v1` replaced.
    let mut older = v1alpha::server_reflection_client::ServerReflectionClient::new(
        plaintext_client(fixture.address),
    );
    let asks = tonic::codegen::tokio_stream::iter([v1alpha::ServerReflectionRequest {
        host: String::new(),
        message_request: Some(
            v1alpha::server_reflection_request::MessageRequest::ListServices(String::new()),
        ),
    }]);
    let mut answers = older
        .server_reflection_info(request(asks))
        .await
        .expect("v1alpha reflection opens")
        .into_inner();
    let answer = timeout(WAIT, answers.message())
        .await
        .expect("v1alpha reflection answers in time")
        .expect("v1alpha reflection stream stays open")
        .and_then(|answer| answer.message_response);
    let Some(v1alpha::server_reflection_response::MessageResponse::ListServicesResponse(listed)) =
        answer
    else {
        panic!("list services is answered with a service list");
    };
    assert!(
        listed.service.iter().any(|s| s.name == ECHO_SERVICE),
        "{listed:?}"
    );
    fixture.stop().await;
}

#[test]
fn bytes_that_are_not_a_descriptor_set_are_rejected() {
    assert_eq!(
        Services::new().describe(&[0xff]),
        Err(Error::InvalidFileDescriptorSet)
    );
}

#[test]
fn reflection_added_twice_is_rejected() {
    let mut services = Services::new();
    services.add_reflection().expect("first registration");
    assert_eq!(
        services.add_reflection(),
        Err(Error::DuplicateService(
            "grpc.reflection.v1.ServerReflection"
        ))
    );
}

#[test]
fn a_service_is_described_before_it_is_added_and_added_once() {
    assert_eq!(
        Services::new().add(EchoServiceServer::new(Echo::new())),
        Err(Error::UndescribedService("example.v1.EchoService"))
    );
    let mut services = described();
    services
        .add(EchoServiceServer::new(Echo::new()))
        .expect("first registration");
    assert_eq!(
        services.add(EchoServiceServer::new(Echo::new())),
        Err(Error::DuplicateService("example.v1.EchoService"))
    );
}

/// Counted on one thread, so the local recorder sees the server's tasks and
/// the client's.
#[tokio::test]
async fn calls_are_counted_by_described_method_whatever_ended_them() {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.echo_client();

    timeout(
        WAIT,
        client.unary(request(UnaryRequest {
            message: "answered".to_owned(),
        })),
    )
    .await
    .expect("answered call")
    .unwrap();

    // A handler's own catalog failure.
    let classified = timeout(
        WAIT,
        client.unary(request(UnaryRequest {
            message: "classified".to_owned(),
        })),
    )
    .await
    .expect("classified call")
    .unwrap_err();
    assert_eq!(classified.code(), Code::InvalidArgument);

    // The caller stops waiting while the handler runs.
    let abandoned = tokio::spawn({
        let mut client = client.clone();
        async move {
            client
                .unary(request(UnaryRequest {
                    message: "hold".to_owned(),
                }))
                .await
        }
    });
    fixture.echo.hold.wait_for(1).await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());

    // The server's own deadline, and a method the contract does not have.
    assert_eq!(raw_call(fixture.address, ECHO_SERVICE_UNARY).await, "4");
    assert_eq!(
        raw_call(fixture.address, "/example.v1.EchoService/Invented").await,
        "12"
    );

    let unary = r#"grpc_service="example.v1.EchoService",grpc_method="Unary""#;
    let expected = [
        format!("grpc_server_started_total{{{unary}}} 4"),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="OK"}} 1"#),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="InvalidArgument"}} 1"#),
        format!(r#"grpc_server_failures_total{{{unary},failure_code="bad_request"}} 1"#),
        format!(r#"grpc_server_failures_total{{{unary},failure_code="gateway_timeout"}} 1"#),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="Canceled"}} 1"#),
        format!(r#"grpc_server_handled_total{{{unary},grpc_code="DeadlineExceeded"}} 1"#),
        r#"grpc_server_started_total{grpc_service="unknown",grpc_method="unknown"} 1"#.to_owned(),
        r#"grpc_server_handled_total{grpc_service="unknown",grpc_method="unknown",grpc_code="Unimplemented"} 1"#.to_owned(),
        format!("grpc_client_started_total{{{unary}}} 3"),
        format!(r#"grpc_client_handled_total{{{unary},grpc_code="OK"}} 1"#),
        format!(r#"grpc_client_handled_total{{{unary},grpc_code="Canceled"}} 1"#),
    ];
    // The server learns of the abandoned call when the reset arrives.
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
    // Only a catalog failure is counted as one: tonic's own `Unimplemented`
    // carries no reason, and the abandoned call got no answer.
    assert_eq!(
        rendered.matches("grpc_server_failures_total{").count(),
        2,
        "{rendered}"
    );
    fixture.stop().await;
}

/// A stream's status arrives in its trailers, after the response headers.
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
        // tonic encodes the messages that are ready together, so a stream
        // that panics at once may end before its first message is sent.
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

    // A caller that stops reading abandons the call.
    let unread = timeout(
        WAIT,
        client.server_stream(request(ServerStreamRequest {
            message: "unread".to_owned(),
        })),
    )
    .await
    .expect("the unread stream opens")
    .unwrap();
    drop(unread);

    // The connection outlives the stream that panicked.
    let followed = timeout(
        WAIT,
        client.unary(request(UnaryRequest {
            message: "after".to_owned(),
        })),
    )
    .await
    .expect("call after the streams")
    .unwrap();
    assert_eq!(followed.into_inner().message, "after");

    let stream = r#"grpc_service="example.v1.EchoService",grpc_method="ServerStream""#;
    let expected = [
        format!("grpc_server_started_total{{{stream}}} 3"),
        format!(r#"grpc_server_handled_total{{{stream},grpc_code="Aborted"}} 1"#),
        format!(r#"grpc_server_handled_total{{{stream},grpc_code="Internal"}} 1"#),
        format!(r#"grpc_server_failures_total{{{stream},failure_code="conflict"}} 1"#),
        format!(r#"grpc_server_failures_total{{{stream},failure_code="internal_error"}} 1"#),
        format!("grpc_client_started_total{{{stream}}} 3"),
        format!(r#"grpc_client_handled_total{{{stream},grpc_code="Aborted"}} 1"#),
        format!(r#"grpc_client_handled_total{{{stream},grpc_code="Internal"}} 1"#),
        format!(r#"grpc_client_handled_total{{{stream},grpc_code="Canceled"}} 1"#),
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

/// A peer that accepts and never answers: the call's own deadline ends it.
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
    let mut call = Request::new(UnaryRequest {
        message: "unanswered".to_owned(),
    });
    call.set_timeout(Duration::from_millis(100));
    let status = timeout(WAIT, client.unary(call))
        .await
        .expect("the deadline ends the call")
        .unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(status.message(), "request deadline exceeded");

    // A call that sets no deadline is still bounded by the client's own.
    let bounded = infra_grpc::Client::new(
        &format!("http://{address}"),
        ClientSecurity::Plaintext,
        Duration::from_millis(100),
    )
    .expect("bounded client");
    let status = timeout(
        WAIT,
        EchoServiceClient::new(bounded).unary(Request::new(UnaryRequest {
            message: "unanswered".to_owned(),
        })),
    )
    .await
    .expect("the client timeout ends the call")
    .unwrap_err();
    assert_eq!(status.code(), Code::DeadlineExceeded);
    silent.abort();
}

fn pem(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("rcgen emits ASCII PEM")
}

fn tls_client(address: SocketAddr, material: ClientTlsMaterial) -> infra_grpc::Client {
    infra_grpc::Client::new(
        &format!("https://127.0.0.1:{}", address.port()),
        ClientSecurity::Tls(material),
        CLIENT_TIMEOUT,
    )
    .expect("tls client")
}

// template:begin authn:grpc-transport-test-auth-provider
struct ProviderFixture {
    /// While set, the provider reads a request and never answers it.
    silent: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Arc<Notify>,
    cancel: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ProviderFixture {
    async fn stop(self) {
        self.cancel.cancel();
        self.stop.store(true, Ordering::Release);
        self.wake.notify_waiters();
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
    let stop = Arc::new(AtomicBool::new(false));
    let wake = Arc::new(Notify::new());
    let cancel = tokio_util::sync::CancellationToken::new();
    let task = tokio::spawn({
        let silent = Arc::clone(&silent);
        let stop = Arc::clone(&stop);
        let wake = Arc::clone(&wake);
        async move {
            loop {
                let notified = wake.notified();
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let accepted = tokio::select! {
                    biased;
                    () = notified => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let silent = Arc::clone(&silent);
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    // The form body can arrive after the request head.
                    let mut request = [0_u8; 4096];
                    let mut read = 0;
                    let contains = |seen: &[u8], needle: &[u8]| {
                        seen.windows(needle.len()).any(|window| window == needle)
                    };
                    while !contains(&request[..read], b"token=") {
                        match stream.read(&mut request[read..]).await {
                            Ok(more) if more > 0 => read += more,
                            _ => break,
                        }
                    }
                    if silent.load(Ordering::Acquire) {
                        std::future::pending::<()>().await;
                    }
                    let unscoped = contains(&request[..read], b"token=unscoped");
                    let response = introspection_response(unscoped);
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        }
    });
    let fixture =
        FixtureTransport::new("provider.test", address, &pki.ca_der, cancel.child_token()).unwrap();
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
            provider_concurrency: std::num::NonZeroUsize::new(32).unwrap(),
            cache: Some(IntrospectionCacheOptions::new(16, Duration::from_secs(60)).unwrap()),
        },
        fixture,
    )
    .unwrap();
    (
        verifier,
        ProviderFixture {
            silent,
            stop,
            wake,
            cancel,
            task,
        },
    )
}

/// An active token, with the `echo.read` scope unless `unscoped`.
fn introspection_response(unscoped: bool) -> String {
    let scope = if unscoped { "" } else { "echo.read" };
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    let body = format!(
        r#"{{"active":true,"iss":"https://issuer.example","aud":"api","exp":{expiry},"sub":"subject","scope":"{scope}"}}"#
    );
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
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
// template:end authn:grpc-transport-test-auth-provider

struct Pki {
    ca_certificate: Vec<u8>,
    // template:begin authn:grpc-transport-test-auth-ca-der
    ca_der: Vec<u8>,
    // template:end authn:grpc-transport-test-auth-ca-der
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
            // template:begin authn:grpc-transport-test-auth-ca-der-init
            ca_der: issuer.der().to_vec(),
            // template:end authn:grpc-transport-test-auth-ca-der-init
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
