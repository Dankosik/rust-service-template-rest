use std::time::Duration;

use axum::ServiceExt as _;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum_test::TestServer;
use infra_bearerauthn::IntrospectionCacheOptions;
use infra_bearerauthn::test_support::verify_fixture_authorization;
use infra_http::authn::ProtectedOperationProblemResponses;
use infra_http::extract::{Json, Path};
use infra_http::idempotency::{
    CommandInput, CreatedText, FreshCommand, Rollback, TerminalProblem, Tx,
};
use infra_http::{
    Authentication, DelegationCredential, HardenOptions, HttpContract, Principal, Problem,
    VerifiedDelegation, VerifiedPrincipal,
};
use infra_idempotency_store::Store;
use infra_postgres::test_support::connect_lazy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower::ServiceExt as _;
use utoipa::ToSchema;

use super::{
    ALICE, BUDGET, MAX_BODY_BYTES, Provider, bearer_document, options, problem_body,
    readiness_reader,
};

const WIDGETS: &str = "/widgets";
const KEY: &str = "idempotency-key";
const RETENTION: Duration = Duration::from_hours(1);

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

infra_http::created_json_response!(CreatedWidget(Widget));

struct UnreachedCommand;

impl FreshCommand for UnreachedCommand {
    type Success = CreatedWidget;
    type Terminal = TerminalProblem;

    fn execute(
        self,
        _: &mut Tx<'_>,
    ) -> impl Future<Output = Result<Self::Success, Rollback<Self::Terminal>>> + Send {
        std::future::poll_fn(|_| unreachable!("DB-free contract tests fail before execution"))
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
    _: Principal,
    _: Option<DelegationCredential>,
    Json(_input): Json<NewWidget>,
    _: CommandInput,
) -> Result<UnreachedCommand, Problem> {
    Ok(UnreachedCommand)
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
    VerifiedPrincipal(_principal): VerifiedPrincipal,
    VerifiedDelegation(_delegation): VerifiedDelegation,
    Path(_id): Path<i64>,
    Json(_input): Json<NewWidget>,
) -> Result<UnreachedCommand, Problem> {
    Ok(UnreachedCommand)
}

fn widget_contract() -> HttpContract<health::ReadinessReader> {
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

struct MissingDelegationCommand;

impl FreshCommand for MissingDelegationCommand {
    type Success = CreatedText;
    type Terminal = std::convert::Infallible;

    fn execute(
        self,
        _: &mut Tx<'_>,
    ) -> impl Future<Output = Result<Self::Success, Rollback>> + Send {
        std::future::poll_fn(|_| unreachable!("missing delegation fails before execution"))
    }
}

#[utoipa::path(
    post,
    path = "/_test/missing-delegation",
    operation_id = "missingDelegation",
    security(("bearerAuth" = [])),
    extensions(("x-security-decision" = json!({
        "exposure": "protected",
        "rationale": "test-only idempotent route verifying fail-closed VerifiedDelegation extraction"
    }))),
    responses(ProtectedOperationProblemResponses)
)]
async fn missing_delegation(_: VerifiedDelegation) -> Result<MissingDelegationCommand, Problem> {
    Ok(MissingDelegationCommand)
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

#[tokio::test]
async fn idempotent_verified_delegation_without_route_opt_in_is_a_sanitized_internal_failure() {
    let provider = Provider::start();
    let pool = connect_lazy("postgres://localhost/unused").expect("lazy pool does no I/O");
    let store = Store::try_new(pool, RETENTION).expect("retention fits PostgreSQL");
    let app = HttpContract::application(bearer_document())
        .unwrap()
        .merge(
            infra_http::idempotent_route!(
                missing_delegation,
                docs = "https://docs.example/widgets#retry"
            )
            .expect("missing delegation fixture composes"),
        )
        .unwrap()
        .build_with_idempotency_store(
            (),
            Authentication::Bearer(provider.verifier()),
            &options(BUDGET),
            store,
        )
        .unwrap();
    let server = TestServer::new(app.into_make_service());
    let response = server
        .post("/_test/missing-delegation")
        .authorization(format!("Bearer {ALICE}"))
        .add_header(KEY, "\"missing-delegation\"")
        .await;
    problem_body(
        &response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
    );
    provider.stop().await;
}

#[tokio::test]
async fn p9_unfinished_authenticated_upload_returns_the_declared_timeout() {
    let provider = Provider::start();
    let verifier = provider.verifier_with_cache(Some(
        IntrospectionCacheOptions::try_new(16, Duration::from_secs(30)).unwrap(),
    ));
    let authorization = format!("Bearer {ALICE}");
    verify_fixture_authorization(&verifier, authorization.as_bytes())
        .await
        .unwrap();
    let pool = connect_lazy("postgres://localhost/unused").expect("lazy pool");
    let store = Store::try_new(pool, RETENTION).unwrap();
    let contract = HttpContract::application(bearer_document())
        .unwrap()
        .merge(widget_contract())
        .unwrap()
        .with_probes("")
        .unwrap();
    let document = serde_json::to_value(contract.clone().into_openapi().unwrap()).unwrap();
    let app = contract
        .build_with_idempotency_store(
            readiness_reader(),
            Authentication::Bearer(verifier),
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
        .header(AUTHORIZATION, authorization)
        .header(KEY, "upload")
        .header(CONTENT_TYPE, "application/json")
        .body(upload)
        .unwrap();
    tokio::time::pause();
    let response = app.oneshot(request).await.unwrap();
    tokio::time::resume();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let media = response.headers()[CONTENT_TYPE].to_str().unwrap();
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
