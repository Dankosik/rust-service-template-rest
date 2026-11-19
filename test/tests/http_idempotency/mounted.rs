use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::ServiceExt as _;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Extension, FromRef, State};
use axum::http::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LANGUAGE, ETAG, LAST_MODIFIED, LOCATION,
    RETRY_AFTER, WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_test::{TestRequest, TestResponse, TestServer};
use health::{Readiness, RefreshPolicy};
use infra_bearerauthn::test_support::{FixtureTransport, prepare_introspection_with_fixture};
use infra_bearerauthn::{
    EndpointUrl, IntrospectionCacheOptions, IntrospectionOptions, IssuerUrl, Verifier,
};
use infra_http::authn::ProtectedOperationProblemResponses;
use infra_http::idempotency::{
    CommandInput, FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Rollback, TerminalProblem, Tx,
};
use infra_http::{
    Application, Authentication, Code, DelegationCredential, HardenOptions, HttpContract,
    Principal, Problem, REQUEST_ID_HEADER, VerifiedPrincipal,
};
use infra_idempotency_store::Store;
use infra_postgres::PgPool;
use integration_tests::{dsn_for, propagate_join_panic};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Executor as _;
use tls_test_support::TlsMaterial;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::aws_lc_rs;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_util::sync::{CancellationToken, DropGuard};
use tokio_util::task::AbortOnDropHandle;
use tracing_subscriber::layer::SubscriberExt as _;
use utoipa::ToSchema;

use crate::commit_proxy::{Boundary, CommitProxy, Fault, ReadyBoundary};
use crate::{
    Hold, RETENTION, RETRY_PAUSE, WAIT, bounded, close_pools, count, make_read_only, template_pool,
};

const WIDGETS: &str = "/widgets";
const PRIMARY_PREFIX: &str = "/primary";
const SECONDARY_PREFIX: &str = "/secondary";
const PRIMARY_WIDGETS: &str = "/primary/widgets";
const SECONDARY_WIDGETS: &str = "/secondary/widgets";
const KEY: &str = "idempotency-key";
const WIDGET_ROWS: &str = "SELECT count(*) FROM widgets";
const BUDGET: Duration = Duration::from_secs(5);
const SHORT_BUDGET: Duration = Duration::from_secs(2);
const COMMAND_BUDGET: Duration = Duration::from_secs(1);
const MAX_BODY_BYTES: usize = 2 * 1_048_576;
const STORED_BODY_BOUND: usize = 1_048_576;
const RESERVED_NAME: &str = "reserved";
const HANDLER_TIMEOUT_COLOR: &str = "handler-timeout";
const UNSERIALIZABLE_COLOR: &str = "unserializable";
const KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const KEY_RULES_LINK: &str = "<https://docs.example/widgets#create>; rel=\"describedby\"";
const KEY_REASON: &str = concat!(
    "Supply exactly one Idempotency-Key field. Send an IETF Structured Field string in double quotes. ",
    "Parameters on the string do not change the key. Unquoted visible ASCII is also accepted for compatibility. ",
    "The decoded value must contain 1 to 255 bytes.",
);

const FIXTURE_HOST: &str = "authn.fixture.test";
const ISSUER: &str = "https://issuer.example";
const AUDIENCE: &str = "service";
const ALICE: &str = "alice-token";
const ALICE_REVOKED: &str = "alice-revoked-client-token";
const BOB: &str = "bob-token";
const NO_SCOPE: &str = "no-scope-token";
const SCOPE_A_AND_B: &str = "scope-a-and-b-token";
const SCOPE_C: &str = "scope-c-only-token";
const SCOPE_A_ONLY: &str = "scope-a-only-token";
const INACTIVE: &str = "inactive-token";
const HELD_PROVIDER: &str = "held-provider-token";
const REVOKED_CLIENT: &str = "revoked-client";
const MAX_INTROSPECTION_REQUEST: usize = 16 * 1024;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
struct NewWidget {
    name: String,
    color: String,
}

#[derive(Debug, Serialize, ToSchema)]
struct Widget {
    id: i64,
    name: String,
    #[serde(serialize_with = "serialize_widget_color")]
    color: String,
}

fn serialize_widget_color<S: serde::Serializer>(
    color: &str,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if color == UNSERIALIZABLE_COLOR {
        return Err(serde::ser::Error::custom(
            "fixture color cannot be serialized",
        ));
    }
    serializer.serialize_str(color)
}

impl Widget {
    fn location(&self) -> String {
        format!("{WIDGETS}/{}", self.id)
    }
}

#[derive(Debug)]
enum CreateError {
    Reserved,
    Store(sqlx::Error),
}

impl From<CreateError> for Rollback<TerminalProblem> {
    fn from(error: CreateError) -> Self {
        match error {
            CreateError::Reserved => Self::refuse(
                Problem::new(Code::UnprocessableContent).detail("the widget name is reserved"),
            ),
            CreateError::Store(error)
                if error.as_database_error().is_some_and(|db| {
                    db.is_unique_violation() && db.constraint() == Some("widgets_unique_name")
                }) =>
            {
                Self::Terminal(TerminalProblem::already_exists().detail("the widget name is taken"))
            }
            CreateError::Store(error) => Self::from(error),
        }
    }
}

#[async_trait]
trait CreateWidgets: Send + Sync {
    async fn create(&self, tx: &mut Tx<'_>, input: &NewWidget) -> Result<Widget, CreateError>;
}

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
                .fetch_one(tx)
                .await
                .map_err(CreateError::Store)?;
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
    request_body = NewWidget,
    params(("x-widget-name" = Option<String>, Header, description = "override the widget name")),
    security(("bearerAuth" = ["widgets:write"])),
    extensions(
        ("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only idempotent operation composed as the adopter guide's feature path"
        }))
    ),
    responses((status = 201, description = "created", body = Widget), ProtectedOperationProblemResponses)
)]
async fn create_widget(
    request: CommandInput,
    principal: Principal,
    delegation: Option<DelegationCredential>,
    state: WidgetState,
) -> Result<CreateWidget, Box<Problem>> {
    assert!(delegation.is_none());
    let mut input: NewWidget = request.json()?;
    if !may_create(&principal) {
        return Err(Box::new(forbidden()));
    }
    if let Some(name) = request.headers().get("x-widget-name") {
        let name = name.to_str().map_err(|_| Problem::new(Code::BadRequest))?;
        name.clone_into(&mut input.name);
    }
    let handler_timeout = input.color == HANDLER_TIMEOUT_COLOR;
    Ok(CreateWidget {
        widgets: state.widgets,
        input,
        handler_timeout,
    })
}

#[utoipa::path(
    put,
    path = "/widgets/{id}",
    operation_id = "replaceWidget",
    tag = "widgets",
    params(("id" = i64, Path, description = "widget resource identifier")),
    request_body = NewWidget,
    security(("bearerAuth" = [])),
    extensions(
        ("x-bearer-delegation" = json!(true)),
        ("x-security-decision" = json!({
            "exposure": "protected",
            "rationale": "test-only idempotent operation composed as the adopter guide's feature path"
        }))
    ),
    responses(ProtectedOperationProblemResponses)
)]
async fn replace_widget(
    mut request: CommandInput,
    principal: Principal,
    delegation: Option<DelegationCredential>,
    state: WidgetState,
) -> Result<CreateWidget, Box<Problem>> {
    assert!(delegation.is_some());
    let _id: i64 = request.path().await?;
    let input = request.json()?;
    if !may_create(&principal) {
        return Err(Box::new(forbidden()));
    }
    Ok(CreateWidget {
        widgets: state.widgets,
        input,
        handler_timeout: false,
    })
}

struct CreateWidget {
    widgets: Arc<dyn CreateWidgets>,
    input: NewWidget,
    handler_timeout: bool,
}

infra_http::created_json_response!(CreatedWidget(Widget));

impl FreshCommand for CreateWidget {
    type Success = CreatedWidget;
    type Terminal = TerminalProblem;

    async fn execute(self, tx: &mut Tx<'_>) -> Result<Self::Success, Rollback<Self::Terminal>> {
        let widget = self.widgets.create(tx, &self.input).await?;
        if widget.color == "invalid-refusal" {
            return Err(Rollback::refuse(StatusCode::NOT_MODIFIED));
        }
        Ok(created_response(widget))
    }

    fn attempt_timeout(&self) -> Option<Duration> {
        self.handler_timeout.then_some(COMMAND_BUDGET)
    }
}

fn created_response(widget: Widget) -> CreatedWidget {
    let location = widget.location();
    let id = widget.id;
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        LOCATION,
        HeaderValue::from_str(&location).expect("a location"),
    );
    headers.append(CONTENT_LANGUAGE, HeaderValue::from_static("en"));
    headers.append(CONTENT_LANGUAGE, HeaderValue::from_static("fr"));
    headers.insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=widget.json"),
    );
    headers.insert(
        ETAG,
        HeaderValue::from_str(&format!("\"widget-{id}\"")).expect("an etag"),
    );
    headers.insert(
        LAST_MODIFIED,
        HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"),
    );
    headers.insert("idempotent-replayed", HeaderValue::from_static("false"));
    CreatedWidget {
        body: widget,
        headers,
    }
}

fn may_create(principal: &Principal) -> bool {
    principal.client_id() != Some(REVOKED_CLIENT)
}

fn forbidden() -> Problem {
    Problem::new(Code::Forbidden).detail("widget creation is not permitted")
}

struct Provider {
    address: SocketAddr,
    material: TlsMaterial,
    requests: Arc<AtomicUsize>,
    held: Arc<tokio::sync::Notify>,
    cancel: CancellationToken,
    _cancel_guard: DropGuard,
    task: AbortOnDropHandle<()>,
}

impl Provider {
    async fn start() -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the provider listens");
        let address = listener.local_addr().expect("the provider's address");
        let material = TlsMaterial::new(FIXTURE_HOST);
        let acceptor = TlsAcceptor::from(Arc::new(tls_config(&material)));
        let cancel = CancellationToken::new();
        let cancel_guard = cancel.clone().drop_guard();
        let requests = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(serve(
            listener,
            acceptor,
            cancel.clone(),
            requests.clone(),
            held.clone(),
        ));
        Self {
            address,
            material,
            requests,
            held,
            cancel,
            _cancel_guard: cancel_guard,
            task: AbortOnDropHandle::new(task),
        }
    }

    fn verifier(&self) -> Verifier {
        self.verifier_with_cache(None)
    }

    fn verifier_with_cache(&self, cache: Option<IntrospectionCacheOptions>) -> Verifier {
        let transport = FixtureTransport::try_new(
            FIXTURE_HOST,
            self.address,
            &self.material.root,
            self.cancel.child_token(),
        )
        .expect("the fixture transport");
        prepare_introspection_with_fixture(
            IntrospectionOptions {
                issuer: IssuerUrl::parse(ISSUER).expect("fixture issuer URL"),
                audiences: vec![AUDIENCE.to_owned()],
                endpoint: EndpointUrl::parse(&format!("https://{FIXTURE_HOST}/introspect"))
                    .expect("fixture endpoint URL"),
                client_id: "fixture-client".to_owned(),
                client_secret: SecretString::from("fixture-secret"),
                provider_concurrency: const { NonZeroUsize::new(16).expect("16 is nonzero") },
                cache,
            },
            transport,
        )
        .expect("the introspection verifier")
    }

    async fn stop(mut self) {
        self.cancel.cancel();
        let result = tokio::time::timeout(WAIT, &mut self.task).await;
        if result.is_err() {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
        propagate_join_panic(result.expect("the provider's tasks to join"));
    }
}

fn tls_config(material: &TlsMaterial) -> ServerConfig {
    ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("fixture TLS protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(material.cert.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key.clone())),
        )
        .expect("the fixture certificate and key")
}

async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    cancel: CancellationToken,
    requests: Arc<AtomicUsize>,
    held: Arc<tokio::sync::Notify>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                propagate_join_panic(result);
            }
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else {
                    break;
                };
                let acceptor = acceptor.clone();
                let cancel = cancel.clone();
                let requests = requests.clone();
                let held = held.clone();
                connections.spawn(async move {
                    cancel
                        .run_until_cancelled(answer(&acceptor, socket, &requests, &held))
                        .await;
                });
            }
        }
    }
    while let Some(result) = connections.join_next().await {
        propagate_join_panic(result);
    }
}

async fn answer(
    acceptor: &TlsAcceptor,
    socket: TcpStream,
    requests: &Arc<AtomicUsize>,
    held: &Arc<tokio::sync::Notify>,
) {
    let Ok(stream) = acceptor.accept(socket).await else {
        return;
    };
    let service = hyper::service::service_fn(|request: hyper::Request<hyper::body::Incoming>| {
        let requests = Arc::clone(requests);
        let held = Arc::clone(held);
        async move {
            let Ok(body) =
                to_bytes(Body::new(request.into_body()), MAX_INTROSPECTION_REQUEST).await
            else {
                return Ok::<_, std::convert::Infallible>(StatusCode::BAD_REQUEST.into_response());
            };
            let Some(token) = url::form_urlencoded::parse(&body)
                .find(|(name, _)| name == "token")
                .map(|(_, value)| value.into_owned())
            else {
                return Ok(StatusCode::BAD_REQUEST.into_response());
            };
            requests.fetch_add(1, Ordering::Relaxed);
            if token == HELD_PROVIDER {
                held.notify_one();
                std::future::pending::<()>().await;
            }
            Ok((
                [(
                    axum::http::header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                )],
                introspection(&token),
            )
                .into_response())
        }
    });
    let _ = hyper::server::conn::http1::Builder::new()
        .keep_alive(false)
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
        .await;
}

fn introspection(token: &str) -> String {
    let (subject, client, scope) = match token {
        ALICE => ("alice", "widgets-app", "widgets:write"),
        ALICE_REVOKED => ("alice", REVOKED_CLIENT, "widgets:write"),
        BOB => ("bob", "widgets-app", "widgets:write"),
        NO_SCOPE => ("scope-less", "widgets-app", ""),
        SCOPE_A_AND_B => ("scoped", "widgets-app", "scope-a scope-b"),
        SCOPE_C => ("scoped", "widgets-app", "scope-c"),
        SCOPE_A_ONLY => ("scoped", "widgets-app", "scope-a"),
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
        "roles": ["widget-author"],
        "tenant": {"id": "tenant-42"},
    })
    .to_string()
}

struct Mounted {
    server: TestServer,
    hold: Arc<Hold>,
    provider: Provider,
    store_pool: PgPool,
}

#[derive(Clone)]
struct WidgetState {
    readiness: health::ReadinessReader,
    widgets: Arc<dyn CreateWidgets>,
}

impl FromRef<WidgetState> for health::ReadinessReader {
    fn from_ref(state: &WidgetState) -> Self {
        state.readiness.clone()
    }
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

fn widget_contract() -> HttpContract<WidgetState> {
    HttpContract::default()
        .merge(
            infra_http::idempotent_route!(
                create_widget,
                docs = "https://docs.example/widgets#create"
            )
            .expect("create composes"),
        )
        .unwrap()
        .merge(
            infra_http::idempotent_route!(
                replace_widget,
                docs = "https://docs.example/widgets#replace"
            )
            .expect("replace composes"),
        )
        .unwrap()
}

#[test]
fn widget_contract_declares_protected_json_operations() {
    let document = serde_json::to_value(
        HttpContract::application(bearer_document())
            .unwrap()
            .merge(widget_contract())
            .unwrap()
            .into_openapi()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        document["components"]["schemas"]["Widget"]["properties"]["id"]["type"],
        "integer"
    );
    for (path, method, operation_id, scopes) in [
        (WIDGETS, "post", "createWidget", json!(["widgets:write"])),
        ("/widgets/{id}", "put", "replaceWidget", json!([])),
    ] {
        let operation = &document["paths"][path][method];
        assert_eq!(operation["operationId"], operation_id);
        assert_eq!(operation["security"], json!([{ "bearerAuth": scopes }]));
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/NewWidget"
        );
        let success = &operation["responses"]["201"]["content"]["application/json"]["schema"];
        assert_eq!(success["$ref"], "#/components/schemas/Widget");
        assert_eq!(
            operation["responses"]["415"]["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
    }
}

impl Mounted {
    async fn on(pool: &PgPool, budget: Duration) -> Self {
        create_widgets(pool).await;
        let store_pool = template_pool(&dsn_for(pool).await, 4).await;
        Self::new(store_pool, budget).await
    }

    async fn new(store_pool: PgPool, budget: Duration) -> Self {
        let provider = Provider::start().await;
        let hold = Arc::new(Hold::default());
        let widgets: Arc<dyn CreateWidgets> = Arc::new(SqlWidgets {
            hold: Arc::clone(&hold),
        });
        let store = Store::new(store_pool.clone(), RETENTION).expect("retention fits PostgreSQL");
        let contract = widget_contract();
        let mounted = HttpContract::application(bearer_document())
            .unwrap()
            .nest(PRIMARY_PREFIX, contract.clone())
            .unwrap()
            .nest(SECONDARY_PREFIX, contract)
            .unwrap();
        assert_eq!(mounted.idempotent_operations(), 4);
        let app = mounted
            .with_probes(PRIMARY_PREFIX)
            .unwrap()
            .with_probes(SECONDARY_PREFIX)
            .unwrap()
            .build_with_idempotency_store(
                WidgetState {
                    readiness: readiness_reader(),
                    widgets,
                },
                Authentication::Bearer(Box::new(provider.verifier())),
                &options(budget),
                store,
            )
            .expect("the mounted auth contract builds");
        Self {
            server: TestServer::new(app.into_make_service()),
            hold,
            provider,
            store_pool,
        }
    }

    fn create(&self, token: &str, key: &str, input: &Value, request_id: &str) -> TestRequest {
        self.create_at(PRIMARY_WIDGETS, token, key, input, request_id)
    }

    fn create_at(
        &self,
        path: &str,
        token: &str,
        key: &str,
        input: &Value,
        request_id: &str,
    ) -> TestRequest {
        self.server
            .post(path)
            .authorization_bearer(token)
            .add_header(KEY, key)
            .add_header(REQUEST_ID_HEADER, request_id)
            .json(input)
    }

    fn replace(
        &self,
        id: i64,
        token: &str,
        key: &str,
        input: &Value,
        request_id: &str,
    ) -> TestRequest {
        self.server
            .put(&format!("{PRIMARY_WIDGETS}/{id}"))
            .authorization_bearer(token)
            .add_header(KEY, key)
            .add_header(REQUEST_ID_HEADER, request_id)
            .json(input)
    }

    async fn finish(self) {
        close_pools(&[&self.store_pool]).await;
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

fn problem(response: &TestResponse, status: StatusCode, code: &str, request_id: &str) -> Value {
    let body = problem_body(response, status, code);
    assert_eq!(
        response.maybe_header(REQUEST_ID_HEADER),
        Some(HeaderValue::from_str(request_id).expect("a request id"))
    );
    assert_eq!(body["request_id"], request_id, "{body}");
    body
}

fn command_timed_out(response: &TestResponse, request_id: &str) {
    let body = problem(
        response,
        StatusCode::GATEWAY_TIMEOUT,
        "gateway_timeout",
        request_id,
    );
    assert_eq!(
        body["detail"],
        concat!(
            "the idempotent request outcome is unknown. Retry the same request with the same ",
            "Idempotency-Key within the published retry window. If the window has expired, ",
            "reconcile the outcome before attempting the operation again",
        )
    );
    assert!(retry_after(response).is_none());
    assert!(response.maybe_header("idempotent-replayed").is_none());
}

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
    assert_eq!(
        response
            .headers()
            .get_all(CONTENT_LANGUAGE)
            .iter()
            .map(HeaderValue::as_bytes)
            .collect::<Vec<_>>(),
        vec![b"en".as_slice(), b"fr".as_slice()]
    );
    assert!(response.maybe_header(CONTENT_ENCODING).is_none());
    assert_eq!(
        response.maybe_header(CONTENT_DISPOSITION),
        Some(HeaderValue::from_static("attachment; filename=widget.json"))
    );
    assert_eq!(
        response.maybe_header(ETAG),
        Some(HeaderValue::from_str(&format!("\"widget-{}\"", body["id"])).expect("an etag"))
    );
    assert_eq!(
        response.maybe_header(LAST_MODIFIED),
        Some(HeaderValue::from_static("Sun, 06 Nov 1994 08:49:37 GMT"))
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

fn retry_after(response: &TestResponse) -> Option<String> {
    response.maybe_header(RETRY_AFTER).map(|value| {
        value
            .to_str()
            .expect("a visible Retry-After value")
            .to_owned()
    })
}

type MetricSeries = (String, Vec<(String, String)>);

struct CapturedMetrics {
    snapshotter: Snapshotter,
    counters: Mutex<BTreeMap<MetricSeries, u64>>,
    histograms: Mutex<BTreeMap<MetricSeries, usize>>,
}

impl CapturedMetrics {
    fn new(recorder: &DebuggingRecorder) -> Self {
        Self {
            snapshotter: recorder.snapshotter(),
            counters: Mutex::new(BTreeMap::new()),
            histograms: Mutex::new(BTreeMap::new()),
        }
    }

    fn drain(&self) {
        let entries = self.snapshotter.snapshot().into_vec();
        let mut counters = self.counters.lock().expect("metrics counter lock");
        let mut histograms = self.histograms.lock().expect("metrics histogram lock");
        for (key, _, _, value) in entries {
            let mut labels: Vec<(String, String)> = key
                .key()
                .labels()
                .map(|label| (label.key().to_owned(), label.value().to_owned()))
                .collect();
            labels.sort();
            let series = (key.key().name().to_owned(), labels);
            match value {
                DebugValue::Counter(delta) => {
                    *counters.entry(series).or_insert(0) += delta;
                }
                DebugValue::Histogram(samples) => {
                    *histograms.entry(series).or_insert(0) += samples.len();
                }
                DebugValue::Gauge(_) => {}
            }
        }
    }

    fn outcomes(&self) -> BTreeMap<String, u64> {
        self.drain();
        self.counters
            .lock()
            .expect("metrics counter lock")
            .iter()
            .filter(|((name, _), count)| name == HTTP_IDEMPOTENCY_OUTCOMES_METRIC && **count > 0)
            .filter_map(|((_, labels), count)| {
                let outcome = labels
                    .iter()
                    .find_map(|(key, value)| (key == "outcome").then_some(value.clone()))?;
                Some((outcome, *count))
            })
            .collect()
    }

    fn counter(&self, target: &str, required: &[(&str, &str)]) -> u64 {
        self.drain();
        self.counters
            .lock()
            .expect("metrics counter lock")
            .iter()
            .filter(|((name, labels), _)| {
                name == target
                    && required
                        .iter()
                        .all(|(k, v)| labels.iter().any(|(lk, lv)| lk == k && lv == v))
            })
            .map(|(_, count)| *count)
            .sum()
    }

    fn histogram_samples(&self, target: &str, required: &[(&str, &str)]) -> usize {
        self.drain();
        self.histograms
            .lock()
            .expect("metrics histogram lock")
            .iter()
            .filter(|((name, labels), _)| {
                name == target
                    && required
                        .iter()
                        .all(|(k, v)| labels.iter().any(|(lk, lv)| lk == k && lv == v))
            })
            .map(|(_, count)| *count)
            .sum()
    }
}

fn outcomes(captured: &CapturedMetrics) -> BTreeMap<String, u64> {
    captured.outcomes()
}

fn counts(expected: &[(&str, u64)]) -> BTreeMap<String, u64> {
    expected
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|&(outcome, count)| (outcome.to_owned(), count))
        .collect()
}

async fn until_free(request: impl Fn() -> TestRequest) -> (TestResponse, u64) {
    let mut conflicts = 0;
    let response =
        integration_tests::poll_until("the key is released", WAIT, RETRY_PAUSE, async || {
            let response = request().await;
            if response.status_code() == StatusCode::CONFLICT {
                conflicts += 1;
                None
            } else {
                Some(response)
            }
        })
        .await;
    (response, conflicts)
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
    Extension(credential): Extension<DelegationCredential>,
    headers: axum::http::HeaderMap,
) -> &'static str {
    assert!(!headers.contains_key(axum::http::header::AUTHORIZATION));
    calls.lock().unwrap().push(credential);
    "granted"
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

#[utoipa::path(head, path = "/_test/authorization", security(), responses((status = 204, description = "public HEAD")))]
async fn public_head(credential: Option<Extension<DelegationCredential>>) -> StatusCode {
    assert!(credential.is_none());
    StatusCode::NO_CONTENT
}

#[utoipa::path(get, path = "/_test/inherited", responses((status = 200, description = "protected")))]
async fn inherited_get() -> &'static str {
    "protected"
}

#[tokio::test]
async fn final_document_controls_head_policy_and_preserves_native_fallbacks() {
    let provider = Provider::start().await;
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
            Authentication::Bearer(Box::new(provider.verifier())),
            &options(BUDGET),
        )
        .unwrap();
    let server = TestServer::new(app.into_make_service());
    let explicit = server
        .method(axum::http::Method::HEAD, "/_test/authorization")
        .authorization("Bearer broken token")
        .await;
    assert_eq!(explicit.status_code(), StatusCode::NO_CONTENT);
    let implicit = server
        .method(axum::http::Method::HEAD, "/_test/inherited")
        .await;
    assert_eq!(implicit.status_code(), StatusCode::UNAUTHORIZED);
    assert_eq!(implicit.header(WWW_AUTHENTICATE), "Bearer realm=\"api\"");
    assert_eq!(
        implicit.header(axum::http::header::CONTENT_TYPE),
        "application/problem+json"
    );
    let accepted = server
        .method(axum::http::Method::HEAD, "/_test/inherited")
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
            assert_eq!(response.header(axum::http::header::ALLOW), "GET,HEAD");
        }
    }
    provider.stop().await;
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
        .build((), Authentication::Bearer(Box::new(verifier)), &options)
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn cached_verification_succeeds_in_the_final_hundred_milliseconds() {
    let provider = Provider::start().await;
    let verifier = provider.verifier_with_cache(Some(
        IntrospectionCacheOptions::try_new(16, Duration::from_secs(30)).unwrap(),
    ));
    let authorization = format!("Bearer {ALICE}");
    verifier
        .verify_fixture_authorization(authorization.as_bytes())
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
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let provider = Provider::start().await;
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
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let provider = Provider::start().await;
    let app = HttpContract::application(bearer_document())
        .unwrap()
        .merge(infra_http::http_route!(authorized_without_idempotency).unwrap())
        .unwrap()
        .with_probes("/")
        .unwrap()
        .build(
            readiness_reader(),
            Authentication::Bearer(Box::new(provider.verifier())),
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
    let provider = Provider::start().await;
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
            Authentication::Bearer(Box::new(provider.verifier())),
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
        (axum::http::Method::GET, StatusCode::OK),
        (axum::http::Method::HEAD, StatusCode::NO_CONTENT),
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

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_authentication_failures_answer_before_the_key_is_read(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    let malformed_key = "\"k-1";
    let inactive = format!("Bearer {INACTIVE}");
    for (authorization, status, code, challenge) in [
        (
            None,
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            Some("Bearer realm=\"api\""),
        ),
        (
            Some("Basic Zm9vOmJhcg=="),
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            Some("Bearer realm=\"api\""),
        ),
        (
            Some("Bearer token token"),
            StatusCode::BAD_REQUEST,
            "authentication_malformed",
            Some("Bearer error=\"invalid_request\""),
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
            .post(PRIMARY_WIDGETS)
            .add_header(KEY, malformed_key)
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
    assert_eq!(outcomes(&captured), counts(&[]));

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
    assert_eq!(outcomes(&captured), counts(&[]));

    let refused = mounted
        .create(ALICE, malformed_key, &input, "req-key")
        .await;
    problem(&refused, StatusCode::BAD_REQUEST, "bad_request", "req-key");
    assert_eq!(outcomes(&captured), counts(&[("invalid_key", 1)]));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_the_key_grammar_is_exact(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    let long = "k".repeat(256);
    let invalid: [&[&[u8]]; 8] = [
        &[],
        &[b""],
        &[long.as_bytes()],
        &[b"k 1"],
        &[b"\"unterminated"],
        &["k-\u{e9}".as_bytes()],
        &[b"k-1", b"k-2"],
        &[b"k-1", b"k-1"],
    ];
    for values in invalid {
        let mut request = mounted
            .server
            .post(PRIMARY_WIDGETS)
            .authorization_bearer(ALICE)
            .add_header(REQUEST_ID_HEADER, "req-key")
            .json(&input);
        for value in values {
            request = request.add_header(KEY, HeaderValue::from_bytes(value).expect("a field"));
        }
        let response = request.await;
        assert_eq!(response.header("link"), KEY_RULES_LINK);
        let body = problem(&response, StatusCode::BAD_REQUEST, "bad_request", "req-key");
        assert_eq!(body["detail"], KEY_DETAIL);
        assert_eq!(
            body["invalid_params"],
            json!([{"name": "header.Idempotency-Key", "reason": KEY_REASON}])
        );
    }
    assert_eq!(outcomes(&captured), counts(&[("invalid_key", 8)]));

    for key in [
        "k".repeat(255),
        "k".to_owned(),
        "!#$%&'*+,-./:=?@[]^_`{|}~09AZaz".to_owned(),
        "\"quoted key\"".to_owned(),
    ] {
        created(
            &mounted.create(ALICE, &key, &input, "req-valid").await,
            "req-valid",
        );
    }
    assert_eq!(
        outcomes(&captured),
        counts(&[("invalid_key", 8), ("executed", 4)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 4);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_success_replays_byte_for_byte_behind_current_authorization(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    let wrong_media_type = mounted
        .create(ALICE, "k-1", &input, "req-media-type")
        .content_type("text/plain")
        .await;
    let body = problem(
        &wrong_media_type,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
        "req-media-type",
    );
    assert_eq!(body["status"], 415);

    let first = mounted.create(ALICE, "k-1", &input, "req-first").await;
    let widget = created(&first, "req-first");
    assert!(
        first.maybe_header("idempotent-replayed").is_none(),
        "a fresh success must not carry handler-supplied replay metadata"
    );
    assert_eq!(
        outcomes(&captured),
        counts(&[("executed", 1), ("not_stored", 1)])
    );

    let reordered = mounted
        .server
        .post(PRIMARY_WIDGETS)
        .authorization_bearer(ALICE)
        .add_header(KEY, "k-1")
        .add_header(REQUEST_ID_HEADER, "req-replay")
        .content_type("application/json")
        .bytes(Bytes::from_static(
            b"{ \"color\" : \"red\",\n  \"name\" : \"gizmo\" }",
        ))
        .await;
    problem(
        &reordered,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-replay",
    );
    assert_eq!(
        outcomes(&captured),
        counts(&[("executed", 1), ("key_mismatch", 1), ("not_stored", 1)])
    );

    let exact = mounted
        .create(ALICE, "\"k-1\";extension=1", &input, "req-exact")
        .await;
    created(&exact, "req-exact");
    assert_eq!(
        exact.maybe_header("idempotent-replayed"),
        Some(HeaderValue::from_static("true")),
        "only the decoded stored response becomes a replay"
    );
    assert_eq!(exact.as_bytes(), first.as_bytes());

    let changed_header = mounted
        .create(ALICE, "k-1", &input, "req-header")
        .add_header("x-widget-name", "different")
        .await;
    problem(
        &changed_header,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-header",
    );

    let refused = mounted
        .create(ALICE_REVOKED, "k-1", &input, "req-refused")
        .await;
    let body = problem_body(&refused, StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(body["detail"], "widget creation is not permitted");
    assert_eq!(
        outcomes(&captured),
        counts(&[
            ("executed", 1),
            ("key_mismatch", 2),
            ("replayed", 1),
            ("not_stored", 2)
        ])
    );

    let changed = json!({"name": "gizmo", "color": "blue"});
    let mismatch = mounted.create(ALICE, "k-1", &changed, "req-changed").await;
    problem(
        &mismatch,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-changed",
    );
    assert!(retry_after(&mismatch).is_none());
    assert_eq!(mismatch.header("link"), KEY_RULES_LINK);
    let other = mounted.create(BOB, "k-1", &input, "req-bob").await;
    assert_ne!(created(&other, "req-bob")["id"], widget["id"]);
    let again = mounted.create(ALICE, "k-1", &input, "req-again").await;
    created(&again, "req-again");
    assert_eq!(
        again.maybe_header("idempotent-replayed"),
        Some(HeaderValue::from_static("true"))
    );
    assert_eq!(again.as_bytes(), first.as_bytes());
    assert_eq!(
        outcomes(&captured),
        counts(&[
            ("executed", 2),
            ("replayed", 2),
            ("key_mismatch", 3),
            ("not_stored", 2)
        ])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 2);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_request_identity_keeps_original_uri_resource_and_operation(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    let first_resource = mounted
        .replace(1, ALICE, "k-resource", &input, "req-resource-one")
        .await;
    created(&first_resource, "req-resource-one");
    let second_resource = mounted
        .replace(2, ALICE, "k-resource", &input, "req-resource-two")
        .await;
    problem(
        &second_resource,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-resource-two",
    );

    let post = mounted
        .create(ALICE, "k-operation", &input, "req-post")
        .await;
    created(&post, "req-post");
    let put = mounted
        .replace(1, ALICE, "k-operation", &input, "req-put")
        .await;
    problem(
        &put,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-put",
    );

    let primary = mounted
        .create_at(PRIMARY_WIDGETS, ALICE, "k-original", &input, "req-primary")
        .await;
    created(&primary, "req-primary");
    let secondary = mounted
        .create_at(
            SECONDARY_WIDGETS,
            ALICE,
            "k-original",
            &input,
            "req-secondary",
        )
        .await;
    problem(
        &secondary,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-secondary",
    );

    let ordered = format!("{PRIMARY_WIDGETS}?a=1&b=2");
    let first_query = mounted
        .create_at(&ordered, ALICE, "k-query", &input, "req-query-one")
        .await;
    created(&first_query, "req-query-one");
    let reordered = format!("{PRIMARY_WIDGETS}?b=2&a=1");
    let second_query = mounted
        .create_at(&reordered, ALICE, "k-query", &input, "req-query-two")
        .await;
    problem(
        &second_query,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-query-two",
    );

    assert_eq!(
        outcomes(&captured),
        counts(&[("executed", 4), ("key_mismatch", 4)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 4);
    mounted.finish().await;
}

#[tokio::test]
async fn p9_unfinished_authenticated_upload_returns_the_declared_timeout() {
    use hyper::service::Service as _;
    use hyper_util::service::TowerToHyperService;

    let provider = Provider::start().await;
    let verifier = provider.verifier_with_cache(Some(
        IntrospectionCacheOptions::try_new(16, Duration::from_secs(30)).unwrap(),
    ));
    let authorization = format!("Bearer {ALICE}");
    verifier
        .verify_fixture_authorization(authorization.as_bytes())
        .await
        .unwrap();
    let pool = PgPool::connect_lazy("postgres://localhost/unused").expect("lazy pool");
    let store = Store::new(pool, RETENTION).unwrap();
    let contract = HttpContract::application(bearer_document())
        .unwrap()
        .merge(widget_contract())
        .unwrap()
        .with_probes("")
        .unwrap();
    let document = serde_json::to_value(contract.clone().into_openapi().unwrap()).unwrap();
    let app = contract
        .build_with_idempotency_store(
            WidgetState {
                readiness: readiness_reader(),
                widgets: Arc::new(SqlWidgets {
                    hold: Arc::default(),
                }),
            },
            Authentication::Bearer(Box::new(verifier)),
            &HardenOptions {
                max_body_bytes: MAX_BODY_BYTES,
                request_timeout: BUDGET,
                max_in_flight: None,
                admission_wait: Duration::ZERO,
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
            },
            store,
        )
        .unwrap();
    let upload =
        Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>());
    let request = axum::http::Request::post(WIDGETS)
        .header(axum::http::header::AUTHORIZATION, authorization)
        .header(KEY, "upload")
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(upload)
        .unwrap();
    tokio::time::pause();
    let response = TowerToHyperService::new(app).call(request).await.unwrap();
    tokio::time::resume();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let media = response.headers()[axum::http::header::CONTENT_TYPE]
        .to_str()
        .unwrap();
    assert_eq!(media, "application/problem+json");
    let reference =
        document["paths"][WIDGETS]["post"]["responses"][response.status().as_str()]["$ref"]
            .as_str()
            .unwrap();
    let declared = document
        .pointer(reference.strip_prefix('#').unwrap())
        .unwrap();
    assert!(declared["content"].get(media).is_some());
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["code"],
        "request_timeout"
    );
    provider.stop().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_serialization_failure_rolls_back_the_widget_and_releases_the_key(pool: PgPool) {
    let mounted = Mounted::on(&pool, BUDGET).await;
    let failed = mounted
        .create(
            ALICE,
            "k-encode",
            &json!({"name": "gizmo", "color": UNSERIALIZABLE_COLOR}),
            "req-encode",
        )
        .await;
    let body = problem(
        &failed,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "req-encode",
    );
    assert_eq!(body["status"], 500);
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM http_idempotency_records").await,
        0
    );
    let input = json!({"name": "gizmo", "color": "red"});
    let first = mounted.create(ALICE, "k-encode", &input, "req-first").await;
    let replay = mounted
        .create(ALICE, "k-encode", &input, "req-replay")
        .await;
    assert_eq!(created(&first, "req-first"), created(&replay, "req-replay"));
    assert_eq!(replay.header("idempotent-replayed"), "true");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM http_idempotency_records").await,
        1
    );
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn terminal_refusal_survives_a_changed_business_condition(pool: PgPool) {
    create_widgets(&pool).await;
    pool.execute("CREATE UNIQUE INDEX widgets_unique_name ON widgets (name)")
        .await
        .unwrap();
    pool.execute("INSERT INTO widgets (name, color) VALUES ('taken', 'seed')")
        .await
        .unwrap();
    let (proxy, proxied) = CommitProxy::start_for(&pool).await;
    let mounted = Mounted::new(template_pool(&proxied, 4).await, BUDGET).await;
    let input = json!({"name": "taken", "color": "unique"});
    let key = "k-refused";

    proxy.arm(Fault::ForwardThenDrop, Boundary::Commit(None));
    let first = mounted.create(ALICE, key, &input, "req-lost").await;
    assert_eq!(proxy.fired(), Some(Fault::ForwardThenDrop));
    problem(
        &first,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_unavailable",
        "req-lost",
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    let original: Vec<u8> =
        sqlx::query_scalar("SELECT body FROM http_idempotency_records WHERE status = 409")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.execute("DELETE FROM widgets WHERE color = 'seed'")
        .await
        .unwrap();
    let changed = json!({"name": "other", "color": "unique"});
    let mismatch = mounted.create(ALICE, key, &changed, "req-mismatch").await;
    problem(
        &mismatch,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_mismatch",
        "req-mismatch",
    );
    let replay = mounted.create(ALICE, key, &input, "req-replay").await;
    let body = problem_body(&replay, StatusCode::CONFLICT, "already_exists");
    assert_eq!(body["detail"], "the widget name is taken");
    assert_eq!(replay.header(REQUEST_ID_HEADER), "req-replay");
    assert_eq!(replay.header("idempotent-replayed"), "true");
    assert_eq!(replay.as_bytes().as_ref(), original.as_slice());
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    let fresh = mounted.create(ALICE, "k-fresh", &input, "req-fresh").await;
    created(&fresh, "req-fresh");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn an_invalid_refusal_rolls_back_the_post(pool: PgPool) {
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "wrong-status", "color": "invalid-refusal"});
    let first = mounted.create(ALICE, "k-304", &input, "req-invalid").await;
    let body = problem(
        &first,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "req-invalid",
    );
    assert_eq!(body["detail"], "request failed");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM http_idempotency_records").await,
        0
    );
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_rolled_back_work_and_undecodable_records_are_never_replayed(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;

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
    assert_eq!(outcomes(&captured), counts(&[("not_stored", 2)]));

    let huge = json!({"name": "x".repeat(STORED_BODY_BOUND), "color": "red"});
    let unstorable = mounted.create(ALICE, "k-3", &huge, "req-huge").await;
    let body = problem(
        &unstorable,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "req-huge",
    );
    assert_eq!(body["detail"], "request failed");
    assert_eq!(
        outcomes(&captured),
        counts(&[("not_stored", 2), ("unstorable", 1)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);

    let input = json!({"name": "gizmo", "color": "red"});
    let stored = mounted.create(ALICE, "k-3", &input, "req-stored").await;
    created(&stored, "req-stored");

    let corrupted = sqlx::query(
        "UPDATE http_idempotency_records SET headers = \
         ARRAY[ROW('x-ignored', convert_to('bad', 'UTF8'))::http_idempotency_header_pair] \
         WHERE body = $1",
    )
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
        outcomes(&captured),
        counts(&[
            ("not_stored", 2),
            ("unstorable", 1),
            ("executed", 1),
            ("integrity", 2)
        ])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_held_key_answers_in_progress_with_a_retry_hint(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});
    let changed = json!({"name": "gizmo", "color": "blue"});

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
        outcomes(&captured),
        counts(&[("in_progress", 2), ("executed", 1), ("replayed", 1)])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_the_database_cutoff_answers_unavailable_and_the_same_key_recovers(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let mounted = Mounted::on(&pool, SHORT_BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    mounted.hold.arm();
    let advance = async {
        mounted.hold.entered().await;
        tokio::time::pause();
    };
    let (expired, ()) = tokio::join!(mounted.create(ALICE, "k-5", &input, "req-expired"), advance);
    problem(
        &expired,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_unavailable",
        "req-expired",
    );
    assert_eq!(retry_after(&expired).as_deref(), Some("1"));
    assert_eq!(outcomes(&captured), counts(&[("unavailable", 1)]));
    tokio::time::resume();
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);

    let (retry, conflicts) = until_free(|| mounted.create(ALICE, "k-5", &input, "req-retry")).await;
    created(&retry, "req-retry");

    let input = json!({"name": "gizmo", "color": HANDLER_TIMEOUT_COLOR});
    mounted.hold.arm();
    let pause_on_entry = async {
        mounted.hold.entered().await;
        tokio::time::pause();
    };
    let (abandoned, ()) = tokio::join!(
        mounted.create(ALICE, "k-handler", &input, "req-abandoned"),
        pause_on_entry
    );
    command_timed_out(&abandoned, "req-abandoned");
    tokio::time::resume();
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    let (stored, retry_conflicts) =
        until_free(|| mounted.create(ALICE, "k-handler", &input, "req-recovered")).await;
    created(&stored, "req-recovered");
    let mut connections = Vec::new();
    for _ in 0..mounted.store_pool.options().get_max_connections() {
        connections.push(
            infra_postgres::acquire(&mounted.store_pool, "hold every idempotency connection")
                .await
                .unwrap(),
        );
    }
    let blocked = mounted
        .create(ALICE, "k-handler", &input, "req-acquiring")
        .await;
    drop(connections);
    command_timed_out(&blocked, "req-acquiring");
    let replay = mounted
        .create(ALICE, "k-handler", &input, "req-replay")
        .await;
    assert_eq!(replay.as_bytes(), stored.as_bytes());
    assert_eq!(
        replay.maybe_header("idempotent-replayed"),
        Some(HeaderValue::from_static("true"))
    );
    assert_eq!(
        outcomes(&captured),
        counts(&[
            ("unavailable", 1),
            ("in_progress", conflicts + retry_conflicts),
            ("executed", 2),
            ("replayed", 1),
            ("abandoned", 2)
        ])
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 2);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_read_only_writer_answers_unavailable(pool: PgPool) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    create_widgets(&pool).await;
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
    assert_eq!(outcomes(&captured), counts(&[("unavailable", 1)]));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_postgres_timeouts_allow_a_same_key_retry(pool: PgPool) {
    create_widgets(&pool).await;
    let dsn = dsn_for(&pool).await;
    let input = json!({"name": "gizmo", "color": "red"});
    let mut lock_connection = infra_postgres::acquire(&pool, "test lock holder")
        .await
        .expect("a lock-holder connection");

    for (setting, sqlstate, expected_rows) in [
        ("lock_timeout", "55P03", 1),
        ("transaction_timeout", "25P04", 2),
    ] {
        pool.execute(sqlx::AssertSqlSafe(format!(
            "DO $$ BEGIN \
             EXECUTE format('ALTER DATABASE %I SET lock_timeout = 0', current_database()); \
             EXECUTE format('ALTER DATABASE %I SET transaction_timeout = 0', current_database()); \
             EXECUTE format('ALTER DATABASE %I SET %I = %L', current_database(), '{setting}', '1s'); \
             END $$"
        )))
        .await
        .expect("set the timeout for new sessions");
        let mounted = Mounted::new(template_pool(&dsn, 1).await, BUDGET).await;
        let recorder = DebuggingRecorder::new();
        let captured = CapturedMetrics::new(&recorder);
        let _local = metrics::set_default_local_recorder(&recorder);

        let mut holder = sqlx::Connection::begin(&mut *lock_connection)
            .await
            .expect("a table-lock holder");
        (&mut *holder)
            .execute("LOCK TABLE http_idempotency_records IN ACCESS EXCLUSIVE MODE")
            .await
            .expect("block idempotency arbitration");
        let refused = mounted.create(ALICE, setting, &input, "req-timeout").await;
        holder.rollback().await.expect("release the table lock");

        assert!(
            captured.histogram_samples(
                "db_client_operation_duration_seconds",
                &[
                    ("db.query.summary", "lock and read idempotency record"),
                    ("error.type", sqlstate),
                ],
            ) > 0
        );
        problem(
            &refused,
            StatusCode::SERVICE_UNAVAILABLE,
            "idempotency_unavailable",
            "req-timeout",
        );
        assert_eq!(retry_after(&refused).as_deref(), Some("1"));
        assert_eq!(count(&pool, WIDGET_ROWS).await, expected_rows - 1);

        let retry = mounted.create(ALICE, setting, &input, "req-retry").await;
        created(&retry, "req-retry");
        assert_eq!(count(&pool, WIDGET_ROWS).await, expected_rows);
        mounted.finish().await;
    }
}

type EventFields = BTreeMap<String, String>;

#[derive(Clone, Default)]
struct CommandFailures(Arc<Mutex<Vec<EventFields>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CommandFailures {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().fields().field("failure_class").is_none() {
            return;
        }
        let mut fields = EventFields::new();
        event.record(
            &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                fields.insert(field.name().to_owned(), format!("{value:?}"));
            },
        );
        if fields
            .get("message")
            .is_some_and(|message| message == "http_idempotency_command_failed")
        {
            self.0.lock().expect("command failure events").push(fields);
        }
    }
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn a_locked_command_keeps_its_timeout_response_and_a_tighter_server_limit(pool: PgPool) {
    create_widgets(&pool).await;
    pool.execute(
        "CREATE SEQUENCE widget_statement_ms MINVALUE 0; CREATE SEQUENCE cancelled_widget_writes;
         CREATE FUNCTION block_widget_write() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
             PERFORM setval('widget_statement_ms', setting::bigint)
                 FROM pg_settings WHERE name = 'statement_timeout';
             PERFORM pg_advisory_xact_lock(734091);
             RETURN NEW;
         EXCEPTION WHEN query_canceled THEN
             PERFORM nextval('cancelled_widget_writes');
             RAISE;
         END $$;
         CREATE TRIGGER block_widget_write BEFORE INSERT ON widgets
             FOR EACH ROW EXECUTE FUNCTION block_widget_write()",
    )
    .await
    .unwrap();
    let mounted = Mounted::new(template_pool(&dsn_for(&pool).await, 1).await, BUDGET).await;
    let input = json!({"name": "gizmo", "color": HANDLER_TIMEOUT_COLOR});
    let mut connection = infra_postgres::acquire(&pool, "hold widget inserts")
        .await
        .unwrap();
    let mut holder = sqlx::Connection::begin(&mut *connection).await.unwrap();
    (&mut *holder)
        .execute("SELECT pg_advisory_xact_lock(734091)")
        .await
        .unwrap();

    mounted
        .store_pool
        .execute("SET statement_timeout = '150ms'")
        .await
        .unwrap();
    let inherited = mounted
        .create(ALICE, "deadline", &input, "inherited-limit")
        .await;
    problem(
        &inherited,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_unavailable",
        "inherited-limit",
    );
    assert_eq!(retry_after(&inherited).as_deref(), Some("1"));
    assert_eq!(
        count(&pool, "SELECT last_value FROM widget_statement_ms").await,
        150
    );

    mounted
        .store_pool
        .execute("SET statement_timeout = '8s'")
        .await
        .unwrap();
    let limited = mounted
        .create(ALICE, "deadline", &input, "command-limit")
        .await;
    command_timed_out(&limited, "command-limit");
    let command_ms = count(&pool, "SELECT last_value FROM widget_statement_ms").await;
    assert!(
        (1..=1000).contains(&command_ms),
        "widget INSERT received {command_ms} ms"
    );
    integration_tests::poll_until(
        "the server cancelled both inserts with the lock still held",
        Duration::from_secs(2),
        RETRY_PAUSE,
        async || {
            (count(&pool, "SELECT last_value FROM cancelled_widget_writes").await == 2)
                .then_some(())
        },
    )
    .await;
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM http_idempotency_records").await,
        0
    );
    holder.rollback().await.unwrap();
    let retry = mounted.create(ALICE, "deadline", &input, "retry").await;
    let replay = mounted.create(ALICE, "deadline", &input, "replay").await;
    assert_eq!(created(&retry, "retry"), created(&replay, "replay"));
    assert_eq!(replay.header("idempotent-replayed"), "true");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_business_insert_errors_are_classified_and_release_the_key(pool: PgPool) {
    create_widgets(&pool).await;
    pool.execute("ALTER TABLE widgets ADD CONSTRAINT reject_widget_name CHECK (name <> 'gizmo')")
        .await
        .expect("an INSERT constraint violation after the lock is released");
    pool.execute(
        "DO $$ BEGIN EXECUTE format(\
         'ALTER DATABASE %I SET lock_timeout = %L', current_database(), '1s'); END $$",
    )
    .await
    .expect("set the lock timeout for writer sessions");
    let mounted = Mounted::new(template_pool(&dsn_for(&pool).await, 1).await, BUDGET).await;
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    let failures = CommandFailures::default();
    let _subscriber =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(failures.clone()));
    let input = json!({"name": "gizmo", "color": "red"});
    let key = "k-business-insert";
    let records = "SELECT count(*) FROM http_idempotency_records";

    let mut connection = infra_postgres::acquire(&pool, "hold the widget table lock")
        .await
        .expect("a lock-holder connection");
    let mut holder = sqlx::Connection::begin(&mut *connection)
        .await
        .expect("a table-lock holder");
    (&mut *holder)
        .execute("LOCK TABLE widgets IN ACCESS EXCLUSIVE MODE")
        .await
        .expect("block the widget INSERT");
    let unavailable = mounted.create(ALICE, key, &input, "req-locked").await;
    holder.rollback().await.expect("release the widget lock");

    problem(
        &unavailable,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_unavailable",
        "req-locked",
    );
    assert_eq!(retry_after(&unavailable).as_deref(), Some("1"));
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    assert_eq!(count(&pool, records).await, 0);

    let internal = mounted.create(ALICE, key, &input, "req-constraint").await;
    let body = problem(
        &internal,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "req-constraint",
    );
    assert_eq!(body["detail"], "request failed");
    assert!(retry_after(&internal).is_none());
    assert_eq!(count(&pool, WIDGET_ROWS).await, 0);
    assert_eq!(count(&pool, records).await, 0);
    let recorded = std::mem::take(&mut *failures.0.lock().expect("command failure events"));
    assert_eq!(
        recorded,
        [("unavailable", "55P03"), ("internal", "23514")].map(|(class, sqlstate)| {
            [
                ("message", "http_idempotency_command_failed"),
                ("operation", "\"createWidget\""),
                ("failure_class", class),
                ("sqlstate", &format!("{sqlstate:?}")),
                ("cause", "\"database\""),
            ]
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<EventFields>()
        })
    );

    pool.execute("ALTER TABLE widgets DROP CONSTRAINT reject_widget_name")
        .await
        .expect("remove the permanent insert fault");
    let retry = mounted.create(ALICE, key, &input, "req-retry").await;
    let replay = mounted.create(ALICE, key, &input, "req-replay").await;
    assert_eq!(created(&retry, "req-retry"), created(&replay, "req-replay"));
    assert!(retry.maybe_header("idempotent-replayed").is_none());
    assert_eq!(replay.header("idempotent-replayed"), "true");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    assert_eq!(count(&pool, records).await, 1);
    assert_eq!(
        outcomes(&captured),
        counts(&[
            ("unavailable", 1),
            ("internal", 1),
            ("executed", 1),
            ("replayed", 1)
        ])
    );
    mounted.finish().await;
}

async fn uncertain_commit(pool: PgPool, fault: Fault) {
    let recorder = DebuggingRecorder::new();
    let captured = CapturedMetrics::new(&recorder);
    let _local = metrics::set_default_local_recorder(&recorder);
    create_widgets(&pool).await;
    let (proxy, proxied) = CommitProxy::start_for(&pool).await;
    let mounted = Mounted::new(template_pool(&proxied, 4).await, BUDGET).await;
    let input = json!({"name": "gizmo", "color": "red"});

    proxy.arm(fault, Boundary::Commit(None));
    let first = bounded("the uncertain request answers", async {
        mounted
            .create(ALICE, "k-uncertain", &input, "req-uncertain")
            .await
    })
    .await;
    assert_eq!(proxy.fired(), Some(fault), "the fault reached COMMIT");
    let committed = fault != Fault::DropBeforeForward;
    assert_eq!(count(&pool, WIDGET_ROWS).await, i64::from(committed));
    let original: Option<Vec<u8>> = sqlx::query_scalar("SELECT body FROM http_idempotency_records")
        .fetch_optional(&pool)
        .await
        .expect("read the actual durable response");
    assert_eq!(
        original.is_some(),
        committed,
        "effect and response share fate"
    );
    problem(
        &first,
        StatusCode::SERVICE_UNAVAILABLE,
        "idempotency_unavailable",
        "req-uncertain",
    );
    assert_eq!(retry_after(&first).as_deref(), Some("1"));
    assert!(first.maybe_header("idempotent-replayed").is_none());
    assert_eq!(outcomes(&captured), counts(&[("unavailable", 1)]));

    let (retry, conflicts) =
        until_free(|| mounted.create(ALICE, "k-uncertain", &input, "req-retry")).await;
    created(&retry, "req-retry");
    if let Some(original) = original {
        assert_eq!(retry.as_bytes().as_ref(), original.as_slice());
        assert_eq!(
            retry.maybe_header("idempotent-replayed"),
            Some(HeaderValue::from_static("true"))
        );
    } else {
        assert!(retry.maybe_header("idempotent-replayed").is_none());
    }
    assert_eq!(
        count(&pool, WIDGET_ROWS).await,
        1,
        "one business effect commits"
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM http_idempotency_records").await,
        1
    );
    assert_eq!(
        outcomes(&captured),
        counts(&[
            ("unavailable", 1),
            ("in_progress", conflicts),
            (if committed { "replayed" } else { "executed" }, 1),
        ]),
    );
    let replay = mounted
        .create(ALICE, "k-uncertain", &input, "req-replay")
        .await;
    created(&replay, "req-replay");
    assert_eq!(replay.as_bytes(), retry.as_bytes());
    assert_eq!(
        replay.maybe_header("idempotent-replayed"),
        Some(HeaderValue::from_static("true"))
    );
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    mounted.finish().await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_command_timeout_awaiting_commit_ack_replays_the_committed_effect(pool: PgPool) {
    create_widgets(&pool).await;
    let (proxy, proxied) = CommitProxy::start_for(&pool).await;
    let mounted = Mounted::new(template_pool(&proxied, 4).await, BUDGET).await;
    let input = json!({"name": "gizmo", "color": HANDLER_TIMEOUT_COLOR});

    proxy.arm_ready_hold(ReadyBoundary::Commit);
    let pause_on_commit = async {
        proxy.ready_held().await;
        tokio::time::pause();
    };
    let (timed_out, ()) = tokio::join!(
        mounted.create(ALICE, "k-commit-timeout", &input, "req-commit-timeout"),
        pause_on_commit
    );
    tokio::time::resume();
    command_timed_out(&timed_out, "req-commit-timeout");

    let original: Vec<u8> = sqlx::query_scalar("SELECT body FROM http_idempotency_records")
        .fetch_one(&pool)
        .await
        .expect("PostgreSQL committed the response while the proxy held its acknowledgement");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    proxy.release_ready();

    let replay = mounted
        .create(ALICE, "k-commit-timeout", &input, "req-replay")
        .await;
    created(&replay, "req-replay");
    assert_eq!(replay.as_bytes().as_ref(), original.as_slice());
    assert_eq!(replay.header("idempotent-replayed"), "true");
    assert_eq!(count(&pool, WIDGET_ROWS).await, 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM http_idempotency_records").await,
        1
    );
    mounted.finish().await;
    proxy.shutdown().await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_lost_commit_ack_replays_the_committed_effect(pool: PgPool) {
    uncertain_commit(pool, Fault::ForwardThenDrop).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_commit_not_forwarded_retries_the_rolled_back_effect(pool: PgPool) {
    uncertain_commit(pool, Fault::DropBeforeForward).await;
}

#[sqlx::test(migrator = "migrate::MIGRATOR")]
async fn p9_a_corrupt_commit_ack_keeps_uncertainty_and_replays(pool: PgPool) {
    uncertain_commit(pool, Fault::ForwardThenCorruptReady).await;
}

fn readiness_reader() -> health::ReadinessReader {
    Readiness::new(
        Vec::new(),
        RefreshPolicy::try_new(
            Duration::from_secs(1),
            Duration::from_secs(1),
            NonZeroU32::MIN,
        )
        .unwrap(),
    )
    .control()
    .reader()
}
