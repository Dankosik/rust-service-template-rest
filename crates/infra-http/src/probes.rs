use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use health::ReadinessReader;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::problem::responses::{ProblemComponents, TransportProblemResponses};

const LIVE_PATH: &str = "/health/live";
const READY_PATH: &str = "/health/ready";
const LIVE_OPERATION_ID: &str = "healthLive";
const READY_OPERATION_ID: &str = "healthReady";

const LIVE_BODY: &str = "ok";
const READY_BODY: &str = "ok";
const NOT_READY_BODY: &str = "not ready";

const HEALTH_PROBES: [(&str, &str); 2] = [
    (LIVE_PATH, LIVE_OPERATION_ID),
    (READY_PATH, READY_OPERATION_ID),
];

pub(crate) fn is_health_probe_route(route: &str) -> bool {
    HEALTH_PROBES.iter().any(|(path, _)| route.ends_with(path))
}

pub(crate) fn is_health_probe_operation(path: &str, operation_id: Option<&str>) -> bool {
    HEALTH_PROBES
        .iter()
        .any(|(probe, id)| path.ends_with(probe) && operation_id == Some(*id))
}

pub fn router<S>() -> OpenApiRouter<S>
where
    ReadinessReader: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    OpenApiRouter::with_openapi(ProblemComponents::openapi())
        .routes(routes!(live))
        .routes(routes!(ready))
}

pub fn liveness_router() -> axum::Router {
    #[expect(
        clippy::disallowed_methods,
        reason = "private diagnostics registration is outside the application OpenAPI contract"
    )]
    let router = axum::Router::new().route(LIVE_PATH, axum::routing::get(live));
    router
}

#[utoipa::path(
    get,
    path = LIVE_PATH,
    tag = "system",
    operation_id = LIVE_OPERATION_ID,
    summary = "Liveness probe",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "process-only platform liveness endpoint with no dependency details"
    }))),
    responses(
        (status = 200, description = "ok", content_type = "text/plain", body = String, example = json!(LIVE_BODY)),
        TransportProblemResponses,
    )
)]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn live() -> (StatusCode, &'static str) {
    (StatusCode::OK, LIVE_BODY)
}

#[utoipa::path(
    get,
    path = READY_PATH,
    tag = "system",
    operation_id = READY_OPERATION_ID,
    summary = "Readiness probe",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "platform readiness endpoint exposing only generic ready state"
    }))),
    responses(
        (status = 200, description = "ready", content_type = "text/plain", body = String, example = json!(READY_BODY)),
        (status = 503, description = "not ready", content_type = "text/plain", body = String, example = json!(NOT_READY_BODY)),
        TransportProblemResponses,
    )
)]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn ready(State(readiness): State<ReadinessReader>) -> (StatusCode, &'static str) {
    match readiness.verdict() {
        Ok(()) => (StatusCode::OK, READY_BODY),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, NOT_READY_BODY),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
    use axum::http::{Method, Request};
    use axum::response::Response;
    use health::{Readiness, RefreshPolicy};
    use itertools::Itertools;
    use tower::ServiceExt;

    use super::*;
    use crate::{HardenOptions, harden};

    fn options() -> HardenOptions {
        HardenOptions {
            max_body_bytes: 8,
            request_timeout: Duration::from_secs(1),
            max_in_flight: None,
            admission_wait: Duration::from_secs(1),
            log_health_probes: false,
            draining: None,
        }
    }

    fn readiness() -> Readiness {
        Readiness::new(
            Vec::new(),
            RefreshPolicy {
                interval: Duration::from_secs(1),
                probe_budget: Duration::from_secs(1),
                failure_threshold: std::num::NonZeroU32::MIN,
            },
        )
    }

    async fn ready_reader() -> ReadinessReader {
        let readiness = readiness();
        readiness.refresh().await;
        readiness.reader()
    }

    fn app(reader: ReadinessReader) -> (axum::Router, serde_json::Value) {
        let contract = router();
        let document = serde_json::to_value(contract.get_openapi()).unwrap();
        let routes = crate::finalize_public(contract)
            .expect("the probe contract is public")
            .with_state(reader);
        (harden(routes, &options()), document)
    }

    fn media_type(response: &Response) -> String {
        let value = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        value.split(';').next().unwrap().trim().to_owned()
    }

    fn declared_media_types(
        document: &serde_json::Value,
        path: &str,
        status: StatusCode,
    ) -> Vec<String> {
        let mut response = &document["paths"][path]["get"]["responses"][status.as_str()];
        if let Some(reference) = response["$ref"].as_str() {
            let name = reference
                .strip_prefix("#/components/responses/")
                .unwrap_or_else(|| panic!("unexpected reference {reference}"));
            response = &document["components"]["responses"][name];
        }
        response["content"]
            .as_object()
            .unwrap_or_else(|| panic!("{path} declares no {status} response"))
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn document_paths_are_the_access_log_probe_routes() {
        let document = router::<ReadinessReader>().into_openapi();
        assert_eq!(
            document
                .paths
                .paths
                .keys()
                .map(String::as_str)
                .sorted()
                .collect_vec(),
            HEALTH_PROBES
                .iter()
                .map(|(path, _)| *path)
                .sorted()
                .collect_vec(),
        );
    }

    #[tokio::test]
    async fn served_probe_responses_are_declared_by_the_document() {
        let (app, document) = app(ready_reader().await);
        for (path, _) in HEALTH_PROBES {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(media_type(&response), "text/plain", "{path}");
            assert!(
                declared_media_types(&document, path, StatusCode::OK)
                    .contains(&media_type(&response))
            );
        }
    }

    #[tokio::test]
    async fn not_ready_is_the_declared_503() {
        let (app, document) = app(readiness().reader());
        let response = app
            .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            declared_media_types(&document, "/health/ready", StatusCode::SERVICE_UNAVAILABLE)
                .contains(&media_type(&response))
        );
    }

    #[tokio::test]
    async fn oversize_body_is_the_declared_413_problem() {
        let (app, document) = app(ready_reader().await);
        let request = Request::builder()
            .method(Method::GET)
            .uri("/health/live")
            .header(CONTENT_LENGTH, 64)
            .body(Body::from("x".repeat(64)))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(media_type(&response), "application/problem+json");
        assert!(
            declared_media_types(&document, "/health/live", StatusCode::PAYLOAD_TOO_LARGE)
                .contains(&media_type(&response))
        );
    }
}
