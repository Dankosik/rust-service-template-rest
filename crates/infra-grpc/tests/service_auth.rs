#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration fixtures fail closed with their local setup context"
)]
#![allow(
    clippy::panic,
    reason = "no panic! call in every projection of this integration fixture"
)]

use std::{
    fmt::Write as _,
    net::SocketAddr,
    num::{NonZeroU32, NonZeroUsize},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use grpc_contracts::example::v1::{
    BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
    ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
    echo_service_client::EchoServiceClient,
    echo_service_server::{EchoService, EchoServiceServer},
};
use health::{Readiness, RefreshPolicy};
use infra_grpc::{
    Authentication, Caller, Limits, RequestExt as _, ServerOptions, ServiceRegistration, Services,
    TlsRequiredError, server_options,
};
use infra_http_transport::{BoundListener, ConnectionLimits, Drained, Server, TlsOptions};
use infra_outbound_grpc::{Client, ClientSecurity, ClientTlsMaterial, transport::PreparedCall};
use infra_serviceauth::{
    JwtType, ServiceName, TokenSource, TrustRegistry, Verifier,
    test_support::{TestKey, has_cached_authorization, now_secs, registry},
};
use opentelemetry::trace::TracerProvider as _;
use parking_lot::Mutex;
use prost::Message as _;
use tonic::{Code, Request, Response, Status};
use tracing_subscriber::layer::SubscriberExt as _;

const TYPE: &str = "example-svc+jwt";
const ECHO: &str = "/example.v1.EchoService";

#[derive(Clone, Debug, Eq, PartialEq)]
struct Seen {
    caller: Option<String>,
    authorization_present: bool,
}

#[derive(Clone, Default)]
struct Echo {
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Echo {
    fn observe<T>(&self, request: &Request<T>) {
        let caller = request
            .extensions()
            .get::<Caller>()
            .map(|caller| caller.name().as_str().to_owned());
        let authorization_present = request.metadata().get("authorization").is_some();
        self.seen.lock().push(Seen {
            caller,
            authorization_present,
        });
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().clone()
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
        if request.get_ref().message == "verified-caller" {
            return Ok(Response::new(UnaryResponse {
                message: request.caller()?.name().as_str().to_owned(),
            }));
        }
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
            ServiceName::parse("catalog").unwrap(),
            JwtType::parse(TYPE).unwrap(),
            TrustRegistry::parse(&registry(&[
                ("orders", &[&self.orders]),
                ("billing", &[&self.billing]),
            ]))
            .unwrap(),
        )
    }

    fn token(key: &TestKey, issuer: &str, audience: &str, age: u64) -> String {
        let issued_at = now_secs() - age;
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
        max_admission_wait: Duration::from_secs(1),
    }
}

fn readiness() -> health::ReadinessControl {
    Readiness::new(
        Vec::new(),
        RefreshPolicy::try_new(
            Duration::from_mins(1),
            Duration::from_secs(1),
            NonZeroU32::MIN,
        )
        .unwrap(),
    )
    .control()
}

fn described_echo(echo: &Echo) -> Services {
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    services.add(EchoServiceServer::new(echo.clone())).unwrap();
    services
}

fn register_echo<'a>(services: &'a mut Services, echo: &Echo) -> ServiceRegistration<'a> {
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    let mut registration = services.add(EchoServiceServer::new(echo.clone())).unwrap();
    for method in ["Unary", "ClientStream", "BidiStream"] {
        registration.allow_callers(method, &["orders"]).unwrap();
    }
    registration
        .allow_callers("ServerStream", &["billing"])
        .unwrap();
    registration
}

fn echo_services(echo: &Echo) -> Services {
    let mut services = Services::new();
    register_echo(&mut services, echo);
    services
}

fn serve(services: Services, authentication: Authentication) -> (Server, SocketAddr) {
    let app = services
        .build(
            readiness().reader(),
            authentication,
            ServerOptions {
                limits: limits(),
                trust_trace_context: false,
            },
        )
        .unwrap();
    let listener = BoundListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let options = server_options(ConnectionLimits {
        max_connections: NonZeroU32::new(64),
        max_connection_age: None,
    });
    let material = certificate();
    let config = infra_grpc::server_tls_config(infra_grpc::ServerTlsMaterial {
        certificate_pem: &material.cert_pem,
        private_key_pem: &material.key_pem.clone().into(),
        client_ca_pem: None,
    })
    .unwrap();
    let server = app
        .serve(
            listener,
            options,
            Some(TlsOptions {
                config,
                handshake_timeout: Duration::from_secs(5),
            }),
        )
        .unwrap();
    let address = server.local_addr();
    (server, address)
}

fn certificate() -> &'static tls_test_support::TlsMaterial {
    static MATERIAL: std::sync::OnceLock<tls_test_support::TlsMaterial> =
        std::sync::OnceLock::new();
    MATERIAL.get_or_init(|| tls_test_support::TlsMaterial::new("127.0.0.1"))
}

fn resource(address: SocketAddr) -> Client {
    resource_with_limit(address, None)
}

fn resource_with_limit(address: SocketAddr, limit: Option<NonZeroUsize>) -> Client {
    Client::builder(
        &format!("https://{address}"),
        ClientSecurity::Tls(ClientTlsMaterial {
            ca_certificate_pem: Some(&certificate().root_pem),
            identity: None,
        }),
        Duration::from_secs(10),
    )
    .opening_limit(limit)
    .build()
    .unwrap()
}

fn client(address: SocketAddr) -> EchoServiceClient<Client> {
    EchoServiceClient::new(resource(address))
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

async fn raw_status(address: SocketAddr, path: &str, token: &str) -> String {
    use bytes::{BufMut as _, BytesMut};
    use tower::ServiceExt as _;

    let message = UnaryRequest::default();
    let len = message.encoded_len();
    let mut frame = BytesMut::with_capacity(5 + len);
    frame.put_u8(0);
    frame.put_u32(u32::try_from(len).unwrap());
    message.encode(&mut frame).unwrap();
    let request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {token}"))
        .body(tonic::body::Body::new(http_body_util::Full::new(
            frame.freeze(),
        )))
        .unwrap();
    let response = resource(address).oneshot(request).await.unwrap();
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
        .map_or_default(|info| info.reason)
}

#[test]
fn a_handler_requiring_a_caller_reports_a_missing_identity_as_internal() {
    let status = Request::new(()).caller().unwrap_err();
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(reason(&status), "INTERNAL_ERROR");
}

#[tokio::test]
async fn a_valid_token_names_its_caller_and_every_contract_breach_is_refused() {
    let keys = Keys::new();
    let echo = Echo::default();
    let mut services = Services::new();
    #[allow(
        unused_mut,
        unused_variables,
        reason = "the bearer profile adds explicit empty scope rules"
    )]
    let mut registration = register_echo(&mut services, &echo);
    // template:begin authn:grpc-service-auth-test-empty-scope-rules
    for method in ["Unary", "ServerStream"] {
        registration.require_scopes(method, &[]).unwrap();
    }
    // template:end authn:grpc-service-auth-test-empty-scope-rules
    let (server, address) = serve(services, Authentication::ServiceTokens(keys.verifier()));

    let valid = Keys::token(&keys.orders, "orders", "catalog", 0);
    let admitted = client(address)
        .unary(with_token(
            UnaryRequest {
                message: "verified-caller".to_owned(),
            },
            Some(&valid),
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(admitted.message, "orders");
    assert_eq!(
        echo.seen(),
        [Seen {
            caller: Some("orders".to_owned()),
            authorization_present: false,
        }]
    );

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

    let status = client(address)
        .server_stream(with_token(ServerStreamRequest::default(), Some(&valid)))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::PermissionDenied);
    assert_eq!(reason(&status), "FORBIDDEN");
    let status = raw_status(address, "/example.v1.EchoService/Missing", &valid).await;
    assert_eq!(status, "7", "a path outside the table is PERMISSION_DENIED");
    assert_eq!(echo.seen().len(), 1);
    assert_eq!(
        server.drain(Duration::from_secs(3)).await.unwrap(),
        Drained::Complete
    );
}

#[tokio::test]
async fn the_outbound_binding_signs_for_its_receiver_and_refuses_a_caller_authorization() {
    let keys = Keys::new();
    let echo = Echo::default();
    let (server, address) = serve(
        echo_services(&echo),
        Authentication::ServiceTokens(keys.verifier()),
    );
    let source = TokenSource::try_new(keys.orders.signing_options("orders", TYPE)).unwrap();
    let origin: http::Uri = format!("https://{address}").parse().unwrap();
    let resource = resource_with_limit(address, Some(NonZeroUsize::MIN));
    let bound = |source: &TokenSource, receiver: &str| {
        let tokens = source
            .receiver(receiver.parse().unwrap(), origin.clone())
            .unwrap();
        EchoServiceClient::new(resource.with_service_tokens(tokens).unwrap())
    };
    let catalog_tokens = source
        .receiver("catalog".parse().unwrap(), origin.clone())
        .unwrap();
    let mut catalog = EchoServiceClient::new(
        resource
            .with_service_tokens(catalog_tokens.clone())
            .unwrap(),
    );
    assert!(!has_cached_authorization(&catalog_tokens));
    let held =
        PreparedCall::prepare(&resource, http::Request::new(tonic::body::Body::empty())).unwrap();
    let status = tokio::time::timeout(
        Duration::from_secs(3),
        catalog.unary(UnaryRequest::default()),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(status.code(), Code::ResourceExhausted);
    let status = catalog
        .unary(with_token(UnaryRequest::default(), Some("caller-supplied")))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::ResourceExhausted);
    assert_eq!(echo.seen(), Vec::<Seen>::new());
    assert!(!has_cached_authorization(&catalog_tokens));
    drop(held);

    for _ in 0..2 {
        let response = catalog
            .unary(UnaryRequest {
                message: "signed".to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(response.into_inner().message, "signed");
    }
    assert!(has_cached_authorization(&catalog_tokens));
    assert_eq!(
        echo.seen(),
        vec![
            Seen {
                caller: Some("orders".to_owned()),
                authorization_present: false,
            };
            2
        ]
    );
    let mut elsewhere = bound(&source, "billing");
    let status = elsewhere.unary(UnaryRequest::default()).await.unwrap_err();
    assert_eq!(status.code(), Code::Unauthenticated);
    assert_eq!(status.message(), "authentication failed");
    assert_eq!(reason(&status), "UNAUTHORIZED");
    let status = catalog
        .unary(with_token(UnaryRequest::default(), Some("caller-supplied")))
        .await
        .unwrap_err();
    assert_eq!(status.code(), Code::Internal);
    assert_eq!(
        status.message(),
        "authorization conflicts with the service token"
    );
    assert_eq!(reason(&status), "");
    let mut open = bound(&TokenSource::disabled(), "catalog");
    let status = open.unary(UnaryRequest::default()).await.unwrap_err();
    assert_eq!(status.code(), Code::Unauthenticated);
    assert_eq!(status.message(), "authentication failed");
    assert_eq!(reason(&status), "UNAUTHORIZED");
    assert_eq!(echo.seen().len(), 2, "no refused call reached the handler");
    assert_eq!(
        server.drain(Duration::from_secs(3)).await.unwrap(),
        Drained::Complete
    );
}

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
    let options = ServerOptions {
        limits: limits(),
        trust_trace_context: false,
    };
    let refused =
        echo_services(&Echo::default()).build(readiness().reader(), Authentication::None, options);
    assert_eq!(
        refused.unwrap_err().to_string(),
        "gRPC caller rules require service token authentication"
    );
    // template:begin oidc-jwt:grpc-service-auth-test-caller-rules-bearer
    let verifier = infra_bearerauthn::test_support::JwtFixture::shared().verifier();
    let refused = echo_services(&Echo::default()).build(
        readiness().reader(),
        Authentication::Bearer(verifier),
        options,
    );
    assert_eq!(
        refused.unwrap_err().to_string(),
        "gRPC caller rules require service token authentication"
    );
    // template:end oidc-jwt:grpc-service-auth-test-caller-rules-bearer
    // template:begin authn:grpc-service-auth-test-without-verifier-bearer
    let readiness = readiness();
    let refused =
        described_echo(&Echo::default()).build(readiness.reader(), Authentication::None, options);
    assert_eq!(
        refused.unwrap_err().to_string(),
        infra_grpc::Error::unauthenticated_business_calls().to_string()
    );
    // template:end authn:grpc-service-auth-test-without-verifier-bearer
    if bearer_retained() {
        return;
    }
    let echo = Echo::default();
    let (server, address) = serve(described_echo(&echo), Authentication::None);
    let token = Keys::token(&Keys::new().orders, "orders", "catalog", 0);
    assert_eq!(unary(address, Some(&token)).await.unwrap(), "hello");
    assert_eq!(unary(address, None).await.unwrap(), "hello");
    assert_eq!(
        echo.seen(),
        vec![
            Seen {
                caller: None,
                authorization_present: false,
            };
            2
        ]
    );
    assert_eq!(
        server.drain(Duration::from_secs(3)).await.unwrap(),
        Drained::Complete
    );
}

#[tokio::test]
async fn service_token_authentication_requires_a_tls_listener() {
    let mut services = echo_services(&Echo::default());
    services.add_listener_layer(tower::layer::util::Identity::new());
    let app = services
        .build(
            readiness().reader(),
            Authentication::ServiceTokens(Keys::new().verifier()),
            ServerOptions {
                limits: limits(),
                trust_trace_context: false,
            },
        )
        .unwrap();
    let listener = BoundListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let options = server_options(ConnectionLimits {
        max_connections: NonZeroU32::new(1),
        max_connection_age: None,
    });
    std::assert_matches!(
        app.serve(listener, options, None).err(),
        Some(TlsRequiredError)
    );
}

// template:begin authn:grpc-service-auth-test-scopes-refused
#[tokio::test]
async fn required_scopes_and_delegation_cannot_be_dropped_for_service_tokens() {
    let keys = Keys::new();
    let readiness = readiness();
    for delegate in [false, true] {
        let mut services = Services::new();
        let mut registration = register_echo(&mut services, &Echo::default());
        if delegate {
            registration
                .require_scopes_for_delegation("Unary", &[])
                .unwrap();
        } else {
            registration
                .require_scopes("Unary", &["echo.read"])
                .unwrap();
        }
        let refused = services.build(
            readiness.reader(),
            Authentication::ServiceTokens(keys.verifier()),
            ServerOptions {
                limits: limits(),
                trust_trace_context: false,
            },
        );
        assert_eq!(
            refused.unwrap_err().to_string(),
            "gRPC required scopes or bearer delegation cannot be enforced when service tokens replace bearer authentication"
        );
    }
}
// template:end authn:grpc-service-auth-test-scopes-refused

#[tokio::test]
async fn a_router_refuses_to_build_while_a_method_has_no_rule() {
    let keys = Keys::new();
    let build = |services: Services, authentication| {
        services
            .build(
                readiness().reader(),
                authentication,
                ServerOptions {
                    limits: limits(),
                    trust_trace_context: false,
                },
            )
            .map(|_| ())
    };
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    services
        .add(EchoServiceServer::new(Echo::default()))
        .unwrap();
    assert_eq!(
        build(services, Authentication::ServiceTokens(keys.verifier()))
            .unwrap_err()
            .to_string(),
        format!("gRPC method {ECHO}/BidiStream has no caller rule"),
        "a verifier without any rule"
    );
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    services
        .add(EchoServiceServer::new(Echo::default()))
        .unwrap()
        .allow_callers("BidiStream", &["orders"])
        .unwrap();
    assert_eq!(
        build(services, Authentication::ServiceTokens(keys.verifier()))
            .unwrap_err()
            .to_string(),
        format!("gRPC method {ECHO}/ClientStream has no caller rule"),
        "every method requires an explicit rule"
    );
    let echo = Echo::default();
    let mut services = echo_services(&echo);
    services.add_reflection().unwrap();
    assert_eq!(
        build(services, Authentication::ServiceTokens(keys.verifier()))
            .unwrap_err()
            .to_string(),
        "gRPC method /grpc.reflection.v1.ServerReflection/ServerReflectionInfo has no caller rule"
    );
    let mut services = echo_services(&echo);
    services
        .add_reflection()
        .unwrap()
        .allow_callers(&[])
        .unwrap();
    build(services, Authentication::ServiceTokens(keys.verifier())).unwrap();
}

#[test]
fn caller_rules_name_registered_methods_once_with_valid_callers() {
    let mut services = Services::new();
    services
        .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
        .unwrap();
    let mut registration = services
        .add(EchoServiceServer::new(Echo::default()))
        .unwrap();
    for method in [
        "unary",
        "Missing",
        "Unary/extra",
        "/example.v1.EchoService/Unary",
    ] {
        let expected_path = format!("{ECHO}/{method}");
        assert_eq!(
            registration
                .allow_callers(method, &["orders"])
                .unwrap_err()
                .to_string(),
            format!(
                "gRPC caller rule for {expected_path} must name a described method of a registered service"
            ),
            "{method}"
        );
    }
    let unary = format!("{ECHO}/Unary");
    for callers in [&["Orders"], &["a b"]] {
        let error = registration.allow_callers("Unary", callers).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("gRPC caller rule for {unary} is invalid"),
            "{callers:?}"
        );
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<infra_serviceauth::InvalidServiceName>(),
            "{callers:?}"
        );
    }
    assert_eq!(
        registration
            .allow_callers("Unary", &["orders", "orders"])
            .unwrap_err()
            .to_string(),
        format!("gRPC caller rule for {unary} names the same caller twice")
    );
    registration
        .allow_callers("Unary", &["orders", "billing"])
        .unwrap();
    assert_eq!(
        registration
            .allow_callers("Unary", &["orders"])
            .unwrap_err()
            .to_string(),
        format!("gRPC caller rule is declared twice for {unary}")
    );
}

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
        attributes.record(&mut Fields(&mut self.0.lock()));
    }

    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        event.record(&mut Fields(&mut self.0.lock()));
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
    let _registration_anchor = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _guard = tracing::subscriber::set_default(subscriber);

    let keys = Keys::new();
    let echo = Echo::default();
    let (server, address) = serve(
        echo_services(&echo),
        Authentication::ServiceTokens(keys.verifier()),
    );
    let valid = Keys::token(&keys.orders, "orders", "catalog", 0);
    let foreign = Keys::token(&keys.billing, "orders", "catalog", 0);
    unary(address, Some(&valid)).await.unwrap();
    unary(address, Some(&foreign)).await.unwrap_err();
    client(address)
        .server_stream(with_token(ServerStreamRequest::default(), Some(&valid)))
        .await
        .unwrap_err();
    assert_eq!(
        server.drain(Duration::from_secs(3)).await.unwrap(),
        Drained::Complete
    );

    let logged = capture.0.lock().clone();
    assert!(
        logged.contains("service_auth_caller_not_admitted"),
        "{logged}"
    );
    let spans = exporter.get_finished_spans().unwrap();
    let rendered = format!("{spans:?}");
    for token in [&valid, &foreign] {
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
