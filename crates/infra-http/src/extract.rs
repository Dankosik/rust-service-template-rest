//! Request extractors whose rejections are [`Problem`] values.
//!
//! axum's own `Json`, `Query`, and `Path` reject with a `text/plain` body.
//! These wrappers run the same extractors and map each rejection into the
//! closed catalog, so a handler that takes them answers a malformed request
//! with `application/problem+json` like every other failure. A rejection never
//! carries a submitted value: `invalid_params` gives the failing member's
//! location with a fixed reason when the deserializer reached it; a missing
//! root member, an unnamed position, or a name over 256 bytes carries no
//! location. axum's own message, which quotes the value, is dropped.
//!
//! `clippy.toml` disallows axum's three types in application code, so a
//! handler cannot take the `text/plain` rejections by accident. [`Json`] is
//! therefore also the JSON response body, as `axum::Json` is.

use std::error::Error as StdError;

use axum::body::Bytes;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, RawPathParams, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use serde_path_to_error::Segment;

use crate::RequestContext;
use crate::context::read_body;
use crate::problem::{Code, Problem, body_too_large, body_unreadable, sanitized_problem};

const BODY_SYNTAX_DETAIL: &str = "request body is not valid JSON";
const BODY_SCHEMA_DETAIL: &str = "request body does not match the operation schema";
const BODY_MEDIA_TYPE_DETAIL: &str = "Content-Type must be application/json";
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
/// malformed JSON `400`, a body that parses but does not fit `T` `422`, and an
/// oversize body `413`. The `422` names the failing member's RFC 6901 pointer
/// in `invalid_params` when the deserializer reached that member; a missing
/// root member or an unnamed position carries no location. As a response it
/// renders `application/json` as `axum::Json` does; a value serde cannot
/// serialize answers the sanitized `500` Problem instead of axum's plain text.
#[derive(Clone, Copy, Debug, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    #[expect(
        clippy::disallowed_types,
        reason = "the native JSON decoder retains the deserialization path"
    )]
    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        // Match axum's media-type check before collecting the bounded body.
        let json_content_type = request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()?.parse::<mime::Mime>().ok())
            .is_some_and(|media_type| {
                media_type.type_() == mime::APPLICATION
                    && (media_type.subtype() == mime::JSON
                        || media_type.suffix() == Some(mime::JSON))
            });
        if !json_content_type {
            return Err(Problem::new(Code::UnsupportedMediaType).detail(BODY_MEDIA_TYPE_DETAIL));
        }
        let context = request.extensions().get::<RequestContext>().cloned();
        let bytes = read_body(context.as_ref(), Bytes::from_request(request, state))
            .await
            .map_err(|rejection| match rejection.status() {
                StatusCode::PAYLOAD_TOO_LARGE => body_too_large(),
                StatusCode::BAD_REQUEST => body_unreadable(),
                _ => wiring_failure("json"),
            })?;
        match axum::Json::<T>::from_bytes(&bytes) {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_problem(&rejection, &bytes)),
        }
    }
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        let Ok(body) = serde_json::to_vec(&self.0) else {
            // serde's message can quote the value, so it stays out of the log.
            tracing::error!(
                response_type = std::any::type_name::<T>(),
                "http_response_serialization_failed"
            );
            return sanitized_problem().into_response();
        };
        (
            [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
            body,
        )
            .into_response()
    }
}

/// Query parameters: `axum::extract::Query` with [`Problem`] rejections.
///
/// A query string that does not fit `T` answers `400`; `invalid_params` names
/// the parameter as `query.<name>` when the failing value was reached, while a
/// missing required parameter carries no location.
#[derive(Clone, Copy, Debug, Default)]
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    #[expect(
        clippy::disallowed_types,
        reason = "this wrapper delegates to the native extractor and maps its rejection"
    )]
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

    #[expect(
        clippy::disallowed_types,
        reason = "this wrapper delegates to the native extractor and maps its rejection"
    )]
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<TrackedPath<T>>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(TrackedPath(Ok(value)))) => Ok(Self(value)),
            Ok(axum::extract::Path(TrackedPath(Err(issue)))) => {
                let names = RawPathParams::from_request_parts(parts, state).await.ok();
                Err(issue.problem(names.as_ref()))
            }
            Err(rejection) => {
                let names = RawPathParams::from_request_parts(parts, state).await.ok();
                Err(path_problem(&rejection, names.as_ref()))
            }
        }
    }
}

fn json_problem(rejection: &JsonRejection, bytes: &[u8]) -> Problem {
    match rejection {
        // Axum checks the end of the document only after typed decoding
        // succeeds. Check the entire document when that decoder stops early.
        JsonRejection::JsonDataError(_)
            if !std::str::from_utf8(bytes)
                .is_ok_and(|text| serde_json::from_str::<IgnoredAny>(text).is_ok()) =>
        {
            Problem::new(Code::BadRequest).detail(BODY_SYNTAX_DETAIL)
        }
        JsonRejection::JsonDataError(error) => with_param(
            Problem::new(Code::UnprocessableContent).detail(BODY_SCHEMA_DETAIL),
            failed_member::<serde_json::Error>(error).and_then(body_pointer),
        ),
        JsonRejection::JsonSyntaxError(_) => {
            Problem::new(Code::BadRequest).detail(BODY_SYNTAX_DETAIL)
        }
        _ => wiring_failure("json"),
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
        return wiring_failure("path");
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
    invalid_path(name)
}

struct TrackedPath<T>(Result<T, PathIssue>);

enum PathIssue {
    MissingField(String),
    UnknownField,
    DuplicateField,
    UnknownVariant(serde_path_to_error::Path),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for TrackedPath<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let error = match serde_path_to_error::deserialize(deserializer) {
            Ok(value) => return Ok(Self(Ok(value))),
            Err(error) => error,
        };
        let message = error.inner().to_string();
        // Axum erases Serde's error hooks. The tracked position distinguishes
        // errors on route keys or the root map from errors decoding a value.
        let mut path = error.path().iter();
        match (path.next(), path.next()) {
            (None, _) => {
                if let Some(field) = message
                    .strip_prefix("missing field `")
                    .and_then(|field| field.strip_suffix('`'))
                {
                    return Ok(Self(Err(PathIssue::MissingField(field.to_owned()))));
                }
                if message.starts_with("duplicate field `") && message.ends_with('`') {
                    return Ok(Self(Err(PathIssue::DuplicateField)));
                }
            }
            (Some(Segment::Map { key }), None)
                if message
                    .strip_prefix("unknown field `")
                    .and_then(|field| field.strip_prefix(key.as_str()))
                    .is_some_and(|suffix| suffix.starts_with("`, ")) =>
            {
                return Ok(Self(Err(PathIssue::UnknownField)));
            }
            _ => {}
        }
        if message.starts_with("unknown variant `") {
            return Ok(Self(Err(PathIssue::UnknownVariant(error.path().clone()))));
        }
        Err(error.into_inner())
    }
}

impl PathIssue {
    fn problem(&self, params: Option<&RawPathParams>) -> Problem {
        let Some(params) = params else {
            return invalid_path(None);
        };
        match self {
            Self::MissingField(field) if !params.iter().any(|(name, _)| name == field.as_str()) => {
                wiring_failure("path")
            }
            Self::UnknownField => wiring_failure("path"),
            // A duplicate route binding needs two captures. A one-capture
            // scalar can return the same message from value validation.
            Self::DuplicateField if params.iter().nth(1).is_some() => wiring_failure("path"),
            Self::MissingField(_) | Self::DuplicateField => invalid_path(None),
            Self::UnknownVariant(path) => {
                let name = match path.iter().next() {
                    Some(Segment::Map { key }) => params
                        .iter()
                        .find(|(name, _)| *name == key.as_str())
                        .map(|(name, _)| name),
                    Some(Segment::Seq { index }) => params.iter().nth(*index).map(|(name, _)| name),
                    _ => {
                        let mut params = params.iter();
                        params
                            .next()
                            .filter(|_| params.next().is_none())
                            .map(|(name, _)| name)
                    }
                };
                invalid_path(name)
            }
        }
    }
}

fn invalid_path(name: Option<&str>) -> Problem {
    with_param(
        Problem::new(Code::BadRequest).detail(PATH_DETAIL),
        name.map(|name| format!("path.{name}")),
    )
}

/// A rejection the caller cannot cause: the handler's extractor does not fit
/// its route. Logged for the operator, sanitized for the caller.
fn wiring_failure(extractor: &'static str) -> Problem {
    tracing::error!(extractor, "http_extractor_wiring_failed");
    sanitized_problem()
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
    shared_rules::error_chain::causes(rejection)
        .find_map(|error| error.downcast_ref::<serde_path_to_error::Error<E>>())
        .map(serde_path_to_error::Error::path)
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
                pointer.push_str(&shared_rules::rfc6901::escape(key));
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
    use crate::problem::SANITIZED_DETAIL;
    use crate::{HardenOptions, REQUEST_ID_HEADER, harden};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Order {
        #[expect(
            dead_code,
            reason = "serde reads the member; the test only checks rejections"
        )]
        slug: String,
        lines: Vec<Line>,
    }

    #[derive(Deserialize)]
    struct Line {
        #[expect(
            dead_code,
            reason = "serde reads the member; the test only checks rejections"
        )]
        quantity: u32,
    }

    #[derive(Deserialize)]
    struct Page {
        #[expect(
            dead_code,
            reason = "serde reads the member; the test only checks rejections"
        )]
        limit: u32,
    }

    #[derive(Deserialize)]
    enum OrderState {
        Open,
        Closed,
    }

    #[derive(Deserialize)]
    struct StatePath {
        id: String,
        state: OrderState,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictPath {
        id: String,
    }

    #[derive(Deserialize)]
    struct AliasedPath {
        #[serde(alias = "old_id")]
        id: String,
    }

    #[derive(Deserialize)]
    #[serde(try_from = "String")]
    struct DistinctPair(String);

    impl TryFrom<String> for DistinctPair {
        type Error = &'static str;

        fn try_from(value: String) -> Result<Self, Self::Error> {
            let (first, second) = value.split_once('-').ok_or("expected two ids")?;
            if first == second {
                return Err("duplicate field `id`");
            }
            Ok(Self(value))
        }
    }

    fn app() -> Router {
        let handler_1 = |Json(order): Json<Order>| async move { order.lines.len().to_string() };
        let handler_2 = |Query(_): Query<Page>| async { "page" };
        let handler_3 = |Path(_): Path<u32>| async { "order" };
        let handler_4 = |Path(_): Path<(u32, u32)>| async { "line" };
        let handler_5 = |Path(_): Path<(u32, u32)>| async { "unreachable" };
        let state = |Path(_): Path<OrderState>| async { "state" };
        let order_state = |Path(params): Path<StatePath>| async move {
            match params.state {
                OrderState::Open => params.id,
                OrderState::Closed => String::new(),
            }
        };
        let strict_path = |Path(params): Path<StrictPath>| async move { params.id };
        let aliased_path = |Path(params): Path<AliasedPath>| async move { params.id };
        let distinct_pair = |Path(pair): Path<DistinctPair>| async move { pair.0 };
        #[expect(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let routes = Router::new()
            .route("/orders", post(handler_1))
            .route("/orders", get(handler_2))
            .route("/orders/{id}", get(handler_3))
            .route("/orders/{id}/lines/{line}", get(handler_4))
            .route("/miswired/{id}", get(handler_5))
            .route("/miswired-struct/{id}", get(order_state))
            .route("/miswired-extra/{id}/{extra}", get(strict_path))
            .route("/miswired-alias/{id}/{old_id}", get(aliased_path))
            .route("/pairs/{pair}", get(distinct_pair))
            .route("/states/{state}", get(state))
            .route("/orders/{id}/states/{state}", get(order_state));
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
        for content_type in [
            "application/json",
            "application/vnd.orders+json; charset=utf-8",
        ] {
            let mut request = post_json(r#"{"slug":"a","lines":[{"quantity":1},{"quantity":2}]}"#);
            request
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
            let response = app().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                "2"
            );
        }
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
    async fn a_response_serde_cannot_serialize_is_the_sanitized_500_problem() {
        let unserializable = std::collections::BTreeMap::from([((1, 2), 3)]);
        let response = Json(unserializable).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "internal_error");
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
        for input in [
            r#"{"slug": submitted-secret"#,
            r#"{"slug":1,"lines":[]} ]"#,
            r#"{"slug":1,"lines":[}"#,
        ] {
            let response = app().oneshot(post_json(input)).await.unwrap();
            let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
            assert_eq!(body["detail"], "request body is not valid JSON");
            assert!(body.get("invalid_params").is_none());
            assert!(!body.to_string().contains("submitted-secret"), "{body}");
        }
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

        let response = app().oneshot(get_uri("/pairs/7-7")).await.unwrap();
        problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
    }

    #[tokio::test]
    async fn an_extractor_that_cannot_match_its_route_is_a_sanitized_500() {
        for uri in [
            "/miswired/7",
            "/miswired-struct/7",
            "/miswired-extra/7/8",
            "/miswired-alias/7/8",
        ] {
            let response = app().oneshot(get_uri(uri)).await.unwrap();
            let body = problem(
                response,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
            )
            .await;
            assert_eq!(body["detail"], SANITIZED_DETAIL);
        }
    }

    #[tokio::test]
    async fn an_unknown_enum_value_names_its_path_parameter() {
        for uri in [
            "/states/submitted-secret",
            "/orders/submitted-secret/states/submitted-secret",
        ] {
            let response = app().oneshot(get_uri(uri)).await.unwrap();
            let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
            assert_eq!(body["invalid_params"][0]["name"], "path.state");
            assert!(!body.to_string().contains("submitted-secret"), "{body}");
        }
    }

    #[test]
    fn body_pointers_escape_rfc_6901_reserved_characters() {
        #[derive(Deserialize)]
        struct Outer {
            #[expect(
                dead_code,
                reason = "serde reads the member; the test only checks rejections"
            )]
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
