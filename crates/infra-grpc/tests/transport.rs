#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]

//! Real loopback transport proof for the generated Echo registration.
//!
//! This deliberately mounts only `register_echo_service` and talks through
//! tonic clients over TCP.  It therefore catches a generated adapter bypassing
//! the inbound call owner, rather than repeating its policy in a test service.

use std::{
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

// template:begin authn:grpc-transport-test-auth-time
use std::time::{SystemTime, UNIX_EPOCH};
// template:end authn:grpc-transport-test-auth-time

use futures_util::Stream;
use grpc_contracts::generated::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_client::EchoServiceClient, echo_service_server::EchoService,
    register_echo_service,
};
use health::{Readiness, RefreshPolicy};
// template:begin authn:grpc-transport-test-auth-imports
use infra_bearerauthn::{
    IntrospectionOptions, ProviderUrl, Verifier,
    test_support::{FixtureTransport, prepare_introspection_with_fixture},
};
// template:end authn:grpc-transport-test-auth-imports
use infra_grpc::{
    Client, ClientSecurity, ClientTlsMaterial, Operation, RunningServer, Server, ServerOptions,
    ServerSecurity, ServerTlsMaterial, Services,
};
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
use tokio::{
    io::AsyncReadExt as _,
    net::TcpStream,
    sync::Notify,
    time::{Instant, timeout},
};
// template:begin authn:grpc-transport-test-auth-listener
use tokio::net::TcpListener;
// template:end authn:grpc-transport-test-auth-listener
// template:begin authn:grpc-transport-test-auth-io
use tokio::{io::AsyncWriteExt as _, task::JoinHandle};
// template:end authn:grpc-transport-test-auth-io
// template:begin authn:grpc-transport-test-auth-tls-acceptor
use tokio_rustls::TlsAcceptor;
// template:end authn:grpc-transport-test-auth-tls-acceptor
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use tonic::{Code, Request, Response, Status};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};

const DEADLINE: Duration = Duration::from_secs(3);
const DRAIN: Duration = Duration::from_secs(9);

type ResponseStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send + 'static>>;

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

#[derive(Clone, Default)]
struct Echo {
    calls: Arc<Mutex<Vec<SeenCall>>>,
    entered: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    entered_notify: Arc<Notify>,
    release: CancellationToken,
}

impl Echo {
    fn observe<T>(&self, request: &Request<T>, seen: Seen) {
        self.calls
            .lock()
            .expect("test observations remain unlocked")
            .push(SeenCall {
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

    async fn hold(&self) {
        let witness = DropWitness(Arc::clone(&self.dropped));
        self.entered.fetch_add(1, Ordering::AcqRel);
        self.entered_notify.notify_waiters();
        self.release.cancelled().await;
        drop(witness);
    }

    async fn wait_for_entries(&self, expected: usize) {
        timeout(DEADLINE, async {
            while self.entered.load(Ordering::Acquire) < expected {
                let notified = self.entered_notify.notified();
                if self.entered.load(Ordering::Acquire) < expected {
                    notified.await;
                }
            }
        })
        .await
        .expect("expected handlers reach the real server");
    }
}

struct DropWitness(Arc<AtomicUsize>);

impl Drop for DropWitness {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[tonic::async_trait]
impl EchoService for Echo {
    type ServerStreamStream = ResponseStream<ServerStreamResponse>;
    type BidiStreamStream = ResponseStream<BidiStreamResponse>;

    async fn unary(
        &self,
        request: Request<UnaryRequest>,
    ) -> Result<Response<UnaryResponse>, Status> {
        self.observe(&request, Seen::Unary);
        match request.into_inner().message.as_str() {
            "raw-initial" => Err(Status::invalid_argument("forged detail: do not leak")),
            "panic-initial" => panic!("panic detail: do not leak"),
            "hold" | "deadline" => {
                self.hold().await;
                Ok(Response::new(UnaryResponse {
                    message: "released".to_owned(),
                }))
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
        let stream: ResponseStream<ServerStreamResponse> = match message.as_str() {
            "deadline-stream" | "hold-stream" => Box::pin(HeldStream {
                first: Some(ServerStreamResponse {
                    message: "first".to_owned(),
                }),
                _witness: DropWitness(Arc::clone(&self.dropped)),
            }),
            "raw-later" => Box::pin(RawLaterStream {
                first: Some(ServerStreamResponse {
                    message: "first".to_owned(),
                }),
                panic: false,
            }),
            "panic-later" => Box::pin(RawLaterStream {
                first: Some(ServerStreamResponse {
                    message: "first".to_owned(),
                }),
                panic: true,
            }),
            other => Box::pin(tokio_stream::iter([Ok(ServerStreamResponse {
                message: other.to_owned(),
            })])),
        };
        Ok(Response::new(stream))
    }

    async fn bidi_stream(
        &self,
        request: Request<tonic::Streaming<BidiStreamRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        self.observe(&request, Seen::BidiStream);
        let mut input = request.into_inner();
        let first = input
            .message()
            .await?
            .expect("test client sends one message")
            .message;
        let stream: ResponseStream<BidiStreamResponse> = match first.as_str() {
            "raw-later" => Box::pin(RawLaterStream {
                first: Some(BidiStreamResponse {
                    message: "first".to_owned(),
                }),
                panic: false,
            }),
            "panic-later" => Box::pin(RawLaterStream {
                first: Some(BidiStreamResponse {
                    message: "first".to_owned(),
                }),
                panic: true,
            }),
            other => Box::pin(tokio_stream::iter([Ok(BidiStreamResponse {
                message: other.to_owned(),
            })])),
        };
        Ok(Response::new(stream))
    }
}

struct RawLaterStream<T> {
    first: Option<T>,
    panic: bool,
}

impl<T: Unpin> Stream for RawLaterStream<T> {
    type Item = Result<T, Status>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() {
            return Poll::Ready(Some(Ok(first)));
        }
        assert!(!self.panic, "later panic detail: do not leak");
        Poll::Ready(Some(Err(Status::permission_denied(
            "later forged detail: do not leak",
        ))))
    }
}

struct HeldStream {
    first: Option<ServerStreamResponse>,
    _witness: DropWitness,
}

impl Stream for HeldStream {
    type Item = Result<ServerStreamResponse, Status>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.first
            .take()
            .map_or(Poll::Pending, |item| Poll::Ready(Some(Ok(item))))
    }
}

struct Fixture {
    running: RunningServer,
    readiness: Readiness,
    echo: Echo,
    address: SocketAddr,
    // template:begin authn:grpc-transport-test-auth-provider-field
    provider: ProviderFixture,
    // template:end authn:grpc-transport-test-auth-provider-field
}

impl Fixture {
    async fn plaintext() -> Self {
        Self::start(ServerSecurity::Plaintext).await
    }

    async fn start(security: ServerSecurity) -> Self {
        // template:begin authn:grpc-transport-test-auth-fixture
        let (verifier, provider) = verifier_fixture().await;
        // template:end authn:grpc-transport-test-auth-fixture
        let readiness = Readiness::new(Vec::new());
        readiness
            .refresh(RefreshPolicy {
                interval: Duration::from_secs(1),
                probe_budget: Duration::from_secs(1),
                failure_threshold: 1,
            })
            .await;
        let echo = Echo::default();
        let mut services = Services::new();
        register_echo_service(&mut services, echo.clone())
            .expect("generated service registers once");
        let prepared = Server::prepare(
            services,
            readiness.reader(),
            // template:begin authn:grpc-transport-test-auth-server-argument
            verifier,
            // template:end authn:grpc-transport-test-auth-server-argument
            ServerOptions {
                security,
                effective_drain_budget: DRAIN,
            },
        )
        .expect("accepted transport setup prepares");
        let bound = prepared
            .bind("127.0.0.1:0".parse().unwrap())
            .await
            .expect("loopback binds");
        let address = bound.local_addr();
        let running = bound.start();
        running.open_admission();
        Self {
            running,
            readiness,
            echo,
            address,
            // template:begin authn:grpc-transport-test-auth-provider-init
            provider,
            // template:end authn:grpc-transport-test-auth-provider-init
        }
    }

    fn client(&self) -> EchoServiceClient<Client> {
        let transport = grpc_contracts::generated::echo_service_client_transport(
            Client::new(
                &format!("http://{}", self.address),
                ClientSecurity::Plaintext,
            )
            .unwrap(),
        )
        .unwrap();
        EchoServiceClient::new(transport)
    }

    async fn native_client(&self) -> EchoServiceClient<tonic::transport::Channel> {
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{}", self.address))
            .unwrap()
            .connect()
            .await
            .unwrap();
        EchoServiceClient::new(channel)
    }

    async fn stop(mut self) {
        self.echo.release.cancel();
        self.running
            .drain(Instant::now() + DEADLINE)
            .await
            .expect("server drains");
        // template:begin authn:grpc-transport-test-auth-provider-stop
        self.provider.stop().await;
        // template:end authn:grpc-transport-test-auth-provider-stop
    }
}

fn request<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.extensions_mut().insert(Operation {
        deadline: Instant::now() + DEADLINE,
    });
    request
        .metadata_mut()
        .insert("authorization", "Bearer accepted".parse().unwrap());
    request
}

fn health_watch_request(service: String) -> Request<HealthCheckRequest> {
    let mut request = Request::new(HealthCheckRequest { service });
    request
        .metadata_mut()
        .insert("authorization", "Bearer accepted".parse().unwrap());
    request
}

// template:begin authn:grpc-transport-test-auth-cardinalities
#[tokio::test]
async fn generated_registration_applies_authentication_validation_and_privacy_to_all_cardinalities()
{
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.client();

    assert_eq!(
        client
            .unary(request(UnaryRequest {
                message: "one".to_owned()
            }))
            .await
            .unwrap()
            .into_inner()
            .message,
        "one"
    );
    assert_eq!(
        client
            .client_stream(request(tokio_stream::iter([ClientStreamRequest {
                message: "two".to_owned()
            }])))
            .await
            .unwrap()
            .into_inner()
            .message,
        "two"
    );
    let mut server = client
        .server_stream(request(ServerStreamRequest {
            message: "three".to_owned(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(server.message().await.unwrap().unwrap().message, "three");
    let mut bidi = client
        .bidi_stream(request(tokio_stream::iter([BidiStreamRequest {
            message: "four".to_owned(),
        }])))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(bidi.message().await.unwrap().unwrap().message, "four");

    assert_eq!(
        fixture.echo.calls.lock().unwrap().as_slice(),
        [
            SeenCall {
                cardinality: Seen::Unary,
                had_identity: true,
                had_authorization: false
            },
            SeenCall {
                cardinality: Seen::ClientStream,
                had_identity: true,
                had_authorization: false
            },
            SeenCall {
                cardinality: Seen::ServerStream,
                had_identity: true,
                had_authorization: false
            },
            SeenCall {
                cardinality: Seen::BidiStream,
                had_identity: true,
                had_authorization: false
            },
        ]
    );

    let invalid = client
        .unary(request(UnaryRequest {
            message: String::new(),
        }))
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), Code::InvalidArgument);
    assert!(!invalid.message().contains("accepted"));
    fixture.stop().await;
}
// template:end authn:grpc-transport-test-auth-cardinalities

#[tokio::test]
async fn later_invalid_stream_message_is_not_delivered_to_the_feature() {
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.client();
    let error = client
        .client_stream(request(tokio_stream::iter([
            ClientStreamRequest {
                message: "first".to_owned(),
            },
            ClientStreamRequest {
                message: String::new(),
            },
        ])))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);
    {
        let calls = fixture.echo.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].cardinality, Seen::ClientStream);
        // template:begin authn:grpc-transport-test-auth-stream-identity
        assert!(calls[0].had_identity);
        assert!(!calls[0].had_authorization);
        // template:end authn:grpc-transport-test-auth-stream-identity
    }
    fixture.stop().await;
}

#[tokio::test]
async fn initial_and_later_handler_failures_are_sanitized_on_the_wire() {
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.client();
    for input in ["raw-initial", "panic-initial"] {
        let error = client
            .unary(request(UnaryRequest {
                message: input.to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        assert_eq!(error.message(), "request failed");
    }
    for input in ["raw-later", "panic-later"] {
        let mut response = client
            .server_stream(request(ServerStreamRequest {
                message: input.to_owned(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.message().await.unwrap().unwrap().message, "first");
        let error = response.message().await.unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        assert_eq!(error.message(), "request failed");

        let mut response = client
            .bidi_stream(request(tokio_stream::iter([BidiStreamRequest {
                message: input.to_owned(),
            }])))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.message().await.unwrap().unwrap().message, "first");
        let error = response.message().await.unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        assert_eq!(error.message(), "request failed");
    }
    fixture.stop().await;
}

#[tokio::test]
async fn held_business_calls_shed_without_starving_standard_health_and_release_after_cancellation()
{
    let fixture = Fixture::plaintext().await;
    // Admit one authenticated stream at a time so the fixture's single
    // introspection slot does not shed calls before transport capacity fills.
    // Three connections keep each below the native 100-stream connection cap.
    let mut clients = [
        fixture.native_client().await,
        fixture.native_client().await,
        fixture.native_client().await,
    ];
    let mut calls = Vec::new();
    for index in 0..256 {
        let mut response = timeout(
            DEADLINE,
            clients[index % 3].server_stream(request(ServerStreamRequest {
                message: "hold-stream".to_owned(),
            })),
        )
        .await
        .expect("stream admission completes")
        .expect("stream authenticates")
        .into_inner();
        assert_eq!(
            timeout(DEADLINE, response.message())
                .await
                .expect("held stream starts")
                .unwrap()
                .unwrap()
                .message,
            "first"
        );
        calls.push(response);
    }
    let exhausted = fixture
        .client()
        .unary(request(UnaryRequest {
            message: "after".to_owned(),
        }))
        .await
        .unwrap_err();
    assert_eq!(exhausted.code(), Code::ResourceExhausted);

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{}", fixture.address))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let health = timeout(
        DEADLINE,
        HealthClient::new(channel).check(Request::new(HealthCheckRequest {
            service: String::new(),
        })),
    )
    .await
    .expect("health is not starved by business capacity")
    .unwrap();
    assert_eq!(health.into_inner().status, ServingStatus::Serving as i32);

    drop(calls);
    timeout(DEADLINE, async {
        while fixture.echo.dropped.load(Ordering::Acquire) < 256 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("held work is dropped");
    assert_eq!(
        fixture
            .client()
            .unary(request(UnaryRequest {
                message: "after".to_owned()
            }))
            .await
            .unwrap()
            .into_inner()
            .message,
        "after"
    );
    drop(clients);
    fixture.stop().await;
}

#[tokio::test]
async fn caller_deadline_cancels_work_and_releases_its_permit() {
    let fixture = Fixture::plaintext().await;
    // Channel installs its own grpc-timeout timer, which can race the server
    // into CANCELLED. Keep native tonic messages over Hyper's HTTP/2 transport
    // here so only the server under test owns the one-second request deadline.
    let stream = timeout(DEADLINE, TcpStream::connect(fixture.address))
        .await
        .unwrap()
        .unwrap();
    let (sender, connection) = timeout(
        DEADLINE,
        hyper::client::conn::http2::handshake::<_, _, tonic::body::Body>(
            hyper_util::rt::TokioExecutor::new(),
            hyper_util::rt::TokioIo::new(stream),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let connection = tokio::spawn(connection);
    let address = fixture.address;
    let transport = tower::service_fn(move |mut request: http::Request<tonic::body::Body>| {
        let mut sender = sender.clone();
        let uri = format!("http://{address}{}", request.uri().path())
            .parse()
            .unwrap();
        *request.uri_mut() = uri;
        async move {
            sender.ready().await?;
            sender.send_request(request).await
        }
    });
    let mut client = EchoServiceClient::new(transport);
    let mut deadline = request(UnaryRequest {
        message: "deadline".to_owned(),
    });
    deadline.set_timeout(Duration::from_secs(1));
    let call = tokio::spawn(async move { client.unary(deadline).await });
    fixture.echo.wait_for_entries(1).await;
    let error = timeout(DEADLINE, call)
        .await
        .expect("server deadline terminates initial work")
        .expect("client joins")
        .unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
    assert_eq!(fixture.echo.dropped.load(Ordering::Acquire), 1);
    assert_eq!(
        fixture
            .client()
            .unary(request(UnaryRequest {
                message: "after".to_owned()
            }))
            .await
            .unwrap()
            .into_inner()
            .message,
        "after"
    );
    fixture.stop().await;
    let _ = timeout(DEADLINE, connection)
        .await
        .expect("HTTP/2 driver stops")
        .expect("HTTP/2 driver joins");
}

#[tokio::test]
async fn deadline_wakes_a_suspended_response_and_emits_terminal_status() {
    let fixture = Fixture::plaintext().await;
    let mut client = fixture.native_client().await;
    let mut request = request(ServerStreamRequest {
        message: "deadline-stream".to_owned(),
    });
    request.set_timeout(Duration::from_secs(1));
    let mut response = client.server_stream(request).await.unwrap().into_inner();
    assert_eq!(response.message().await.unwrap().unwrap().message, "first");
    let error = timeout(DEADLINE, response.message())
        .await
        .expect("server wakes the pending body for deadline trailers")
        .unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
    assert_eq!(fixture.echo.dropped.load(Ordering::Acquire), 1);
    fixture.stop().await;
}

#[tokio::test]
async fn unknown_methods_do_not_authenticate_or_invoke_the_feature() {
    let fixture = Fixture::plaintext().await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{}", fixture.address))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = tonic::client::Grpc::new(channel);
    for authenticated in [false, true] {
        let message = UnaryRequest {
            message: "unknown".to_owned(),
        };
        let request = if authenticated {
            request(message)
        } else {
            Request::new(message)
        };
        client.ready().await.unwrap();
        let result: Result<Response<UnaryResponse>, Status> = client
            .unary(
                request,
                http::uri::PathAndQuery::from_static("/example.v1.EchoService/Unknown"),
                tonic_prost::ProstCodec::default(),
            )
            .await;
        assert_eq!(result.unwrap_err().code(), Code::Unimplemented);
    }
    assert!(fixture.echo.calls.lock().unwrap().is_empty());
    // template:begin authn:grpc-transport-test-unknown-provider
    assert_eq!(fixture.provider.requests.load(Ordering::Acquire), 0);
    // template:end authn:grpc-transport-test-unknown-provider
    fixture.stop().await;
}

#[tokio::test]
async fn health_reports_readiness_unknown_watch_and_monotone_drain() {
    let fixture = Fixture::plaintext().await;
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{}", fixture.address))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut health = HealthClient::new(channel);
    let unknown = health
        .check(Request::new(HealthCheckRequest {
            service: "missing".to_owned(),
        }))
        .await
        .unwrap_err();
    assert_eq!(unknown.code(), Code::NotFound);
    let mut unknown_watch = health
        .watch(health_watch_request("missing".to_owned()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        unknown_watch.message().await.unwrap().unwrap().status,
        ServingStatus::ServiceUnknown as i32
    );
    assert!(
        timeout(Duration::from_millis(20), unknown_watch.message())
            .await
            .is_err(),
        "unknown Watch must remain open"
    );
    let mut watch = health
        .watch(health_watch_request(String::new()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        watch.message().await.unwrap().unwrap().status,
        ServingStatus::Serving as i32
    );
    fixture.readiness.start_drain();
    fixture.running.begin_drain();
    // template:begin authn:grpc-transport-test-drain-provider-before
    let provider_requests = fixture.provider.requests.load(Ordering::Acquire);
    // template:end authn:grpc-transport-test-drain-provider-before
    let rejected = fixture
        .client()
        .unary(request(UnaryRequest {
            message: "after-drain".to_owned(),
        }))
        .await
        .unwrap_err();
    assert_eq!(rejected.code(), Code::Unavailable);
    assert!(fixture.echo.calls.lock().unwrap().is_empty());
    // template:begin authn:grpc-transport-test-drain-provider-after
    assert_eq!(
        fixture.provider.requests.load(Ordering::Acquire),
        provider_requests
    );
    // template:end authn:grpc-transport-test-drain-provider-after
    assert_eq!(
        watch.message().await.unwrap().unwrap().status,
        ServingStatus::NotServing as i32
    );
    let stopped = health
        .check(Request::new(HealthCheckRequest {
            service: String::new(),
        }))
        .await
        .unwrap();
    assert_eq!(
        stopped.into_inner().status,
        ServingStatus::NotServing as i32
    );
    fixture.stop().await;
}

#[tokio::test]
async fn expired_drain_retains_transport_tasks_for_the_existing_cleanup_stage() {
    let mut fixture = Fixture::plaintext().await;
    let mut client = fixture.native_client().await;
    let call = tokio::spawn(async move {
        client
            .unary(request(UnaryRequest {
                message: "hold".to_owned(),
            }))
            .await
    });
    fixture.echo.wait_for_entries(1).await;
    assert_eq!(
        fixture.running.drain(Instant::now()).await,
        Err(infra_grpc::Error::DrainTimedOut)
    );
    fixture
        .running
        .join_shutdown(Instant::now() + DEADLINE)
        .await
        .expect("forced transport work joins in cleanup");
    assert_eq!(fixture.echo.dropped.load(Ordering::Acquire), 1);
    assert!(
        timeout(DEADLINE, call)
            .await
            .expect("client observes closure")
            .expect("client task joins")
            .is_err()
    );
    fixture.stop().await;
}

#[tokio::test]
async fn tls13_and_mtls_accept_only_the_trusted_client_and_reject_tls12() {
    let pki = Pki::new("localhost");
    let fixture = Fixture::start(ServerSecurity::Tls(ServerTlsMaterial {
        certificate_pem: pki.server_certificate.clone(),
        private_key_pem: pki.server_key.clone(),
        client_ca_pem: Some(pki.ca_certificate.clone()),
    }))
    .await;
    let transport = grpc_contracts::generated::echo_service_client_transport(
        Client::new(
            &format!("https://localhost:{}", fixture.address.port()),
            ClientSecurity::Tls(ClientTlsMaterial {
                ca_certificate_pem: Some(pki.ca_certificate.clone()),
                certificate_pem: Some(pki.client_certificate.clone()),
                private_key_pem: Some(pki.client_key.clone()),
            }),
        )
        .unwrap(),
    )
    .unwrap();
    let mut client = EchoServiceClient::new(transport);
    assert_eq!(
        client
            .unary(request(UnaryRequest {
                message: "tls13".to_owned()
            }))
            .await
            .unwrap()
            .into_inner()
            .message,
        "tls13"
    );

    assert_tls_denied(fixture.address, &pki, None, false).await;
    let wrong = Pki::new("localhost");
    assert_tls_denied(
        fixture.address,
        &pki,
        Some((&wrong.client_certificate, &wrong.client_key)),
        false,
    )
    .await;
    assert_tls_denied(fixture.address, &pki, None, true).await;
    fixture.stop().await;
}

// template:begin authn:grpc-transport-test-auth-provider
struct ProviderFixture {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    requests: Arc<AtomicUsize>,
}

impl ProviderFixture {
    async fn stop(self) {
        self.cancel.cancel();
        self.task.await.expect("provider task joins");
    }
}

async fn verifier_fixture() -> (Verifier, ProviderFixture) {
    let pki = Pki::new("provider.test");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut tls = server_tls_config(&pki, false);
    // This provider writes HTTP/1.1; advertising h2 would select the wrong wire protocol.
    tls.alpn_protocols.clear();
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let cancel = CancellationToken::new();
    let requests = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn({
        let cancel = cancel.clone();
        let requests = Arc::clone(&requests);
        async move {
            loop {
                let accepted = tokio::select! { () = cancel.cancelled() => break, accepted = listener.accept() => accepted };
                let Ok((stream, _)) = accepted else {
                    continue;
                };
                let acceptor = acceptor.clone();
                let requests = Arc::clone(&requests);
                tokio::spawn(async move {
                    if let Ok(mut stream) = acceptor.accept(stream).await {
                        let mut request = [0_u8; 4096];
                        let _ = stream.read(&mut request).await;
                        requests.fetch_add(1, Ordering::AcqRel);
                        let expiry = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                            + 60;
                        let body = format!(
                            r#"{{"active":true,"iss":"https://issuer.example","aud":"api","exp":{expiry},"sub":"subject"}}"#
                        );
                        let response = format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                        let _ = stream.shutdown().await;
                    }
                });
            }
        }
    });
    let fixture =
        FixtureTransport::new("provider.test", address, &pki.ca_der, cancel.child_token()).unwrap();
    let verifier = prepare_introspection_with_fixture(
        IntrospectionOptions {
            issuer: ProviderUrl::parse("https://issuer.example").unwrap(),
            audiences: vec!["api".to_owned()],
            endpoint: ProviderUrl::parse_endpoint(&format!(
                "https://provider.test:{}/introspect",
                address.port()
            ))
            .unwrap(),
            client_id: "fixture".to_owned(),
            client_secret: SecretString::from("fixture"),
            provider_concurrency: std::num::NonZeroUsize::new(1).unwrap(),
            cache: None,
        },
        fixture,
    )
    .unwrap();
    (
        verifier,
        ProviderFixture {
            cancel,
            task,
            requests,
        },
    )
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
    fn new(host: &str) -> Self {
        let mut ca = CertificateParams::default();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let issuer = CertifiedIssuer::self_signed(ca, KeyPair::generate().unwrap()).unwrap();
        let (server_certificate, server_key) =
            leaf(host, vec![ExtendedKeyUsagePurpose::ServerAuth], &issuer);
        let (client_certificate, client_key) =
            leaf("client", vec![ExtendedKeyUsagePurpose::ClientAuth], &issuer);
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
    host: &str,
    usages: Vec<ExtendedKeyUsagePurpose>,
    issuer: &CertifiedIssuer<'_, KeyPair>,
) -> (Vec<u8>, Vec<u8>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![host.to_owned()]).unwrap();
    params.extended_key_usages = usages;
    let certificate = params.signed_by(&key, issuer).unwrap();
    (
        certificate.pem().into_bytes(),
        key.serialize_pem().into_bytes(),
    )
}

fn server_tls_config(pki: &Pki, client_auth: bool) -> rustls::ServerConfig {
    // PEM parsing here follows the production parser through the public material.
    let certs = rustls::pki_types::CertificateDer::pem_slice_iter(&pki.server_certificate)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(&pki.server_key).unwrap();
    let builder = rustls::ServerConfig::builder_with_provider(
        rustls::crypto::aws_lc_rs::default_provider().into(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap();
    let mut config = if client_auth {
        let mut roots = RootCertStore::empty();
        for certificate in rustls::pki_types::CertificateDer::pem_slice_iter(&pki.ca_certificate) {
            roots.add(certificate.unwrap()).unwrap();
        }
        builder.with_client_cert_verifier(
            rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .unwrap(),
        )
    } else {
        builder.with_no_client_auth()
    }
    .with_single_cert(certs, key)
    .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec()];
    config
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
    let versions = if tls12 {
        vec![&rustls::version::TLS12]
    } else {
        vec![&rustls::version::TLS13]
    };
    let builder =
        ClientConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_protocol_versions(&versions)
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
        DEADLINE,
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), stream),
    )
    .await;
    let denied = match result {
        Ok(Err(_)) => true,
        Ok(Ok(mut stream)) => {
            // TLS 1.3 can finish the client's flight before the client reads
            // the server's certificate-required or untrusted-certificate alert.
            // A timeout is not proof of denial: observe an error or peer close.
            let mut byte = [0_u8; 1];
            matches!(
                timeout(DEADLINE, stream.read(&mut byte)).await,
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
