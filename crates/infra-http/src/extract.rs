//! Request extractors whose rejections are [`Problem`] values.
//!
//! axum's own `Json`, `Query`, and `Path` reject with a `text/plain` body.
//! These wrappers run the same extractors and map each rejection into the
//! closed catalog, so a handler that takes them answers a malformed request
//! with `application/problem+json` like every other failure. A rejection never
//! carries a submitted value: `invalid_params` gives the failing member's
//! location with a fixed reason, and axum's own message, which quotes the
//! value, is dropped. A body location is the caller's own member name, so it
//! is bounded.
//!
//! `clippy.toml` disallows axum's three types in application code, so a
//! handler cannot take the `text/plain` rejections by accident. [`Json`] is
//! therefore also the JSON response body, as `axum::Json` is.

#![allow(
    clippy::disallowed_types,
    reason = "the wrappers delegate to the axum extractors they replace"
)]

use std::error::Error as StdError;

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, RawPathParams, Request};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_path_to_error::Segment;

use crate::problem::{Code, Problem, SANITIZED_DETAIL};

const BODY_SYNTAX_DETAIL: &str = "request body is not valid JSON";
const BODY_SCHEMA_DETAIL: &str = "request body does not match the operation schema";
const BODY_MEDIA_TYPE_DETAIL: &str = "Content-Type must be application/json";
const BODY_READ_DETAIL: &str = "request body could not be read";
const BODY_LIMIT_DETAIL: &str = "request body exceeds the configured limit";
const QUERY_DETAIL: &str = "query string does not match the operation parameters";
const PATH_DETAIL: &str = "path does not match the operation parameters";
const SCHEMA_REASON: &str = "is missing or does not match the declared schema";

/// A location longer than this is left out of `invalid_params`: a body
/// member's name is caller-supplied, and the problem must not reflect an
/// unbounded one.
const MAX_PARAM_NAME_BYTES: usize = 256;

/// A JSON body: `axum::Json` with [`Problem`] rejections.
///
/// As an extractor, a missing or wrong `Content-Type` answers `415`,
/// malformed JSON `400`, a body that parses but does not fit `T` `422` with
/// the member's RFC 6901 pointer in `invalid_params`, and an oversize body
/// `413`. As a response it renders `application/json` exactly as `axum::Json`
/// does.
#[derive(Clone, Copy, Debug, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(request, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_problem(&rejection)),
        }
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// Query parameters: `axum::extract::Query` with [`Problem`] rejections.
///
/// A query string that does not fit `T` answers `400`, naming the parameter
/// as `query.<name>` in `invalid_params`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(query_problem(&rejection)),
        }
    }
}

/// Path parameters: `axum::extract::Path` with [`Problem`] rejections.
///
/// A segment that does not fit `T` answers `400`, naming the parameter as
/// `path.<name>` in `invalid_params`. A handler whose `T` cannot match its
/// route template is a wiring fault and answers the sanitized `500`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Self(value)),
            Err(rejection) => {
                let names = RawPathParams::from_request_parts(parts, state).await.ok();
                Err(path_problem(&rejection, names.as_ref()))
            }
        }
    }
}

fn json_problem(rejection: &JsonRejection) -> Problem {
    match rejection {
        JsonRejection::JsonDataError(error) => with_param(
            Problem::new(Code::UnprocessableContent).detail(BODY_SCHEMA_DETAIL),
            failed_member::<serde_json::Error>(error).and_then(body_pointer),
        ),
        JsonRejection::JsonSyntaxError(_) => {
            Problem::new(Code::BadRequest).detail(BODY_SYNTAX_DETAIL)
        }
        JsonRejection::MissingJsonContentType(_) => {
            Problem::new(Code::UnsupportedMediaType).detail(BODY_MEDIA_TYPE_DETAIL)
        }
        // The body could not be buffered: over the limit, or a broken stream.
        other => match other.status() {
            StatusCode::PAYLOAD_TOO_LARGE => {
                Problem::new(Code::RequestEntityTooLarge).detail(BODY_LIMIT_DETAIL)
            }
            StatusCode::BAD_REQUEST => Problem::new(Code::BadRequest).detail(BODY_READ_DETAIL),
            _ => wiring_fault("json"),
        },
    }
}

fn query_problem(rejection: &QueryRejection) -> Problem {
    with_param(
        Problem::new(Code::BadRequest).detail(QUERY_DETAIL),
        failed_member::<serde::de::value::Error>(rejection).and_then(|path| {
            match path.iter().next() {
                Some(Segment::Map { key }) => Some(format!("query.{key}")),
                _ => None,
            }
        }),
    )
}

/// `names` are the route template's parameters in order; they name a
/// parameter axum identifies only by position, or not at all for a single one.
fn path_problem(rejection: &PathRejection, names: Option<&RawPathParams>) -> Problem {
    use axum::extract::path::ErrorKind;

    if rejection.status() != StatusCode::BAD_REQUEST {
        return wiring_fault("path");
    }
    let nth = |index: usize| names?.iter().nth(index).map(|(name, _)| name);
    let name = match rejection {
        PathRejection::FailedToDeserializePathParams(failed) => match failed.kind() {
            ErrorKind::ParseErrorAtKey { key, .. }
            | ErrorKind::InvalidUtf8InPathParam { key }
            | ErrorKind::DeserializeError { key, .. } => Some(key.as_str()),
            ErrorKind::ParseErrorAtIndex { index, .. } => nth(*index),
            ErrorKind::ParseError { .. } => nth(0).filter(|_| nth(1).is_none()),
            _ => None,
        },
        _ => None,
    };
    with_param(
        Problem::new(Code::BadRequest).detail(PATH_DETAIL),
        name.map(|name| format!("path.{name}")),
    )
}

/// A rejection the caller cannot cause: the handler's extractor does not fit
/// its route. Logged for the operator, sanitized for the caller.
fn wiring_fault(extractor: &'static str) -> Problem {
    tracing::error!(extractor, "http_extractor_wiring_failed");
    Problem::new(Code::InternalServerError).detail(SANITIZED_DETAIL)
}

fn with_param(problem: Problem, name: Option<String>) -> Problem {
    match name {
        Some(name) if name.len() <= MAX_PARAM_NAME_BYTES => {
            problem.invalid_param(name, SCHEMA_REASON)
        }
        _ => problem,
    }
}

/// Where deserialization failed, which axum's extractors record through
/// `serde_path_to_error` and keep in the rejection's source chain.
fn failed_member<'a, E>(
    rejection: &'a (dyn StdError + 'static),
) -> Option<&'a serde_path_to_error::Path>
where
    E: StdError + 'static,
{
    let mut current = Some(rejection);
    while let Some(error) = current {
        if let Some(located) = error.downcast_ref::<serde_path_to_error::Error<E>>() {
            return Some(located.path());
        }
        current = error.source();
    }
    None
}

/// The RFC 6901 pointer of a body member, or `None` for the document root or
/// a position the deserializer could not name.
fn body_pointer(path: &serde_path_to_error::Path) -> Option<String> {
    let mut pointer = String::new();
    for segment in path {
        pointer.push('/');
        match segment {
            Segment::Seq { index } => pointer.push_str(&index.to_string()),
            Segment::Map { key } | Segment::Enum { variant: key } => {
                pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
            }
            Segment::Unknown => return None,
        }
    }
    (!pointer.is_empty()).then_some(pointer)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Method, Request as HttpRequest};
    use axum::response::Response;
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::{HardenOptions, REQUEST_ID_HEADER, harden};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Order {
        #[allow(dead_code)]
        slug: String,
        lines: Vec<Line>,
    }

    #[derive(Deserialize)]
    struct Line {
        #[allow(dead_code)]
        quantity: u32,
    }

    #[derive(Deserialize)]
    struct Page {
        #[allow(dead_code)]
        limit: u32,
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "transport fixture exercises the extractors independently of contract finalization"
    )]
    fn app() -> Router {
        let routes = Router::new()
            .route(
                "/orders",
                post(|Json(order): Json<Order>| async move { order.lines.len().to_string() }),
            )
            .route("/orders", get(|Query(_): Query<Page>| async { "page" }))
            .route("/orders/{id}", get(|Path(_): Path<u32>| async { "order" }))
            .route(
                "/orders/{id}/lines/{line}",
                get(|Path(_): Path<(u32, u32)>| async { "line" }),
            )
            .route(
                "/miswired/{id}",
                get(|Path(_): Path<(u32, u32)>| async { "unreachable" }),
            );
        harden(
            routes,
            &HardenOptions {
                max_body_bytes: 512,
                request_timeout: Duration::from_secs(1),
                max_in_flight: None,
                log_health_probes: false,
            },
        )
    }

    fn post_json(body: impl Into<Body>) -> HttpRequest<Body> {
        HttpRequest::post("/orders")
            .header(CONTENT_TYPE, "application/json")
            .body(body.into())
            .unwrap()
    }

    fn get_uri(uri: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    /// The problem body, after checking the envelope every rejection shares.
    async fn problem(response: Response, status: StatusCode, code: &str) -> Value {
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let request_id = response.headers().get(&REQUEST_ID_HEADER).cloned();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], code);
        assert_eq!(
            body["request_id"].as_str(),
            request_id.as_ref().and_then(|id| id.to_str().ok())
        );
        body
    }

    #[tokio::test]
    async fn a_valid_body_reaches_the_handler() {
        let response = app()
            .oneshot(post_json(
                r#"{"slug":"a","lines":[{"quantity":1},{"quantity":2}]}"#,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "2"
        );
    }

    #[tokio::test]
    async fn a_json_response_is_what_axum_renders() {
        let response = Json(json!({"slug": "a"})).into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            r#"{"slug":"a"}"#
        );
    }

    #[tokio::test]
    async fn a_body_that_does_not_fit_the_schema_is_a_422_naming_the_member() {
        let response = app()
            .oneshot(post_json(
                r#"{"slug":"a","lines":[{"quantity":1},{"quantity":"submitted-secret"}]}"#,
            ))
            .await
            .unwrap();
        let body = problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unprocessable_content",
        )
        .await;
        assert_eq!(
            body["invalid_params"],
            json!([{"name": "/lines/1/quantity", "reason": SCHEMA_REASON}])
        );
        assert!(!body.to_string().contains("submitted-secret"), "{body}");
    }

    #[tokio::test]
    async fn a_missing_or_undeclared_member_is_a_422_without_the_submitted_value() {
        for (body, name) in [
            // A member missing from the document root has no location.
            (r#"{"lines":[]}"#, None),
            (
                r#"{"slug":"a","lines":[],"extra":"submitted-secret"}"#,
                Some("/extra"),
            ),
        ] {
            let response = app().oneshot(post_json(body)).await.unwrap();
            let problem = problem(
                response,
                StatusCode::UNPROCESSABLE_ENTITY,
                "unprocessable_content",
            )
            .await;
            assert_eq!(problem["detail"], BODY_SCHEMA_DETAIL);
            assert_eq!(problem["invalid_params"][0]["name"].as_str(), name);
            assert!(
                !problem.to_string().contains("submitted-secret"),
                "{problem}"
            );
        }
    }

    #[tokio::test]
    async fn an_oversize_member_name_is_not_reflected() {
        let name = "k".repeat(MAX_PARAM_NAME_BYTES);
        let response = app()
            .oneshot(post_json(format!(
                r#"{{"slug":"a","lines":[],"{name}":1}}"#
            )))
            .await
            .unwrap();
        let body = problem(
            response,
            StatusCode::UNPROCESSABLE_ENTITY,
            "unprocessable_content",
        )
        .await;
        assert!(body.get("invalid_params").is_none(), "{body}");
    }

    #[tokio::test]
    async fn malformed_json_is_a_400() {
        let response = app()
            .oneshot(post_json(r#"{"slug": submitted-secret"#))
            .await
            .unwrap();
        let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
        assert_eq!(body["detail"], BODY_SYNTAX_DETAIL);
        assert!(body.get("invalid_params").is_none());
        assert!(!body.to_string().contains("submitted-secret"), "{body}");
    }

    #[tokio::test]
    async fn a_body_without_the_json_media_type_is_a_415() {
        for content_type in [None, Some("text/plain")] {
            let mut request = HttpRequest::post("/orders");
            if let Some(content_type) = content_type {
                request = request.header(CONTENT_TYPE, content_type);
            }
            let response = app()
                .oneshot(request.body(Body::from("{}")).unwrap())
                .await
                .unwrap();
            let body = problem(
                response,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
            )
            .await;
            assert_eq!(body["detail"], BODY_MEDIA_TYPE_DETAIL);
        }
    }

    #[tokio::test]
    async fn an_oversize_streamed_body_is_a_413() {
        let chunks =
            (0..64).map(|_| Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![b' '; 16])));
        let response = app()
            .oneshot(post_json(Body::from_stream(futures_util::stream::iter(
                chunks,
            ))))
            .await
            .unwrap();
        problem(
            response,
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_entity_too_large",
        )
        .await;
    }

    #[tokio::test]
    async fn a_query_parameter_that_does_not_fit_is_a_400_naming_it() {
        for uri in ["/orders?limit=submitted-secret", "/orders"] {
            let response = app().oneshot(get_uri(uri)).await.unwrap();
            let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
            assert_eq!(body["detail"], QUERY_DETAIL);
            assert!(!body.to_string().contains("submitted-secret"), "{body}");
        }
        let response = app()
            .oneshot(get_uri("/orders?limit=submitted-secret"))
            .await
            .unwrap();
        let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
        assert_eq!(
            body["invalid_params"],
            json!([{"name": "query.limit", "reason": SCHEMA_REASON}])
        );
    }

    #[tokio::test]
    async fn a_path_parameter_that_does_not_fit_is_a_400_naming_it() {
        let response = app()
            .oneshot(get_uri("/orders/submitted-secret"))
            .await
            .unwrap();
        let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
        assert_eq!(
            body["invalid_params"],
            json!([{"name": "path.id", "reason": SCHEMA_REASON}])
        );
        assert!(!body.to_string().contains("submitted-secret"), "{body}");

        let response = app()
            .oneshot(get_uri("/orders/7/lines/submitted-secret"))
            .await
            .unwrap();
        let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
        assert_eq!(body["invalid_params"][0]["name"], "path.line");
        assert!(!body.to_string().contains("submitted-secret"), "{body}");

        let response = app().oneshot(get_uri("/orders/7")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_extractor_that_cannot_match_its_route_is_a_sanitized_500() {
        let response = app().oneshot(get_uri("/miswired/7")).await.unwrap();
        let body = problem(
            response,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        )
        .await;
        assert_eq!(body["detail"], SANITIZED_DETAIL);
    }

    #[test]
    fn body_pointers_escape_rfc_6901_reserved_characters() {
        #[derive(Deserialize)]
        struct Outer {
            #[allow(dead_code)]
            #[serde(rename = "a/b~c")]
            inner: u32,
        }
        let mut deserializer = serde_json::Deserializer::from_str(r#"{"a/b~c":"x"}"#);
        let error = serde_path_to_error::deserialize::<_, Outer>(&mut deserializer)
            .err()
            .expect("the member does not fit");
        assert_eq!(body_pointer(error.path()).as_deref(), Some("/a~1b~0c"));
    }
}
