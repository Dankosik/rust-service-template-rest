use crate::{FinalizeError, HttpContract};
use axum::extract::{FromRef, State};
use axum::http::{Method, StatusCode};
use health::ReadinessReader;
use std::sync::Arc;

const LIVE_PATH: &str = "/health/live";
const READY_PATH: &str = "/health/ready";
const LIVE_OPERATION_ID: &str = "healthLive";
const READY_OPERATION_ID: &str = "healthReady";

const LIVE_BODY: &str = "ok";
const READY_BODY: &str = "ok";
const NOT_READY_BODY: &str = "not ready";

#[derive(Clone, Debug, Default)]
pub(crate) struct ProbeRoutes(Arc<[(Box<str>, bool)]>);

impl ProbeRoutes {
    pub(crate) fn new(routes: Vec<(Box<str>, bool)>) -> Self {
        Self(routes.into())
    }

    pub(crate) fn contains(&self, method: &Method, route: &str) -> bool {
        self.contains_method(method.as_str(), route)
    }

    pub(crate) fn contains_method(&self, method: &str, route: &str) -> bool {
        self.0.iter().any(|(path, implicit_head)| {
            path.as_ref() == route && (method == "GET" || (*implicit_head && method == "HEAD"))
        })
    }
}

pub(crate) fn router<S>() -> Result<HttpContract<S>, FinalizeError>
where
    ReadinessReader: FromRef<S>,
    S: Clone + Send + Sync + 'static,
{
    crate::http_route!(live)?.merge(crate::http_route!(ready)?)
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
    )
)]
#[cfg_attr(feature = "hotpath", hotpath::measure)]
async fn ready(State(readiness): State<ReadinessReader>) -> (StatusCode, &'static str) {
    if readiness.is_ready() {
        (StatusCode::OK, READY_BODY)
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, NOT_READY_BODY)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::Request;
    use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
    use axum::response::Response;
    use health::{Readiness, RefreshPolicy};
    use tower::ServiceExt;

    use super::*;
    use crate::{Application, Authentication, HardenOptions};

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
            RefreshPolicy::try_new(
                Duration::from_secs(1),
                Duration::from_secs(1),
                std::num::NonZeroU32::MIN,
            )
            .unwrap(),
        )
    }

    async fn ready_reader() -> ReadinessReader {
        let readiness = readiness();
        let reader = readiness.control().reader();
        readiness.prepare().await.unwrap().activate().unwrap();
        reader
    }

    fn app(reader: ReadinessReader) -> (Application, serde_json::Value) {
        let contract = HttpContract::default().with_probes("/").unwrap();
        let document = serde_json::to_value(contract.clone().into_openapi().unwrap()).unwrap();
        (
            contract
                .build(reader, Authentication::Public, &options())
                .unwrap(),
            document,
        )
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

    #[tokio::test]
    async fn served_probe_responses_are_declared_by_the_document() {
        let (app, document) = app(ready_reader().await);
        for path in [LIVE_PATH, READY_PATH] {
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
        let readiness = readiness();
        let (app, document) = app(readiness.control().reader());
        let _prepared = readiness.prepare().await.unwrap();
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
