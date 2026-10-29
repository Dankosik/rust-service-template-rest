#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]

//! Loopback proof of service-token admission on the tonic router.

use std::{
    fmt::Write as _,
    net::SocketAddr,
    num::NonZeroU32,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use grpc_contracts::example::v1::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_client::EchoServiceClient,
    echo_service_server::{EchoService, EchoServiceServer},
};
use health::{Readiness, RefreshPolicy};
use infra_grpc::{Caller, ClientSecurity, Error, Limits, Services, server_options};
use infra_http::Server;
use infra_serviceauth::{
    CallerName, JwtType, SigningOptions, TokenSource, TrustRegistry, Verifier,
    test_support::{TestKey, registry},
};
use opentelemetry::trace::TracerProvider as _;
use prost::Message as _;
use tonic::{Code, Request, Response, Status};
use tracing_subscriber::layer::SubscriberExt as _;

const TYPE: &str = "example-svc+jwt";
const ECHO: &str = "/example.v1.EchoService";

/// The caller a handler saw, and whether `authorization` reached it.
type Seen = (Option<String>, bool);

/// What each handler call saw.
#[derive(Clone, Default)]
struct Echo {
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Echo {
    fn observe<T>(&self, request: &Request<T>) {
        let caller = request
            .extensions()
            .get::<Caller>()
            .map(|caller| caller.name().to_owned());
        let authorization = request.metadata().get("authorization").is_some();
        self.seen.lock().unwrap().push((caller, authorization));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

type Stream<T> =
    Pin<Box<dyn tonic::codegen::tokio_stream::Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl EchoService for Echo {
    type ServerStreamStream = Stream<ServerStreamResponse>;
    type BidiStreamStream = Stream<BidiStreamResponse>;

    async fn unary(
        &self,
        request: Request<UnaryRequest>,
    ) -> Result<Response<UnaryResponse>, Status> {
        self.observe(&request);
        Ok(Response::new(UnaryResponse {
            message: request.into_inner().message,
        }))
    }

    async fn client_stream(
        &self,
        request: Request<tonic::Streaming<ClientStreamRequest>>,
    ) -> Result<Response<ClientStreamResponse>, Status> {
        self.observe(&request);
        Ok(Response::new(ClientStreamResponse::default()))
    }

    async fn server_stream(
        &self,
        request: Request<ServerStreamRequest>,
    ) -> Result<Response<Self::ServerStreamStream>, Status> {
        self.observe(&request);
        Ok(Response::new(Box::pin(futures_util::stream::empty())))
    }

    async fn bidi_stream(
        &self,
        request: Request<tonic::Streaming<BidiStreamRequest>>,
    ) -> Result<Response<Self::BidiStreamStream>, Status> {
        self.observe(&request);
        Ok(Response::new(Box::pin(futures_util::stream::empty())))
    }
}

struct Keys {
    orders: TestKey,
    billing: TestKey,
}

impl Keys {
    fn new() -> Self {
        Self {
            orders: TestKey::new("orders/2026-10"),
            billing: TestKey::new("billing/2026-10"),
        }
    }

    fn verifier(&self) -> Verifier {
        Verifier::new(
            CallerName::parse("catalog").unwrap(),
            JwtType::parse(TYPE).unwrap(),
            TrustRegistry::parse(&registry(&[
                ("orders", &[&self.orders]),
                ("billing", &[&self.billing]),
            ]))
            .unwrap(),
        )
    }

    /// A token `key` signs, claiming `issuer`, for `audience`, issued `age` seconds ago.
    fn token(key: &TestKey, issuer: &str, audience: &str, age: u64) -> String {
        let issued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - age;
        key.sign(
            &serde_json::json!({"alg": "ES256", "typ": TYPE, "kid": key.kid()}),
            &serde_json::json!({"iss": issuer, "sub": issuer, "aud": audience, "iat": issued_at, "exp": issued_at + 60}),
        )
    }
}

fn limits() -> Limits {
    Limits {
        request_timeout: Duration::from_secs(8),
        max_in_flight: NonZeroU32::new(64),
        max_connections: NonZeroU32::new(64),
        max_connection_age: None,
    }
}

fn readiness() -> Readiness {
    Readiness::new(
        Vec::new(),
        RefreshPolicy {
            interval: Duration::from_secs(60),
            probe_budget: Duration::from_secs(1),
            failure_threshold: 1,
        },
    )
}

/// Echo with every method admitting orders, except server streaming,
/// which admits billing only.
fn echo_services(echo: &Echo) -> Services {
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    services.add(EchoServiceServer::new(echo.clone())).unwrap();
    for method in ["Unary", "ClientStream", "BidiStream"] {
        services
            .allow_callers(&format!("{ECHO}/{method}"), &["orders"])
            .unwrap();
    }
    services
        .allow_callers(&format!("{ECHO}/ServerStream"), &["billing"])
        .unwrap();
    services
}

async fn serve(services: Services) -> (Server, SocketAddr) {
    let app = infra_grpc::router(
        services,
        readiness().reader(),
        // template:begin authn:grpc-service-auth-test-bearer-argument
        None,
        // template:end authn:grpc-service-auth-test-bearer-argument
        limits(),
    )
    .unwrap();
    let server = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        app,
        server_options(limits()),
    )
    .await
    .unwrap();
    let address = server.local_addr();
    (server, address)
}

fn client(address: SocketAddr) -> EchoServiceClient<infra_grpc::Client> {
    EchoServiceClient::new(
        infra_grpc::Client::try_new(
            &format!("http://{address}"),
            ClientSecurity::Plaintext,
            Duration::from_secs(10),
        )
        .unwrap(),
    )
}

fn with_token<T>(message: T, token: Option<&str>) -> Request<T> {
    let mut request = Request::new(message);
    if let Some(token) = token {
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    request
}

/// The `grpc-status` of a call to `path`, which no generated client names.
async fn raw_status(address: SocketAddr, path: &str, token: &str) -> String {
    use tower::ServiceExt as _;

    let message = UnaryRequest::default().encode_to_vec();
    let mut frame = vec![0];
    frame.extend_from_slice(&u32::try_from(message.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&message);
    let request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {token}"))
        .body(tonic::body::Body::new(http_body_util::Full::new(
            bytes::Bytes::from(frame),
        )))
        .unwrap();
    let client = infra_grpc::Client::try_new(
        &format!("http://{address}"),
        ClientSecurity::Plaintext,
        Duration::from_secs(10),
    )
    .unwrap();
    let response = client.oneshot(request).await.unwrap();
    response
        .headers()
        .get("grpc-status")
        .expect("a refused call answers trailers-only")
        .to_str()
        .unwrap()
        .to_owned()
}

async fn unary(address: SocketAddr, token: Option<&str>) -> Result<String, Status> {
    client(address)
        .unary(with_token(
            UnaryRequest {
                message: "hello".to_owned(),
            },
            token,
        ))
        .await
        .map(|response| response.into_inner().message)
}

fn reason(status: &Status) -> String {
    use tonic_types::StatusExt as _;
    status
        .get_details_error_info()
        .map(|info| info.reason)
        .unwrap_or_default()
}

#[tokio::test]
async fn a_valid_token_names_its_caller_and_every_contract_breach_is_refused() {
    let keys = Keys::new();
    let echo = Echo::default();
    let mut services = echo_services(&echo);
    services.authenticate_callers(keys.verifier());
    let (server, address) = serve(services).await;

    let valid = Keys::token(&keys.orders, "orders", "catalog", 0);
    assert_eq!(unary(address, Some(&valid)).await.unwrap(), "hello");
    assert_eq!(echo.seen(), [(Some("orders".to_owned()), false)]);

    for (label, token) in [
        ("no token", None),
        (
            "a foreign audience",
            Some(Keys::token(&keys.orders, "orders", "billing", 0)),
        ),
        (
            "an expired token",
            Some(Keys::token(&keys.orders, "orders", "catalog", 120)),
        ),
        (
            "another issuer's key",
            Some(Keys::token(&keys.billing, "orders", "catalog", 0)),
        ),
        ("a token that is not a JWS", Some("not.a.jws".to_owned())),
    ] {
        let status = unary(address, token.as_deref()).await.unwrap_err();
        assert_eq!(status.code(), Code::Unauthenticated, "{label}");
        assert_eq!(reason(&status), "UNAUTHORIZED", "{label}");
    }
    assert_eq!(echo.seen().len(), 1, "no refused call reached the handler");

    // A method whose rule does not list the caller, and a path without a rule.
    let status = client(address)
        .server_stream(with_token(ServerStreamRequest::default(), Some(&valid)))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::PermissionDenied);
    assert_eq!(reason(&status), "FORBIDDEN");
    let status = raw_status(address, "/example.v1.EchoService/Missing", &valid).await;
    assert_eq!(status, "7", "a path outside the table is PERMISSION_DENIED");
    assert_eq!(echo.seen().len(), 1);
    drop(server.drain(Duration::from_secs(3)).await);
}

#[tokio::test]
async fn the_outbound_binding_signs_for_its_receiver_and_refuses_a_caller_authorization() {
    let keys = Keys::new();
    let echo = Echo::default();
    let mut services = echo_services(&echo);
    services.authenticate_callers(keys.verifier());
    let (server, address) = serve(services).await;
    let source = TokenSource::try_new(SigningOptions {
        name: CallerName::parse("orders").unwrap(),
        jwt_type: JwtType::parse(TYPE).unwrap(),
        key_id: keys.orders.kid().to_owned(),
        private_key: keys.orders.pem().into(),
    })
    .unwrap();
    let resource = || {
        infra_grpc::Client::try_new(
            &format!("http://{address}"),
            ClientSecurity::Plaintext,
            Duration::from_secs(10),
        )
        .unwrap()
    };
    let mut catalog =
        EchoServiceClient::new(resource().with_service_tokens(source.receiver("catalog").unwrap()));
    for _ in 0..2 {
        let response = catalog
            .unary(UnaryRequest {
                message: "signed".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(response.into_inner().message, "signed");
    }
    assert_eq!(echo.seen(), vec![(Some("orders".to_owned()), false); 2]);
    // A handle for another receiver signs a token this receiver refuses.
    let mut elsewhere =
        EchoServiceClient::new(resource().with_service_tokens(source.receiver("billing").unwrap()));
    let status = elsewhere.unary(UnaryRequest::default()).await.unwrap_err();
    assert_eq!(status.code(), Code::Unauthenticated);
    let status = catalog
        .unary(with_token(UnaryRequest::default(), Some("caller-supplied")))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Internal);
    // Without service authentication the handle sends nothing.
    let mut open = EchoServiceClient::new(
        resource().with_service_tokens(TokenSource::disabled().receiver("catalog").unwrap()),
    );
    assert_eq!(
        open.unary(UnaryRequest::default())
            .await
            .unwrap_err()
            .code(),
        Code::Unauthenticated
    );
    drop(server.drain(Duration::from_secs(3)).await);
}

/// Whether this projection retains bearer authentication, which then admits
/// the calls a process does not verify as service tokens.
#[allow(
    unused_mut,
    unused_assignments,
    clippy::let_and_return,
    reason = "a projection without bearer authentication keeps the default"
)]
fn bearer_retained() -> bool {
    let mut retained = false;
    // template:begin authn:grpc-service-auth-test-bearer-retained
    retained = true;
    // template:end authn:grpc-service-auth-test-bearer-retained
    retained
}

#[tokio::test]
async fn without_a_verifier_bearer_or_nothing_admits_calls_and_no_token_reaches_the_handler() {
    // template:begin authn:grpc-service-auth-test-without-verifier-bearer
    // With bearer authentication retained, a listener that verifies no
    // service token needs its bearer verifier.
    let readiness = readiness();
    let refused = infra_grpc::router(
        echo_services(&Echo::default()),
        readiness.reader(),
        None,
        limits(),
    );
    assert_eq!(refused.err(), Some(Error::UnauthenticatedBusinessCalls));
    // template:end authn:grpc-service-auth-test-without-verifier-bearer
    if bearer_retained() {
        return;
    }
    // Development only: no token is needed, and one sent never reaches the handler.
    let echo = Echo::default();
    let (server, address) = serve(echo_services(&echo)).await;
    let token = Keys::token(&Keys::new().orders, "orders", "catalog", 0);
    assert_eq!(unary(address, Some(&token)).await.unwrap(), "hello");
    assert_eq!(unary(address, None).await.unwrap(), "hello");
    assert_eq!(echo.seen(), vec![(None, false), (None, false)]);
    drop(server.drain(Duration::from_secs(3)).await);
}

#[test]
fn a_router_refuses_to_build_while_a_method_has_no_rule() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let keys = Keys::new();
        let build = |services: Services| {
            let readiness = readiness();
            async move {
                infra_grpc::router(
                    services,
                    readiness.reader(),
                    // template:begin authn:grpc-service-auth-test-refusal-bearer-argument
                    None,
                    // template:end authn:grpc-service-auth-test-refusal-bearer-argument
                    limits(),
                )
                .map(|_| ())
            }
        };
        let mut services = Services::new();
        services
            .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
            .unwrap();
        services
            .add(EchoServiceServer::new(Echo::default()))
            .unwrap();
        services.authenticate_callers(keys.verifier());
        assert_eq!(
            build(services).await,
            Err(Error::MissingCallerRule(format!("{ECHO}/Unary"))),
            "a verifier without any rule"
        );
        let mut services = Services::new();
        services
            .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
            .unwrap();
        services
            .add(EchoServiceServer::new(Echo::default()))
            .unwrap();
        services
            .allow_callers(&format!("{ECHO}/Unary"), &["orders"])
            .unwrap();
        assert_eq!(
            build(services).await,
            Err(Error::MissingCallerRule(format!("{ECHO}/ClientStream"))),
            "a declared table must be complete even before tokens are verified"
        );
        // Reflection, once added, needs its two rules as well.
        let echo = Echo::default();
        let mut services = echo_services(&echo);
        services.add_reflection().unwrap();
        assert_eq!(
            build(services).await,
            Err(Error::MissingCallerRule(
                "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo".to_owned()
            ))
        );
        let mut services = echo_services(&echo);
        services.add_reflection().unwrap();
        for path in [
            "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
            "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
        ] {
            services.allow_callers(path, &[]).unwrap();
        }
        services.authenticate_callers(keys.verifier());
        assert_eq!(build(services).await, Ok(()));
    });
}

#[test]
fn caller_rules_name_registered_methods_once_with_valid_callers() {
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    assert_eq!(
        services.allow_callers(&format!("{ECHO}/Unary"), &["orders"]),
        Err(Error::UnregisteredCallerRule),
        "before the service is added"
    );
    services
        .add(EchoServiceServer::new(Echo::default()))
        .unwrap();
    for path in [
        "example.v1.EchoService/Unary",
        "/example.v1.EchoService/unary",
        "/example.v1.EchoService/Unary/extra",
        "/example.v1.Other/Unary",
        "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    ] {
        assert_eq!(
            services.allow_callers(path, &["orders"]),
            Err(Error::UnregisteredCallerRule),
            "{path}"
        );
    }
    for callers in [&["Orders"][..], &["orders", "orders"], &["a b"]] {
        assert_eq!(
            services.allow_callers(&format!("{ECHO}/Unary"), callers),
            Err(Error::InvalidCallerRule),
            "{callers:?}"
        );
    }
    services
        .allow_callers(&format!("{ECHO}/Unary"), &["orders", "billing"])
        .unwrap();
    assert_eq!(
        services.allow_callers(&format!("{ECHO}/Unary"), &["orders"]),
        Err(Error::DuplicateCallerRule)
    );
}

/// Every event field and span field, and every exported span, as text.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

struct Fields<'a>(&'a mut String);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = write!(self.0, "{}={value:?};", field.name());
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        attributes.record(&mut Fields(&mut self.0.lock().unwrap()));
    }

    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        event.record(&mut Fields(&mut self.0.lock().unwrap()));
    }
}

#[tokio::test]
async fn token_text_never_reaches_logs_or_spans_and_the_span_names_the_caller() {
    let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(capture.clone())
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    // A second live dispatcher keeps callsite interest per-dispatch, so tests
    // that run alongside without a subscriber cannot disable these callsites.
    let _registration_anchor = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(subscriber);

    let keys = Keys::new();
    let echo = Echo::default();
    let mut services = echo_services(&echo);
    services.authenticate_callers(keys.verifier());
    let (server, address) = serve(services).await;
    let valid = Keys::token(&keys.orders, "orders", "catalog", 0);
    let foreign = Keys::token(&keys.billing, "orders", "catalog", 0);
    unary(address, Some(&valid)).await.unwrap();
    unary(address, Some(&foreign)).await.unwrap_err();
    client(address)
        .server_stream(with_token(ServerStreamRequest::default(), Some(&valid)))
        .await
        .unwrap_err();
    drop(server.drain(Duration::from_secs(3)).await);

    let logged = capture.0.lock().unwrap().clone();
    assert!(
        logged.contains("service_auth_caller_not_admitted"),
        "{logged}"
    );
    let spans = exporter.get_finished_spans().unwrap();
    let rendered = format!("{spans:?}");
    for token in [&valid, &foreign] {
        // Every part of the token, signature included, stays out.
        for part in token.split('.') {
            assert!(!logged.contains(part), "log carries token text: {logged}");
            assert!(
                !rendered.contains(part),
                "span carries token text: {rendered}"
            );
        }
    }
    let admitted = spans.iter().find(|span| {
        span.attributes.iter().any(|attribute| {
            attribute.key.as_str() == "peer.service" && attribute.value.as_str() == "orders"
        })
    });
    assert!(
        admitted.is_some(),
        "the admitted call's span names its caller: {rendered}"
    );
}
