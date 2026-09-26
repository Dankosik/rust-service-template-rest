//! The operations this crate owns: the platform probes, seeded with the
//! problem components every operation references.
//!
//! [`router`] returns a [`OpenApiRouter`], so the axum routes, their OpenAPI
//! description stay together until the service
//! crate finalizes policy and supplies state. New operations join a feature
//! carrier, never this file or the hardened chain.

// Probe handlers live in `probes`; the readiness reader comes from the
// `health` crate.
use crate::probes;
use crate::problem::responses::ProblemComponents;
use health::ReadinessReader;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// Route templates served without an access-log line unless enabled.
pub(crate) const HEALTH_PROBE_ROUTES: &[&str] = &[probes::LIVE_PATH, probes::READY_PATH];

/// The probe routes with their contract and the problem components, as one
/// [`OpenApiRouter`] whose [`ReadinessReader`] state is still unapplied: the
/// service crate's `api::contract` merges this value, finalizes policy, then
/// calls `with_state` before [`crate::harden`]. One `routes!` call per path:
/// the macro groups the methods of a single path.
pub fn router() -> OpenApiRouter<ReadinessReader> {
    let router = OpenApiRouter::with_openapi(ProblemComponents::openapi());
    #[expect(
        clippy::disallowed_methods,
        reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
    )]
    let live = routes!(probes::live);
    let router = router.routes(live);
    #[expect(
        clippy::disallowed_methods,
        reason = "utoipa_axum::routes! generates annotated MethodRouter::on calls"
    )]
    let ready = routes!(probes::ready);
    router.routes(ready)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
    use axum::http::{Method, Request, StatusCode};
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
            log_health_probes: false,
        }
    }

    async fn ready_reader() -> ReadinessReader {
        let readiness = Readiness::new(Vec::new());
        readiness
            .refresh(RefreshPolicy {
                interval: Duration::from_secs(1),
                probe_budget: Duration::from_secs(1),
                failure_threshold: 1,
            })
            .await;
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

    /// The media types the document declares for one response, following a
    /// `$ref` into `components/responses`.
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
        let document = router().into_openapi();
        assert_eq!(
            document
                .paths
                .paths
                .keys()
                .map(String::as_str)
                .sorted()
                .collect_vec(),
            HEALTH_PROBE_ROUTES.iter().copied().sorted().collect_vec(),
        );
    }

    #[tokio::test]
    async fn served_probe_responses_are_declared_by_the_document() {
        let (app, document) = app(ready_reader().await);
        for path in HEALTH_PROBE_ROUTES {
            let response = app
                .clone()
                .oneshot(Request::get(*path).body(Body::empty()).unwrap())
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
        let (app, document) = app(Readiness::new(Vec::new()).reader());
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
