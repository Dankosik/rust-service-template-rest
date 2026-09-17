//! Application router.
//!
//! The router is one route tree. New operations join it through the API
//! contract and feature-owned handlers; shared request policy is added here
//! only when it is truly shared by every route.

use axum::Router;
use axum::routing::get;

use crate::health::{self, Readiness};

/// Build the application router over the shared readiness flag.
pub fn router(readiness: Readiness) -> Router {
    Router::new()
        .route("/health/live", get(health::live))
        .route("/health/ready", get(health::ready))
        .with_state(readiness)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    async fn call(readiness: Readiness, method: Method, uri: &str) -> (StatusCode, String, String) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = router(readiness).oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            content_type,
            String::from_utf8(body.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn live_is_ok_regardless_of_readiness() {
        let (status, content_type, body) =
            call(Readiness::new(), Method::GET, "/health/live").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(content_type, "text/plain; charset=utf-8");
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn ready_is_503_until_the_flag_is_set() {
        let readiness = Readiness::new();
        let (status, content_type, body) =
            call(readiness.clone(), Method::GET, "/health/ready").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(content_type, "text/plain; charset=utf-8");
        assert_eq!(body, "not ready");

        readiness.set_ready(true);
        let (status, _, body) = call(readiness, Method::GET, "/health/ready").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn unknown_route_is_404() {
        let (status, _, _) = call(Readiness::new(), Method::GET, "/missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn probes_reject_other_methods() {
        let (status, _, _) = call(Readiness::new(), Method::POST, "/health/live").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }
}
