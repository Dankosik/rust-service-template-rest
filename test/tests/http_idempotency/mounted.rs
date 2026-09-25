//! P9: the idempotency boundary mounted under the hardened chain.
//!
//! A test-only operation is composed exactly as the adopter guide's feature
//! path. Its handler takes the `Idempotency` extractor, the verified
//! principal, its `async-trait` port through `Extension`, and the JSON input;
//! it authorizes every attempt before the seam and runs its work through
//! `Idempotency::execute`, where the port's adapter writes through
//! `infra_idempotency_store::connection`. `Composer::route` composes it with
//! the real introspection verifier, which asks a TLS fixture provider on
//! loopback; agreement is asserted active, and `axum-test` drives the
//! hardened router. Each response is checked for status, content type,
//! Problem code, and headers, and the outcome counter is read from a
//! thread-local Prometheus recorder: `#[sqlx::test]` runs a current-thread
//! runtime, so every emission lands on the test thread.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Bytes;
use axum::http::header::{
    CONTENT_LANGUAGE, ETAG, LOCATION, RETRY_AFTER, WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_test::{TestRequest, TestResponse, TestServer};
use health::Readiness;
use infra_bearerauthn::test_support::{FixtureTransport, prepare_introspection_with_fixture};
use infra_bearerauthn::{IntrospectionOptions, ProviderUrl, Verifier};
use infra_http::idempotency::{
    Activation, Composer, Fingerprint, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Idempotency, Tx,
};
use infra_http::problem::SANITIZED_DETAIL;
use infra_http::{
    Code, HardenOptions, Problem, REQUEST_ID_HEADER, VerifiedPrincipal, harden, routes,
};
use infra_idempotency_store::Store;
use infra_postgres::PgPool;
use integration_tests::dsn_for;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusRecorder};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Executor as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use utoipa::ToSchema;

use crate::commit_proxy::Fault;
use crate::{
    Hold, RETENTION, RETRY_PAUSE, WAIT, bounded, close, count, make_read_only, proxied_pool,
    template_pool,
};

const WIDGETS: &str = "/widgets";
const KEY: &str = "idempotency-key";
const WIDGET_ROWS: &str = "SELECT count(*) FROM widgets";
/// The operation's fingerprint version.
const CREATE_WIDGET_V1: NonZeroU32 = NonZeroU32::MIN;
/// The request budget, above the provider's own three-second bound, so a slow
/// TLS handshake is never mistaken for a boundary outcome.
const BUDGET: Duration = Duration::from_secs(5);
/// A request budget that expires while held work waits.
const SHORT_BUDGET: Duration = Duration::from_secs(2);
/// Request bodies up to 2 MiB, so a success can outgrow the stored-body bound.
const MAX_BODY_BYTES: usize = 2 * 1_048_576;
/// The specified stored-body bound: 1 MiB.
const STORED_BODY_BOUND: usize = 1_048_576;
/// A name the operation's own validation refuses inside the work.
const RESERVED_NAME: &str = "reserved";
const KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const KEY_REASON: &str = "must be one Idempotency-Key field of 1 to 255 RFC 9110 token characters";

// The fixture provider and the callers it knows.
const FIXTURE_HOST: &str = "authn.fixture.test";
const FIXTURE_ROOT_DER: &[u8] =
    include_bytes!("../../../crates/infra-bearerauthn/tests/fixtures/authn-fixture-root.der");
const FIXTURE_CERT_DER: &[u8] =
    include_bytes!("../../../crates/infra-bearerauthn/tests/fixtures/authn-fixture-cert.der");
const FIXTURE_KEY_DER: &[u8] =
    include_bytes!("../../../crates/infra-bearerauthn/tests/fixtures/authn-fixture-key.der");
const ISSUER: &str = "https://issuer.example";
const AUDIENCE: &str = "service";
const ALICE: &str = "alice-token";
/// Alice through a client that lost its permission: the same scope, refused
/// by the operation's authorization.
const ALICE_REVOKED: &str = "alice-revoked-client-token";
const BOB: &str = "bob-token";
/// A verified caller with no scope required by this operation.
const NO_SCOPE: &str = "no-scope-token";
const INACTIVE: &str = "inactive-token";
const REVOKED_CLIENT: &str = "revoked-client";
/// Bound on one introspection request.
const MAX_INTROSPECTION_REQUEST: usize = 16 * 1024;

/// The operation's semantic input: the whole decoded body.
#[derive(Debug, Deserialize, Serialize, ToSchema)]
struct NewWidget {
    name: String,
    color: String,
}

#[derive(Debug, Serialize, ToSchema)]
struct Widget {
    id: i64,
    name: String,
    color: String,
}

impl Widget {
    fn location(&self) -> String {
        format!("{WIDGETS}/{}", self.id)
    }
}

/// The port's failures, in the feature's terms.
#[derive(Debug)]
enum CreateError {
    /// The operation's own validation, checked inside the work.
    Reserved,
    /// The adapter could not write.
    Store,
}

impl CreateError {
    fn into_problem(self) -> Problem {
        match self {
            Self::Reserved => {
                Problem::new(Code::UnprocessableContent).detail("the widget name is reserved")
            }
            Self::Store => Problem::new(Code::InternalServerError).detail(SANITIZED_DETAIL),
        }
    }
}

/// The feature's port: create one widget inside the boundary's transaction.
#[async_trait]
trait CreateWidgets: Send + Sync {
    async fn create(&self, tx: &mut Tx<'_>, input: &NewWidget) -> Result<Widget, CreateError>;
}

/// The port's provider adapter, the only code that reaches the transaction's
/// connection.
#[derive(Debug)]
struct SqlWidgets {
    hold: Arc<Hold>,
}

#[async_trait]
impl CreateWidgets for SqlWidgets {
    async fn create(&self, tx: &mut Tx<'_>, input: &NewWidget) -> Result<Widget, CreateError> {
        let id =
            sqlx::query_scalar("INSERT INTO widgets (name, color) VALUES ($1, $2) RETURNING id")
                .bind(&input.name)
                .bind(&input.color)
                .fetch_one(infra_idempotency_store::connection(tx))
                .await
                .map_err(|_| CreateError::Store)?;
        // Test control: an armed hold keeps this attempt inside its
        // transaction after the effect.
        self.hold.pass().await;
        if input.name == RESERVED_NAME {
            return Err(CreateError::Reserved);
        }
        Ok(Widget {
            id,
            name: input.name.clone(),
            color: input.color.clone(),
        })
    }
}

#[utoipa::path(
    post,
    path = "/widgets",
    operation_id = "createWidget",
    tag = "widgets",
    params(infra_http::idempotency::IdempotencyKey),
    request_body = NewWidget,
    security(("bearerAuth" = [])),
    extensions(
        ("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only idempotent operation composed as the adopter guide's feature path"
        })),
        ("x-idempotent" = json!(true))
    ),
    responses(
        (status = 201, description = "created", body = Widget),
        infra_http::idempotency::IdempotentOperationProblemResponses
    )
)]
async fn create_widget(
    idempotency: Idempotency,
    principal: VerifiedPrincipal,
    Extension(widgets): Extension<Arc<dyn CreateWidgets>>,
    Json(input): Json<NewWidget>,
) -> Response {
    // Every attempt is authorized before the seam, so a replay never skips it.
    if let Err(response) = infra_http::require_scope(&principal, "widgets:write") {
        return response;
    }
    if !may_create(&principal) {
        return forbidden();
    }
    idempotency
        .execute(
            Fingerprint::new(CREATE_WIDGET_V1, &input),
            async |tx: &mut Tx<'_>| match widgets.create(tx, &input).await {
                Ok(widget) => (
                    StatusCode::CREATED,
                    [
                        (LOCATION, widget.location()),
                        // Replayable: kept on the first response and every
                        // replay.
                        (CONTENT_LANGUAGE, "en".to_owned()),
                        // Not replayable, so the boundary strips it from the
                        // first response too.
                        (ETAG, format!("\"widget-{}\"", widget.id)),
                    ],
                    Json(widget),
                )
                    .into_response(),
                // Non-2xx: rolled back and returned unchanged.
                Err(err) => err.into_problem().into_response(),
            },
        )
        .await
}

/// The operation's authorization: a caller acting through a revoked client
/// may not create widgets.
fn may_create(principal: &VerifiedPrincipal) -> bool {
    principal.client_id() != Some(REVOKED_CLIENT)
}

fn forbidden() -> Response {
    Problem::new(Code::Forbidden)
        .detail("widget creation is not permitted")
        .into_response()
}

/// A TLS introspection provider on loopback that answers each request by its
/// token.
struct Provider {
    address: SocketAddr,
    cancel: CancellationToken,
    tasks: TaskTracker,
}

impl Provider {
    async fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the provider listens");
        let address = listener.local_addr().expect("the provider's address");
        let acceptor = TlsAcceptor::from(Arc::new(tls_config()));
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        tasks.spawn(serve(listener, acceptor, cancel.clone(), tasks.clone()));
        Self {
            address,
            cancel,
            tasks,
        }
    }

    /// The real introspection verifier, reaching this provider through the
    /// fixture transport.
    fn verifier(&self) -> Verifier {
        let transport = FixtureTransport::new(
            FIXTURE_HOST,
            self.address,
            FIXTURE_ROOT_DER,
            self.cancel.child_token(),
        )
        .expect("the fixture transport");
        prepare_introspection_with_fixture(
            IntrospectionOptions {
                issuer: ProviderUrl::parse(ISSUER).expect("fixture issuer URL"),
                audiences: vec![AUDIENCE.to_owned()],
                endpoint: ProviderUrl::parse(&format!("https://{FIXTURE_HOST}/introspect"))
                    .expect("fixture endpoint URL"),
                client_id: "fixture-client".to_owned(),
                client_secret: SecretString::from("fixture-secret"),
                provider_concurrency: NonZeroUsize::new(16).expect("fixture capacity"),
                cache: None,
            },
            transport,
        )
        .expect("the introspection verifier")
    }

    /// Stop serving and join every task, the verifier's fixture resolution
    /// included.
    async fn stop(self) {
        self.cancel.cancel();
        self.tasks.close();
        bounded("the provider's tasks to join", self.tasks.wait()).await;
    }
}

fn tls_config() -> ServerConfig {
    ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("fixture TLS protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(FIXTURE_CERT_DER)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(FIXTURE_KEY_DER)),
        )
        .expect("the fixture certificate and key")
}

/// Accept provider connections until cancelled; each answers one request.
async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    cancel: CancellationToken,
    tasks: TaskTracker,
) {
    loop {
        let socket = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((socket, _)) => socket,
                Err(_) => return,
            },
        };
        let acceptor = acceptor.clone();
        let cancel = cancel.clone();
        tasks.spawn(async move {
            cancel.run_until_cancelled(answer(&acceptor, socket)).await;
        });
    }
}

/// Answer one introspection request by its token.
async fn answer(acceptor: &TlsAcceptor, socket: TcpStream) {
    let Ok(mut stream) = acceptor.accept(socket).await else {
        return;
    };
    let Some(token) = read_token(&mut stream).await else {
        return;
    };
    let body = introspection(&token);
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// The `token` form field of one HTTP/1.1 request.
async fn read_token<S: AsyncRead + Unpin>(stream: &mut S) -> Option<String> {
    let mut request = Vec::new();
    loop {
        if let Some(body) = request_body(&request) {
            return url::form_urlencoded::parse(body)
                .find(|(name, _)| name == "token")
                .map(|(_, token)| token.into_owned());
        }
        if request.len() > MAX_INTROSPECTION_REQUEST
            || stream.read_buf(&mut request).await.ok()? == 0
        {
            return None;
        }
    }
}

/// The body of a complete request, once its head and `content-length` bytes
/// have arrived.
fn request_body(request: &[u8]) -> Option<&[u8]> {
    let head_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?
        + 4;
    let head = std::str::from_utf8(&request[..head_end]).ok()?;
    let length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    })?;
    request.get(head_end..head_end + length)
}

/// The provider's answer for `token`: two subjects, the first also through a
/// revoked client, and inactive for anything else.
fn introspection(token: &str) -> String {
    let (subject, client, scope) = match token {
        ALICE => ("alice", "widgets-app", "widgets:write"),
        ALICE_REVOKED => ("alice", REVOKED_CLIENT, "widgets:write"),
        BOB => ("bob", "widgets-app", "widgets:write"),
        NO_SCOPE => ("scope-less", "widgets-app", ""),
        _ => return json!({"active": false}).to_string(),
    };
    json!({
        "active": true,
        "iss": ISSUER,
        "aud": AUDIENCE,
        "exp": 2_147_483_647,
        "sub": subject,
        "client_id": client,
        "scope": scope,
    })
    .to_string()
}

/// The operation composed over a store pool under the hardened chain, with
/// its fixture provider and the hold its adapter passes.
struct Mounted {
    server: TestServer,
    hold: Arc<Hold>,
    provider: Provider,
    store_pool: PgPool,
}

impl Mounted {
    /// Create the widget table and compose over a new template pool on the
    /// per-test database.
    async fn on(pool: &PgPool, budget: Duration) -> Self {
        create_widgets(pool).await;
        let store_pool = template_pool(&dsn_for(pool).await, 4).await;
        Self::new(store_pool, budget).await
    }

    /// Compose the operation over `store_pool`, as the composition root does,
    /// under a request budget of `budget`.
    async fn new(store_pool: PgPool, budget: Duration) -> Self {
        let provider = Provider::start().await;
        let hold = Arc::new(Hold::default());
        let widgets: Arc<dyn CreateWidgets> = Arc::new(SqlWidgets {
            hold: Arc::clone(&hold),
        });
        let mut composer = Composer::new(Store::new(store_pool.clone(), RETENTION));
        // As `service::api::contract` assembles it: the transport router
        // registers the shared problem responses the family references (the
        // 403 among them must resolve), and the composer adds its own.
        let contract = infra_http::router()
            .routes(composer.route(routes!(create_widget)))
            .merge_document(composer.components());
        let document = contract.document().clone();
        let activation = composer
            .agree(&document)
            .expect("the composed operation agrees with the document");
        assert!(
            matches!(activation, Activation::Active { .. }),
            "{activation:?}"
        );
        let routes = infra_http::authn::finalize(contract, provider.verifier(), 32 * 1024)
            .expect("the mounted auth contract finalizes");
        let app = harden(
            routes
                .layer(Extension(widgets))
                .with_state(Readiness::new(Vec::new()).reader()),
            &HardenOptions {
                max_body_bytes: MAX_BODY_BYTES,
                request_timeout: budget,
                max_in_flight: NonZeroU32::new(16),
                log_health_probes: false,
            },
        );
        Self {
            server: TestServer::new(app),
            hold,
            provider,
            store_pool,
        }
    }

    /// A create request from the caller behind `token`, with `key` as its one
    /// `Idempotency-Key` field and a JSON `input`.
    fn create(&self, token: &str, key: &str, input: &Value, request_id: &str) -> TestRequest {
        self.server
            .post(WIDGETS)
            .authorization_bearer(token)
            .add_header(KEY, key)
            .add_header(REQUEST_ID_HEADER, request_id)
            .json(input)
    }

    async fn finish(self) {
        close(&[&self.store_pool]).await;
        self.provider.stop().await;
    }
}

async fn create_widgets(pool: &PgPool) {
    pool.execute(
        "CREATE TABLE widgets (id bigserial PRIMARY KEY, name text NOT NULL, color text NOT NULL)",
    )
    .await
    .expect("the widget table");
}

/// Assert a Problem's status, content type, and code, and return its body.
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

/// Assert a Problem of the chain, authentication, or the boundary: it also
/// carries the exchange's own request id, in the header and the body.
fn problem(response: &TestResponse, status: StatusCode, code: &str, request_id: &str) -> Value {
    let body = problem_body(response, status, code);
    assert_eq!(
        response.maybe_header(REQUEST_ID_HEADER),
        Some(HeaderValue::from_str(request_id).expect("a request id"))
    );
    assert_eq!(body["request_id"], request_id, "{body}");
    body
}

/// Assert the stored form of a created widget answered to `request_id`: 201,
/// the replayable headers the work set, none it set beyond them, and the
/// exchange's own request id. Returns the body.
fn created(response: &TestResponse, request_id: &str) -> Value {
    assert_eq!(
        response.status_code(),
        StatusCode::CREATED,
        "{}",
        response.text()
    );
    assert_eq!(
        response.maybe_content_type().as_deref(),
        Some("application/json")
    );
    let body: Value = serde_json::from_slice(response.as_bytes()).expect("a JSON widget");
    let location = format!("{WIDGETS}/{}", body["id"]);
    assert_eq!(
        response.maybe_header(LOCATION),
        Some(HeaderValue::from_str(&location).expect("a location"))
    );
    assert_eq!(
        response.maybe_header(CONTENT_LANGUAGE),
        Some(HeaderValue::from_static("en"))
    );
    assert!(
        response.maybe_header(ETAG).is_none(),
        "a header beyond the replayable five is stripped"
    );
    assert_eq!(
        response.maybe_header(REQUEST_ID_HEADER),
        Some(HeaderValue::from_str(request_id).expect("a request id"))
    );
    assert_eq!(
        response.maybe_header(X_CONTENT_TYPE_OPTIONS),
        Some(HeaderValue::from_static("nosniff"))
    );
    body
}

/// The retry hint of the retryable idempotency Problems.
fn retry_after(response: &TestResponse) -> Option<String> {
    response.maybe_header(RETRY_AFTER).map(|value| {
        value
            .to_str()
            .expect("a visible Retry-After value")
            .to_owned()
    })
}

/// Every `http_idempotency_outcomes_total` series the recorder renders, by
/// `outcome`.
fn outcomes(recorder: &PrometheusRecorder) -> BTreeMap<String, u64> {
    recorder
        .handle()
        .render()
        .lines()
        .filter_map(|line| {
            let series = line
                .strip_prefix(HTTP_IDEMPOTENCY_OUTCOMES_METRIC)?
                .strip_prefix("{outcome=\"")?;
            let (outcome, value) = series.split_once("\"} ")?;
            Some((
                outcome.to_owned(),
                value.parse().expect("a whole counter value"),
            ))
        })
        .collect()
}

/// The expected outcome counts; a zero count is a series never recorded.
fn counts(expected: &[(&str, u64)]) -> BTreeMap<String, u64> {
    expected
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|&(outcome, count)| (outcome.to_owned(), count))
        .collect()
}

/// Send `request()` until it is no longer 409, and count the 409s: a dropped
/// attempt holds its key until `PostgreSQL` ends that attempt's transaction.
async fn until_free(request: impl Fn() -> TestRequest) -> (TestResponse, u64) {
    let deadline = Instant::now() + WAIT;
    let mut conflicts = 0;
    loop {
        let response = request().await;
        if response.status_code() != StatusCode::CONFLICT {
            return (response, conflicts);
        }
        conflicts += 1;
        assert!(
            Instant::now() < deadline,
            "the key stayed held for {WAIT:?}"
        );
        tokio::time::sleep(RETRY_PAUSE).await;
    }
}

#[utoipa::path(
    get,
    path = "/_test/authorization",
    operation_id = "authorizationWithoutIdempotency",
    responses(
        (status = 200, description = "verified", content_type = "text/plain", body = String),
        infra_http::problem::responses::ProtectedOperationProblemResponses,
    )
)]
async fn authorized_without_idempotency(
    principal: VerifiedPrincipal,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Err(response) = infra_http::require_scope(&principal, "widgets:write") {
        return response;
    }
    assert!(!headers.contains_key(axum::http::header::AUTHORIZATION));
    (
        StatusCode::OK,
        principal.subject().unwrap_or_default().to_owned(),
    )
        .into_response()
}

#[tokio::test(flavor = "current_thread")]
async fn repair_regression_authentication_and_scope_authorization_work_without_a_composer() {
    // Keep the router and provider tasks on the test thread so this recorder
    // cannot observe counters from concurrently running tests.
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let provider = Provider::start().await;
    let root_policy = serde_json::from_value(json!({
        "openapi": "3.1.0",
        "info": {"title": "authorization fixture", "version": "1"},
        "paths": {},
        "components": {"securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}}},
        "security": [{"bearerAuth": []}]
    }))
    .expect("the inherited bearer policy");
    let contract = infra_http::ContractRouter::with_openapi(root_policy)
        .merge(infra_http::router())
        .routes(routes!(authorized_without_idempotency));
    let router = infra_http::authn::finalize(contract, provider.verifier(), 32 * 1024)
        .expect("the protected contract finalizes without an idempotency composer");
    let server = TestServer::new(harden(
        router.with_state(Readiness::new(Vec::new()).reader()),
        &HardenOptions {
            max_body_bytes: MAX_BODY_BYTES,
            request_timeout: BUDGET,
            max_in_flight: NonZeroU32::new(16),
            log_health_probes: false,
        },
    ));
    let mut responses = Vec::new();
    let mut verification_counts = Vec::new();
    for authorization in [
        None,
        Some("Bearer token token"),
        Some(NO_SCOPE),
        Some(ALICE),
    ] {
        let mut request = server.get("/_test/authorization");
        if let Some(value) = authorization {
            request = request.authorization(if value.starts_with("Bearer ") {
                value.to_owned()
            } else {
                format!("Bearer {value}")
            });
        }
        responses.push(request.await);
        let scrape = recorder.handle().render();
        let count = |name: &str| -> u64 {
            scrape
                .lines()
                .filter_map(|line| {
                    let series = line.strip_prefix(name)?;
                    if !series.starts_with('{') && !series.starts_with(' ') {
                        return None;
                    }
                    let (_, value) = series.rsplit_once(' ')?;
                    Some(value.parse::<u64>().expect("a whole verification counter"))
                })
                .sum()
        };
        verification_counts.push((
            count("authn_verifications_total"),
            count("authn_token_verifications_total"),
        ));
    }
    provider.stop().await;

    for (response, status, code, challenge) in [
        (
            &responses[0],
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Bearer",
        ),
        (
            &responses[1],
            StatusCode::BAD_REQUEST,
            "authentication_malformed",
            "Bearer error=\"invalid_request\"",
        ),
        (
            &responses[2],
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
    assert_eq!(responses[3].status_code(), StatusCode::OK);
    assert_eq!(responses[3].text(), "alice");
    assert_eq!(
        verification_counts,
        [(1, 0), (2, 0), (3, 1), (4, 2)],
        "each request records one HTTP outcome; only a parsed token reaches engine verification"
    );
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_authentication_failures_answer_before_the_key_is_read(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    // Authentication answers first: a bad key behind a failed authentication
    // is never looked at.
    let oversize = format!("Bearer {}", "a".repeat(32 * 1024 + 1));
    let inactive = format!("Bearer {INACTIVE}");
    for (authorization, status, code, challenge) in [
        (
            None,
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            Some("Bearer"),
        ),
        (
            Some("Basic Zm9vOmJhcg=="),
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            Some("Bearer"),
        ),
        (
            Some("Bearer token token"),
            StatusCode::BAD_REQUEST,
            "authentication_malformed",
            Some("Bearer error=\"invalid_request\""),
        ),
        (
            Some(oversize.as_str()),
            StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            "authentication_oversize",
            None,
        ),
        (
            Some(inactive.as_str()),
            StatusCode::UNAUTHORIZED,
            "authentication_invalid",
            Some("Bearer error=\"invalid_token\""),
        ),
    ] {
        let mut request = mounted
            .server
            .post(WIDGETS)
            .add_header(KEY, "\"k-1\"")
            .add_header(REQUEST_ID_HEADER, "req-authn")
            .json(&input);
        if let Some(authorization) = authorization {
            request = request.authorization(authorization);
        }
        let response = request.await;
        problem(&response, status, code, "req-authn");
        assert_eq!(
            response.maybe_header(WWW_AUTHENTICATE),
            challenge.map(HeaderValue::from_static)
        );
    }
    assert_eq!(outcomes(&recorder), counts(&[]));

    let scope_denied = mounted
        .create(NO_SCOPE, "scope-check", &input, "req-scope")
        .await;
    problem_body(&scope_denied, StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(
        scope_denied.maybe_header(WWW_AUTHENTICATE),
        Some(HeaderValue::from_static(
            "Bearer error=\"insufficient_scope\""
        ))
    );
    assert_eq!(outcomes(&recorder), counts(&[]));

    // Once authentication passes, the same key is refused.
    let refused = mounted.create(ALICE, "\"k-1\"", &input, "req-key").await;
    problem(&refused, StatusCode::BAD_REQUEST, "bad_request", "req-key");
    assert_eq!(outcomes(&recorder), counts(&[("invalid_key", 1)]));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_the_key_grammar_is_exact(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    // Every malformed key is one fixed 400 that names the rule and never the
    // value: missing, empty, 256 bytes, the quoted form, a space, a comma,
    // non-ASCII, and a repeated field, with distinct and with equal values.
    let long = "k".repeat(256);
    let invalid: [&[&[u8]]; 9] = [
        &[],
        &[b""],
        &[long.as_bytes()],
        &[b"\"k-1\""],
        &[b"k 1"],
        &[b"k,1"],
        &["k-\u{e9}".as_bytes()],
        &[b"k-1", b"k-2"],
        &[b"k-1", b"k-1"],
    ];
    for values in invalid {
        let mut request = mounted
            .server
            .post(WIDGETS)
            .authorization_bearer(ALICE)
            .add_header(REQUEST_ID_HEADER, "req-key")
            .json(&input);
        for value in values {
            request = request.add_header(KEY, HeaderValue::from_bytes(value).expect("a field"));
        }
        let body = problem(
            &request.await,
            StatusCode::BAD_REQUEST,
            "bad_request",
            "req-key",
        );
        assert_eq!(body["detail"], KEY_DETAIL);
        assert_eq!(
            body["invalid_params"],
            json!([{"name": "header.Idempotency-Key", "reason": KEY_REASON}])
        );
    }
    assert_eq!(outcomes(&recorder), counts(&[("invalid_key", 9)]));

    // The longest key, the shortest, and every token symbol are accepted.
    for key in [
        "k".repeat(255),
        "k".to_owned(),
        "!#$%&'*+-.^_`|~09AZaz".to_owned(),
    ] {
        created(
            &mounted.create(ALICE, &key, &input, "req-valid").await,
            "req-valid",
        );
    }
    assert_eq!(
        outcomes(&recorder),
        counts(&[("invalid_key", 9), ("executed", 3)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 3);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_success_replays_byte_for_byte_behind_current_authorization(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    let first = mounted.create(ALICE, "k-1", &input, "req-first").await;
    let widget = created(&first, "req-first");
    assert_eq!(outcomes(&recorder), counts(&[("executed", 1)]));

    // Member order and whitespace are representation: the retry replays the
    // stored bytes under its own request id, still without the stripped
    // header.
    let reordered = mounted
        .server
        .post(WIDGETS)
        .authorization_bearer(ALICE)
        .add_header(KEY, "k-1")
        .add_header(REQUEST_ID_HEADER, "req-replay")
        .content_type("application/json")
        .bytes(Bytes::from_static(
            b"{ \"color\" : \"red\",\n  \"name\" : \"gizmo\" }",
        ))
        .await;
    created(&reordered, "req-replay");
    assert_eq!(reordered.as_bytes(), first.as_bytes());
    assert_eq!(
        outcomes(&recorder),
        counts(&[("executed", 1), ("replayed", 1)])
    );

    // Authorization runs on every attempt: the same caller through a revoked
    // client is refused before any replay, and no outcome is recorded.
    let refused = mounted
        .create(ALICE_REVOKED, "k-1", &input, "req-refused")
        .await;
    let body = problem_body(&refused, StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(body["detail"], "widget creation is not permitted");
    assert_eq!(
        outcomes(&recorder),
        counts(&[("executed", 1), ("replayed", 1)])
    );

    // Another input under the same key is a mismatch, and the record stays.
    let changed = json!({"name": "gizmo", "color": "blue"});
    let mismatch = mounted.create(ALICE, "k-1", &changed, "req-changed").await;
    problem(
        &mismatch,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-changed",
    );
    assert!(retry_after(&mismatch).is_none());
    // Another caller's same key is independent.
    let other = mounted.create(BOB, "k-1", &input, "req-bob").await;
    assert_ne!(created(&other, "req-bob")["id"], widget["id"]);
    let again = mounted.create(ALICE, "k-1", &input, "req-again").await;
    created(&again, "req-again");
    assert_eq!(again.as_bytes(), first.as_bytes());
    assert_eq!(
        outcomes(&recorder),
        counts(&[("executed", 2), ("replayed", 2), ("key_mismatch", 1)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 2);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_rolled_back_work_and_undecodable_records_are_never_replayed(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;

    // The operation's own non-2xx comes back unchanged, rolls back, and is
    // not stored, so a retry runs the work again.
    let reserved = json!({"name": RESERVED_NAME, "color": "red"});
    for request_id in ["req-reserved", "req-reserved-again"] {
        let response = mounted.create(ALICE, "k-2", &reserved, request_id).await;
        let body = problem_body(
            &response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unprocessable_content",
        );
        assert_eq!(body["detail"], "the widget name is reserved");
    }
    assert_eq!(outcomes(&recorder), counts(&[("not_stored", 2)]));

    // A success too large to store is a sanitized 500 that commits nothing.
    let huge = json!({"name": "x".repeat(STORED_BODY_BOUND), "color": "red"});
    let unstorable = mounted.create(ALICE, "k-3", &huge, "req-huge").await;
    let body = problem(
        &unstorable,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "req-huge",
    );
    assert_eq!(body["detail"], "request failed");
    assert_eq!(outcomes(&recorder), counts(&[("not_stored", 3)]));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);

    // No record stayed behind, so the same key executes another input.
    let input = json!({"name": "gizmo", "color": "red"});
    let stored = mounted.create(ALICE, "k-3", &input, "req-stored").await;
    created(&stored, "req-stored");

    // An undecodable live record is an integrity failure on every retry, and
    // the work never runs.
    let corrupted = sqlx::query("UPDATE http_idempotency_records SET format = 9 WHERE body = $1")
        .bind(&stored.as_bytes()[..])
        .execute(&pool)
        .await
        .expect("the record is corrupted");
    assert_eq!(corrupted.rows_affected(), 1);
    for request_id in ["req-integrity", "req-integrity-again"] {
        let response = mounted.create(ALICE, "k-3", &input, request_id).await;
        let body = problem(
            &response,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            request_id,
        );
        assert_eq!(body["detail"], "request failed");
    }
    assert_eq!(
        outcomes(&recorder),
        counts(&[("not_stored", 3), ("executed", 1), ("integrity", 2)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_held_key_answers_in_progress_with_a_retry_hint(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});
    let changed = json!({"name": "gizmo", "color": "blue"});

    // While the first attempt works, the same key answers 409 at once, with
    // the same input and with another.
    let duplicates = async {
        mounted.hold.entered().await;
        for (duplicate, request_id) in [(&input, "req-duplicate"), (&changed, "req-changed")] {
            let response = mounted.create(ALICE, "k-4", duplicate, request_id).await;
            problem(
                &response,
                StatusCode::CONFLICT,
                "idempotency_request_in_progress",
                request_id,
            );
            assert_eq!(retry_after(&response).as_deref(), Some("1"));
        }
        mounted.hold.release();
    };
    mounted.hold.arm();
    let (held, ()) = tokio::join!(mounted.create(ALICE, "k-4", &input, "req-held"), duplicates);
    created(&held, "req-held");
    let replay = mounted.create(ALICE, "k-4", &input, "req-after").await;
    created(&replay, "req-after");
    assert_eq!(replay.as_bytes(), held.as_bytes());
    assert_eq!(
        outcomes(&recorder),
        counts(&[("in_progress", 2), ("executed", 1), ("replayed", 1)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_an_expired_budget_answers_504_and_counts_abandoned(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, SHORT_BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    // The chain's timer answers while the work waits inside its transaction,
    // which rolls back.
    mounted.hold.arm();
    let expired = mounted.create(ALICE, "k-5", &input, "req-expired").await;
    problem(
        &expired,
        StatusCode::GATEWAY_TIMEOUT,
        "request_timeout",
        "req-expired",
    );
    mounted.hold.entered().await;
    assert_eq!(outcomes(&recorder), counts(&[("abandoned", 1)]));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);

    // The key is free once `PostgreSQL` ends the dropped transaction.
    let (retry, conflicts) = until_free(|| mounted.create(ALICE, "k-5", &input, "req-retry")).await;
    created(&retry, "req-retry");
    assert_eq!(
        outcomes(&recorder),
        counts(&[
            ("abandoned", 1),
            ("in_progress", conflicts),
            ("executed", 1)
        ])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_read_only_writer_answers_unavailable(pool: PgPool) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    create_widgets(&pool).await;
    // The writer turns read-only after startup: every session of the store's
    // pool is read-only.
    make_read_only(&pool).await;
    let store_pool = template_pool(&dsn_for(&pool).await, 4).await;
    let mounted = Mounted::new(store_pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    let refused = mounted
        .create(ALICE, "k-1", &input, "req-unavailable")
        .await;
    problem(
        &refused,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_unavailable",
        "req-unavailable",
    );
    assert_eq!(retry_after(&refused).as_deref(), Some("1"));
    assert_eq!(outcomes(&recorder), counts(&[("unavailable", 1)]));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_lost_commit_acknowledgements_answer_outcome_unknown_or_the_reconciled_success(
    pool: PgPool,
) {
    let recorder = PrometheusBuilder::new().build_recorder();
    let _local = metrics::set_default_local_recorder(&recorder);
    create_widgets(&pool).await;
    let (proxy, store_pool) = proxied_pool(&pool, 4).await;
    let mounted = Mounted::new(store_pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    // `COMMIT` never reaches the server: the readback finds no record, so the
    // outcome is unknown.
    proxy.arm(Fault::DropBeforeForward);
    let unknown = mounted.create(ALICE, "k-6", &input, "req-unknown").await;
    problem(
        &unknown,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_outcome_unknown",
        "req-unknown",
    );
    assert_eq!(retry_after(&unknown).as_deref(), Some("1"));
    assert_eq!(proxy.fired(), Some(Fault::DropBeforeForward));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    // A later retry executes once.
    let (retry, conflicts) = until_free(|| mounted.create(ALICE, "k-6", &input, "req-retry")).await;
    created(&retry, "req-retry");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);

    // The server commits and the acknowledgement is lost: the readback
    // reconciles the stored success, and a retry replays the same bytes.
    proxy.arm(Fault::ForwardThenDrop);
    let reconciled = mounted.create(ALICE, "k-7", &input, "req-reconciled").await;
    created(&reconciled, "req-reconciled");
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    let replay = mounted.create(ALICE, "k-7", &input, "req-replay").await;
    created(&replay, "req-replay");
    assert_eq!(replay.as_bytes(), reconciled.as_bytes());
    assert_eq!(
        outcomes(&recorder),
        counts(&[
            ("outcome_unknown", 1),
            ("in_progress", conflicts),
            ("executed", 1),
            ("reconciled", 1),
            ("replayed", 1),
        ])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 2);
    mounted.finish().await;
    proxy.shutdown().await;
}
