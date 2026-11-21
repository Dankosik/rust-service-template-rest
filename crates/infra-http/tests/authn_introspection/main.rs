#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a broken fixture or setup fails the test"
)]

// template:begin http-idempotency-mounted:http-introspection-idempotency-module
mod idempotency;
// template:end http-idempotency-mounted:http-introspection-idempotency-module

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::ServiceExt as _;
use axum::body::Bytes;
use axum::extract::{Extension, State};
use axum::http::header::{ALLOW, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum_test::{TestResponse, TestServer};
use health::{Readiness, RefreshPolicy};
use infra_bearerauthn::test_support::{
    FixtureTransport, prepare_introspection_with_fixture, verify_fixture_authorization,
};
use infra_bearerauthn::{
    EndpointUrl, IntrospectionCacheOptions, IntrospectionOptions, IssuerUrl, Verifier,
};
use infra_http::{
    Application, Authentication, DelegationCredential, HardenOptions, HttpContract,
    REQUEST_ID_HEADER, VerifiedDelegation, VerifiedPrincipal,
};
use infra_http_transport::{BoundListener, ConnectionLimits, Http1Options, Server, TlsOptions};
use observe_test_support::CumulativeMetrics;
use secrecy::SecretString;
use serde::Deserialize;
use serde_json::{Value, json};
use tls_test_support::TlsMaterial;
use tokio_util::sync::{CancellationToken, DropGuard};

const BUDGET: Duration = Duration::from_secs(5);
const WAIT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 2 * 1_048_576;

const FIXTURE_HOST: &str = "authn.fixture.test";
const ISSUER: &str = "https://issuer.example";
const AUDIENCE: &str = "service";
const ALICE: &str = "alice-token";
const NO_SCOPE: &str = "no-scope-token";
const SCOPE_A_AND_B: &str = "scope-a-and-b-token";
const SCOPE_C: &str = "scope-c-only-token";
const SCOPE_A_ONLY: &str = "scope-a-only-token";
const HELD_PROVIDER: &str = "held-provider-token";

struct Provider {
    server: Server,
    material: TlsMaterial,
    requests: Arc<AtomicUsize>,
    held: Arc<tokio::sync::Notify>,
    cancel: CancellationToken,
    _cancel_guard: DropGuard,
}

impl Provider {
    #[expect(
        clippy::disallowed_methods,
        reason = "local non-shipped identity provider fixture"
    )]
    fn start() -> Self {
        let material = TlsMaterial::new(FIXTURE_HOST);
        let requests = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(tokio::sync::Notify::new());
        let cancel = CancellationToken::new();
        let cancel_guard = cancel.clone().drop_guard();
        let router = axum::Router::new().route(
            "/introspect",
            axum::routing::post({
                let requests = Arc::clone(&requests);
                let held = Arc::clone(&held);
                let cancel = cancel.clone();
                move |body: Bytes| {
                    let requests = Arc::clone(&requests);
                    let held = Arc::clone(&held);
                    let cancel = cancel.clone();
                    async move {
                        let Some(token) = url::form_urlencoded::parse(&body)
                            .find(|(name, _)| name == "token")
                            .map(|(_, value)| value.into_owned())
                        else {
                            return StatusCode::BAD_REQUEST.into_response();
                        };
                        requests.fetch_add(1, Ordering::Relaxed);
                        if token == HELD_PROVIDER {
                            held.notify_one();
                            cancel.cancelled().await;
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        (
                            [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
                            introspection(&token),
                        )
                            .into_response()
                    }
                }
            }),
        );
        let listener =
            BoundListener::bind((Ipv4Addr::LOCALHOST, 0).into()).expect("the provider listens");
        let server = listener.serve_tls(
            router,
            Http1Options {
                header_read_timeout: Duration::from_secs(5),
                max_read_buffer_bytes: 16 * 1024,
                connections: ConnectionLimits {
                    max_connections: NonZeroU32::new(16),
                    max_connection_age: None,
                },
            },
            TlsOptions {
                config: Arc::new(material.server_config_tls13()),
                handshake_timeout: Duration::from_secs(5),
            },
        );
        Self {
            server,
            material,
            requests,
            held,
            cancel,
            _cancel_guard: cancel_guard,
        }
    }

    fn verifier(&self) -> Verifier {
        self.verifier_with_cache(None)
    }

    fn verifier_with_cache(&self, cache: Option<IntrospectionCacheOptions>) -> Verifier {
        let transport = FixtureTransport::try_new(
            FIXTURE_HOST,
            self.server.local_addr(),
            &self.material.root,
            self.cancel.child_token(),
        )
        .expect("the fixture transport");
        prepare_introspection_with_fixture(
            IntrospectionOptions::try_new(
                &IssuerUrl::parse(ISSUER).expect("fixture issuer URL"),
                vec![AUDIENCE.to_owned()],
                EndpointUrl::parse(&format!("https://{FIXTURE_HOST}/introspect"))
                    .expect("fixture endpoint URL"),
                "fixture-client".to_owned(),
                SecretString::from("fixture-secret"),
                const { NonZeroUsize::new(16).expect("16 is nonzero") },
                cache,
            )
            .expect("the introspection options"),
            transport,
        )
    }

    async fn stop(self) {
        self.cancel.cancel();
        let _ = self.server.drain(WAIT).await;
    }
}

fn introspection(token: &str) -> String {
    let (subject, scope) = match token {
        ALICE => ("alice", "widgets:write"),
        NO_SCOPE => ("scope-less", ""),
        SCOPE_A_AND_B => ("scoped", "scope-a scope-b"),
        SCOPE_C => ("scoped", "scope-c"),
        SCOPE_A_ONLY => ("scoped", "scope-a"),
        _ => return json!({"active": false}).to_string(),
    };
    json!({
        "active": true,
        "iss": ISSUER,
        "aud": AUDIENCE,
        "exp": 2_147_483_647,
        "sub": subject,
        "client_id": "widgets-app",
        "scope": scope,
        "roles": ["widget-author"],
        "tenant": {"id": "tenant-42"},
    })
    .to_string()
}

fn options(budget: Duration) -> HardenOptions {
    HardenOptions {
        max_body_bytes: MAX_BODY_BYTES,
        request_timeout: budget,
        max_in_flight: NonZeroU32::new(16),
        admission_wait: budget,
        log_health_probes: false,
        trust_trace_context: false,
        draining: None,
    }
}

fn bearer_document() -> utoipa::openapi::OpenApi {
    serde_json::from_value(json!({
        "openapi": "3.1.0",
        "info": {"title": "authorization fixture", "version": "1"},
        "paths": {},
        "components": {"securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}}},
        "security": [{"bearerAuth": []}]
    }))
    .expect("the inherited bearer policy")
}

fn readiness_reader() -> health::ReadinessReader {
    Readiness::new(
        Vec::new(),
        RefreshPolicy::try_new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            NonZeroU32::MIN,
        )
        .expect("probe policy"),
    )
    .control()
    .reader()
}

#[track_caller]
fn problem_body(response: &TestResponse, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status_code(), status, "{}", response.text());
    assert_eq!(
        response.maybe_content_type().as_deref(),
        Some("application/problem+json")
    );
    let body: Value = serde_json::from_slice(response.as_bytes()).expect("a JSON Problem");
    assert_eq!(body["code"], code, "{body}");
    body
}

#[track_caller]
fn problem(response: &TestResponse, status: StatusCode, code: &str, request_id: &str) -> Value {
    let body = problem_body(response, status, code);
    assert_eq!(
        response.maybe_header(REQUEST_ID_HEADER),
        Some(HeaderValue::from_str(request_id).expect("a request id"))
    );
    assert_eq!(body["request_id"], request_id, "{body}");
    body
}

async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not complete within {WAIT:?}"))
}

#[utoipa::path(
    get,
    path = "/_test/authorization",
    operation_id = "authorizationWithoutIdempotency",
    security(("bearerAuth" = ["widgets:write"])),
    responses(
        (status = 200, description = "verified", content_type = "text/plain", body = String),
        infra_http::authn::ProtectedOperationProblemResponses,
    )
)]
async fn authorized_without_idempotency(
    VerifiedPrincipal(principal): VerifiedPrincipal,
    credential: Option<Extension<DelegationCredential>>,
    headers: axum::http::HeaderMap,
) -> Response {
    #[derive(Deserialize)]
    struct ApplicationClaims {
        roles: Vec<String>,
        tenant: Value,
        scope: String,
    }
    assert!(!headers.contains_key(axum::http::header::AUTHORIZATION));
    let claims = principal
        .claims::<ApplicationClaims>()
        .expect("verified custom claims");
    assert_eq!(claims.roles, ["widget-author"]);
    assert_eq!(claims.tenant, json!({"id": "tenant-42"}));
    assert_eq!(claims.scope, "widgets:write");
    assert_eq!(principal.scopes(), ["widgets:write"]);
    assert!(principal.claims::<BTreeMap<String, u64>>().is_err());
    assert!(credential.is_none());
    assert!(principal.actor().is_none());
    (
        StatusCode::OK,
        principal.subject().unwrap_or_default().to_owned(),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/_test/scoped-alternatives",
    operation_id = "scopedAlternatives",
    extensions(("x-bearer-delegation" = json!(true))),
    security(
        ("bearerAuth" = ["scope-a", "scope-b"]),
        ("bearerAuth" = ["scope-c"]),
    ),
    responses(
        (status = 200, description = "granted", content_type = "text/plain", body = String),
        infra_http::authn::ProtectedOperationProblemResponses,
    )
)]
async fn scoped_alternatives(
    State(calls): State<Arc<Mutex<Vec<DelegationCredential>>>>,
    VerifiedDelegation(credential): VerifiedDelegation,
    headers: axum::http::HeaderMap,
) -> &'static str {
    assert!(!headers.contains_key(axum::http::header::AUTHORIZATION));
    calls.lock().unwrap().push(credential);
    "granted"
}

#[utoipa::path(head, path = "/_test/authorization", security(), responses((status = 204, description = "public HEAD")))]
async fn public_head(credential: Option<Extension<DelegationCredential>>) -> StatusCode {
    assert!(credential.is_none());
    StatusCode::NO_CONTENT
}

#[utoipa::path(get, path = "/_test/inherited", responses((status = 200, description = "protected")))]
async fn inherited_get() -> &'static str {
    "protected"
}

fn timeout_router(
    verifier: Verifier,
    timeout: Duration,
    entered: Arc<tokio::sync::Notify>,
) -> Application {
    #[utoipa::path(get, path = "/hold", security(), responses((status = 204)))]
    async fn hold(State(entered): State<Arc<tokio::sync::Notify>>) -> StatusCode {
        entered.notify_one();
        tokio::time::sleep(Duration::from_millis(150)).await;
        StatusCode::NO_CONTENT
    }
    let contract = HttpContract::application(bearer_document())
        .unwrap()
        .merge(infra_http::http_route!(inherited_get).unwrap())
        .unwrap()
        .merge(infra_http::http_route!(hold).unwrap().with_state(entered))
        .unwrap();
    let mut options = options(timeout);
    options.max_in_flight = Some(NonZeroU32::MIN);
    contract
        .build((), Authentication::Bearer(verifier), &options)
        .unwrap()
}

#[tokio::test]
async fn final_document_controls_head_policy_and_preserves_native_fallbacks() {
    let provider = Provider::start();
    let app = HttpContract::application(bearer_document())
        .unwrap()
        .merge(infra_http::http_route!(authorized_without_idempotency).unwrap())
        .unwrap()
        .merge(infra_http::http_route!(public_head).unwrap())
        .unwrap()
        .merge(infra_http::http_route!(inherited_get).unwrap())
        .unwrap()
        .with_probes("/")
        .unwrap()
        .build(
            readiness_reader(),
            Authentication::Bearer(provider.verifier()),
            &options(BUDGET),
        )
        .unwrap();
    let server = TestServer::new(app.into_make_service());
    let explicit = server
        .method(Method::HEAD, "/_test/authorization")
        .authorization("Bearer broken token")
        .await;
    assert_eq!(explicit.status_code(), StatusCode::NO_CONTENT);
    let implicit = server.method(Method::HEAD, "/_test/inherited").await;
    assert_eq!(implicit.status_code(), StatusCode::UNAUTHORIZED);
    assert_eq!(implicit.header(WWW_AUTHENTICATE), "Bearer realm=\"api\"");
    assert_eq!(implicit.header(CONTENT_TYPE), "application/problem+json");
    let accepted = server
        .method(Method::HEAD, "/_test/inherited")
        .authorization(format!("Bearer {ALICE}"))
        .await;
    assert_eq!(accepted.status_code(), StatusCode::OK);
    let probe = server
        .get("/health/live")
        .authorization("Bearer broken token")
        .await;
    assert_eq!(probe.status_code(), StatusCode::OK);
    for (uri, status, code) in [
        ("/_test/absent", StatusCode::NOT_FOUND, "not_found"),
        (
            "/_test/inherited",
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        ),
    ] {
        let response = server.post(uri).await;
        problem_body(&response, status, code);
        if status == StatusCode::METHOD_NOT_ALLOWED {
            assert_eq!(response.header(ALLOW), "GET,HEAD");
        }
    }
    provider.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn cached_verification_succeeds_in_the_final_hundred_milliseconds() {
    let provider = Provider::start();
    let verifier = provider.verifier_with_cache(Some(
        IntrospectionCacheOptions::try_new(16, Duration::from_secs(30)).unwrap(),
    ));
    let authorization = format!("Bearer {ALICE}");
    verify_fixture_authorization(&verifier, authorization.as_bytes())
        .await
        .unwrap();
    assert_eq!(provider.requests.load(Ordering::Relaxed), 1);
    tokio::time::pause();
    let entered = Arc::new(tokio::sync::Notify::new());
    let app = timeout_router(verifier, Duration::from_millis(200), Arc::clone(&entered));
    let server = TestServer::new(app.into_make_service());
    let holder = server.get("/hold");
    let holder = tokio::spawn(async move { holder.await });
    entered.notified().await;
    let response = server
        .get("/_test/inherited")
        .authorization(authorization)
        .await;
    assert_eq!(response.status_code(), StatusCode::OK);
    assert_eq!(
        provider.requests.load(Ordering::Relaxed),
        1,
        "the live hit bypasses provider work"
    );
    assert_eq!(holder.await.unwrap().status_code(), StatusCode::NO_CONTENT);
    tokio::time::resume();
    provider.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn outer_http_timeout_cancels_inflight_verification_and_owns_504() {
    let captured = CumulativeMetrics::new();
    let _local = captured.install_default();
    let provider = Provider::start();
    let router = timeout_router(provider.verifier(), Duration::from_secs(1), Arc::default());
    let server = TestServer::new(router.into_make_service());
    let request = server
        .get("/_test/inherited")
        .authorization(format!("Bearer {HELD_PROVIDER}"))
        .add_header(REQUEST_ID_HEADER, "outer-timeout");
    let response = tokio::spawn(async move { request.await });
    bounded("provider exchange started", provider.held.notified()).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    let response = bounded("outer request completed", response).await.unwrap();
    let body = problem(
        &response,
        StatusCode::GATEWAY_TIMEOUT,
        "gateway_timeout",
        "outer-timeout",
    );
    assert_eq!(
        body["detail"],
        "request budget expired before a response could be committed"
    );
    assert!(response.maybe_header(WWW_AUTHENTICATE).is_none());
    assert_eq!(
        captured.counter(
            "authn_verifications_total",
            &[("result", "timeout"), ("failure", "timeout")],
        ),
        1
    );
    tokio::time::resume();
    provider.stop().await;
}

#[tokio::test(flavor = "current_thread")]
async fn authentication_and_scope_authorization_work_without_a_composer() {
    let captured = CumulativeMetrics::new();
    let _local = captured.install_default();
    let provider = Provider::start();
    let app = HttpContract::application(bearer_document())
        .unwrap()
        .merge(infra_http::http_route!(authorized_without_idempotency).unwrap())
        .unwrap()
        .with_probes("/")
        .unwrap()
        .build(
            readiness_reader(),
            Authentication::Bearer(provider.verifier()),
            &options(BUDGET),
        )
        .unwrap();
    let server = TestServer::new(app.into_make_service());
    let mut responses = Vec::new();
    let mut verification_counts = Vec::new();
    let no_scope = format!("Bearer {NO_SCOPE}");
    let alice = format!("Bearer {ALICE}");
    for authorization in [
        None,
        Some("Basic Zm9vOmJhcg=="),
        Some("Bearer token token"),
        Some(&no_scope),
        Some(&alice),
    ] {
        let mut request = server.get("/_test/authorization");
        if let Some(value) = authorization {
            request = request.authorization(value);
        }
        responses.push(request.await);
        verification_counts.push((
            captured.counter("authn_verifications_total", &[]),
            captured.counter("authn_token_verifications_total", &[]),
            captured.counter(
                "authn_verifications_total",
                &[
                    ("transport", "http"),
                    ("result", "success"),
                    ("failure", "none"),
                ],
            ),
        ));
    }
    provider.stop().await;

    for (response, status, code, challenge) in [
        (
            &responses[0],
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Bearer realm=\"api\"",
        ),
        (
            &responses[1],
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Bearer realm=\"api\"",
        ),
        (
            &responses[2],
            StatusCode::BAD_REQUEST,
            "authentication_malformed",
            "Bearer error=\"invalid_request\"",
        ),
        (
            &responses[3],
            StatusCode::FORBIDDEN,
            "forbidden",
            "Bearer error=\"insufficient_scope\"",
        ),
    ] {
        problem_body(response, status, code);
        assert_eq!(
            response.header(WWW_AUTHENTICATE),
            HeaderValue::from_static(challenge)
        );
    }
    assert_eq!(responses[4].status_code(), StatusCode::OK);
    assert_eq!(responses[4].text(), "alice");
    assert_eq!(
        verification_counts,
        [(1, 0, 0), (2, 0, 0), (3, 0, 0), (4, 1, 1), (5, 2, 2)],
        "each request records one HTTP outcome and both parsed tokens verify successfully"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn scope_alternatives_are_ored_and_scopes_within_one_are_anded() {
    let provider = Provider::start();
    let calls = Arc::new(Mutex::new(Vec::<DelegationCredential>::new()));
    let app = HttpContract::application(bearer_document())
        .unwrap()
        .merge(infra_http::http_route!(authorized_without_idempotency).unwrap())
        .unwrap()
        .merge(infra_http::http_route!(public_head).unwrap())
        .unwrap()
        .merge(
            infra_http::http_route!(scoped_alternatives)
                .unwrap()
                .with_state(Arc::clone(&calls)),
        )
        .unwrap()
        .with_probes("/")
        .unwrap()
        .build(
            readiness_reader(),
            Authentication::Bearer(provider.verifier()),
            &options(BUDGET),
        )
        .unwrap();
    let server = TestServer::new(app.clone().into_make_service());
    let granted_by_and = server
        .get("/_test/scoped-alternatives")
        .authorization_bearer(SCOPE_A_AND_B)
        .await;
    assert_eq!(granted_by_and.status_code(), StatusCode::OK);
    assert_eq!(granted_by_and.text(), "granted");
    let granted_by_or = server
        .get("/_test/scoped-alternatives")
        .authorization_bearer(SCOPE_C)
        .await;
    assert_eq!(granted_by_or.status_code(), StatusCode::OK);

    let denied = server
        .get("/_test/scoped-alternatives")
        .authorization_bearer(SCOPE_A_ONLY)
        .await;
    problem_body(&denied, StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(
        denied.header(WWW_AUTHENTICATE),
        HeaderValue::from_static("Bearer error=\"insufficient_scope\"")
    );
    let forged = {
        let calls = calls.lock().unwrap();
        let delegated: Vec<_> = calls
            .iter()
            .map(|c| secrecy::ExposeSecret::expose_secret(c.access_token()))
            .collect();
        assert_eq!(delegated, [SCOPE_A_AND_B, SCOPE_C]);
        calls[0].clone()
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the test injects a forged extension outside the finalized application"
    )]
    let injected = axum::Router::new()
        .fallback_service(app)
        .layer(Extension(forged));
    let server = TestServer::new(injected.into_make_service());
    for (method, expected) in [
        (Method::GET, StatusCode::OK),
        (Method::HEAD, StatusCode::NO_CONTENT),
    ] {
        assert_eq!(
            server
                .method(method, "/_test/authorization")
                .authorization_bearer(ALICE)
                .await
                .status_code(),
            expected
        );
    }
    provider.stop().await;
}
