use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequest, FromRequestParts, RawPathParams, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use compact_str::{CompactString, format_compact};
use itertools::Itertools as _;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use serde_path_to_error::Segment;

use crate::context::read_request_bytes;
use crate::problem::{Code, Problem, body_too_large, body_unreadable, sanitized_problem};

const BODY_SYNTAX_DETAIL: &str = "request body is not valid JSON";
const BODY_SCHEMA_DETAIL: &str = "request body does not match the operation schema";
const BODY_MEDIA_TYPE_DETAIL: &str =
    "Content-Type must be application/json or an application/*+json type";
const QUERY_DETAIL: &str = "query string does not match the operation parameters";
const PATH_DETAIL: &str = "path does not match the operation parameters";
const SCHEMA_REASON: &str = "is missing or does not match the declared schema";

const MAX_PARAM_NAME_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Default)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem;

    async fn from_request(request: Request, _state: &S) -> Result<Self, Self::Rejection> {
        require_json_content_type(request.headers().get(CONTENT_TYPE))?;
        let bytes =
            read_request_bytes(request)
                .await
                .map_err(|rejection| match rejection.status() {
                    StatusCode::PAYLOAD_TOO_LARGE => body_too_large(),
                    StatusCode::BAD_REQUEST => body_unreadable(),
                    _ => wiring_failure("json"),
                })?;
        decode_json(&bytes).map(Self)
    }
}

pub(crate) fn is_json_media_type(value: &str) -> bool {
    value.eq_ignore_ascii_case("application/json")
        || value.parse::<mime::Mime>().is_ok_and(|media_type| {
            media_type.type_() == mime::APPLICATION
                && (media_type.subtype() == mime::JSON || media_type.suffix() == Some(mime::JSON))
        })
}

#[expect(
    clippy::result_large_err,
    reason = "the extractor returns Problem by value"
)]
pub(crate) fn require_json_content_type(content_type: Option<&HeaderValue>) -> Result<(), Problem> {
    let json = content_type
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_json_media_type);
    if json {
        Ok(())
    } else {
        Err(Problem::new(Code::UnsupportedMediaType).detail_static(BODY_MEDIA_TYPE_DETAIL))
    }
}

pub(crate) fn decode_json<T: DeserializeOwned, E: From<Problem>>(bytes: &[u8]) -> Result<T, E> {
    let mut input = serde_json::Deserializer::from_slice(bytes);
    let value = serde_path_to_error::deserialize(&mut input)
        .map_err(|error| json_problem(&error, bytes))?;
    input
        .end()
        .map_err(|_| Problem::new(Code::BadRequest).detail_static(BODY_SYNTAX_DETAIL))?;
    Ok(value)
}

impl<T> IntoResponse for Json<T>
where
    T: Serialize,
{
    fn into_response(self) -> Response {
        self.with_status(StatusCode::OK)
    }
}

impl<T: Serialize> Json<T> {
    #[must_use]
    pub fn with_status(self, status: StatusCode) -> Response {
        let Ok(body) = serde_json::to_vec(&self.0) else {
            tracing::error!(
                response_type = std::any::type_name::<T>(),
                "http_response_serialization_failed"
            );
            return sanitized_problem().into_response();
        };
        (
            status,
            [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
            Body::from(body),
        )
            .into_response()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = Problem;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(decode_query(parts.uri.query().unwrap_or_default()).map(Self))
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the extractor returns Problem by value"
)]
pub(crate) fn decode_query<T: DeserializeOwned>(query: &str) -> Result<T, Problem> {
    let fields = serde_html_form::Deserializer::from_bytes(query.as_bytes());
    serde_path_to_error::deserialize(fields).map_err(|error| query_problem(error.path()))
}

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
                Err(issue.problem(parts, state).await)
            }
            Err(rejection) => Err(path_problem(&rejection, parts, state).await),
        }
    }
}

fn json_problem(error: &serde_path_to_error::Error<serde_json::Error>, bytes: &[u8]) -> Problem {
    if error.inner().is_data()
        && std::str::from_utf8(bytes)
            .is_ok_and(|text| serde_json::from_str::<IgnoredAny>(text).is_ok())
    {
        with_param(
            Problem::new(Code::UnprocessableContent).detail_static(BODY_SCHEMA_DETAIL),
            body_pointer(error.path()),
        )
    } else {
        Problem::new(Code::BadRequest).detail_static(BODY_SYNTAX_DETAIL)
    }
}

fn query_problem(path: &serde_path_to_error::Path) -> Problem {
    with_param(
        Problem::new(Code::BadRequest).detail_static(QUERY_DETAIL),
        match path.iter().next() {
            Some(Segment::Map { key }) => param_name("query.", key),
            _ => None,
        },
    )
}

async fn path_problem<S: Send + Sync>(
    rejection: &PathRejection,
    parts: &mut Parts,
    state: &S,
) -> Problem {
    use axum::extract::path::ErrorKind;

    if rejection.status() != StatusCode::BAD_REQUEST {
        return wiring_failure("path");
    }
    let PathRejection::FailedToDeserializePathParams(failed) = rejection else {
        return invalid_path(None);
    };
    let index = match failed.kind() {
        ErrorKind::ParseErrorAtKey { key, .. }
        | ErrorKind::InvalidUtf8InPathParam { key }
        | ErrorKind::DeserializeError { key, .. } => return invalid_path(Some(key)),
        ErrorKind::ParseErrorAtIndex { index, .. } => Some(*index),
        ErrorKind::ParseError { .. } => None,
        _ => return invalid_path(None),
    };
    let Ok(params) = RawPathParams::from_request_parts(parts, state).await else {
        return invalid_path(None);
    };
    let param = match index {
        Some(index) => params.iter().nth(index),
        None => params.iter().exactly_one().ok(),
    };
    invalid_path(param.map(|(name, _)| name))
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
        let mut track = serde_path_to_error::Track::new();
        let error = match T::deserialize(serde_path_to_error::Deserializer::new(
            deserializer,
            &mut track,
        )) {
            Ok(value) => return Ok(Self(Ok(value))),
            Err(error) => error,
        };
        match PathIssue::classify(track.path(), &error.to_string()) {
            Some(issue) => Ok(Self(Err(issue))),
            None => Err(error),
        }
    }
}

impl PathIssue {
    fn classify(path: serde_path_to_error::Path, message: &str) -> Option<Self> {
        let mut segments = path.iter();
        match (segments.next(), segments.next()) {
            (None, _) => {
                if let Some(field) = message.strip_circumfix("missing field `", '`') {
                    return Some(Self::MissingField(field.to_owned()));
                }
                if message.strip_circumfix("duplicate field `", '`').is_some() {
                    return Some(Self::DuplicateField);
                }
            }
            (Some(Segment::Map { key }), None)
                if message
                    .strip_prefix("unknown field `")
                    .and_then(|field| field.strip_prefix(key.as_str()))
                    .is_some_and(|suffix| suffix.starts_with("`, ")) =>
            {
                return Some(Self::UnknownField);
            }
            _ => {}
        }
        message
            .starts_with("unknown variant `")
            .then_some(Self::UnknownVariant(path))
    }

    async fn problem<S: Send + Sync>(&self, parts: &mut Parts, state: &S) -> Problem {
        if matches!(self, Self::UnknownField) {
            return wiring_failure("path");
        }
        let Ok(params) = RawPathParams::from_request_parts(parts, state).await else {
            return invalid_path(None);
        };
        match self {
            Self::MissingField(field) if !params.iter().any(|(name, _)| name == field.as_str()) => {
                wiring_failure("path")
            }
            Self::DuplicateField if params.iter().nth(1).is_some() => wiring_failure("path"),
            Self::UnknownVariant(path) => {
                let name = match path.iter().next() {
                    Some(Segment::Map { key }) => params
                        .iter()
                        .find(|(name, _)| *name == key.as_str())
                        .map(|(name, _)| name),
                    Some(Segment::Seq { index }) => params.iter().nth(*index).map(|(name, _)| name),
                    _ => params.iter().exactly_one().ok().map(|(name, _)| name),
                };
                invalid_path(name)
            }
            _ => invalid_path(None),
        }
    }
}

fn invalid_path(name: Option<&str>) -> Problem {
    with_param(
        Problem::new(Code::BadRequest).detail_static(PATH_DETAIL),
        name.and_then(|name| param_name("path.", name)),
    )
}

fn param_name(prefix: &str, name: &str) -> Option<CompactString> {
    (name.len() <= MAX_PARAM_NAME_BYTES - prefix.len()).then(|| format_compact!("{prefix}{name}"))
}

fn wiring_failure(extractor: &'static str) -> Problem {
    tracing::error!(extractor, "http_extractor_wiring_failed");
    sanitized_problem()
}

fn with_param(problem: Problem, name: Option<CompactString>) -> Problem {
    match name {
        Some(name) => problem.invalid_param(name, CompactString::const_new(SCHEMA_REASON)),
        None => problem,
    }
}

fn body_pointer(path: &serde_path_to_error::Path) -> Option<CompactString> {
    let mut pointer = CompactString::default();
    for segment in path {
        pointer.push('/');
        match segment {
            Segment::Seq { index } => {
                pointer.push_str(index.format_into(&mut std::fmt::NumBuffer::new()));
            }
            Segment::Map { key } | Segment::Enum { variant: key } => {
                if key.len() > MAX_PARAM_NAME_BYTES.saturating_sub(pointer.len()) {
                    return None;
                }
                pointer.push_str(&shared_rules::rfc6901::escape(key));
            }
            Segment::Unknown => return None,
        }
        if pointer.len() > MAX_PARAM_NAME_BYTES {
            return None;
        }
    }
    (!pointer.is_empty()).then_some(pointer)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::http::{Method, Request as HttpRequest};
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::problem::SANITIZED_DETAIL;
    use crate::test_support::body_json;
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
    struct Filters {
        #[serde(default)]
        quantity: Vec<u32>,
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
        let count_order_lines =
            |Json(order): Json<Order>| async move { order.lines.len().to_string() };
        let list_orders = |Query(_): Query<Page>| async { "page" };
        let filtered = |Query(filters): Query<Filters>| async move {
            filters.quantity.iter().sum::<u32>().to_string()
        };
        let get_order = |Path(_): Path<u32>| async { "order" };
        let get_order_line = |Path(_): Path<(u32, u32)>| async { "line" };
        let miswired_pair = |Path(_): Path<(u32, u32)>| async { "unreachable" };
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
            .route("/orders", post(count_order_lines))
            .route("/orders", get(list_orders))
            .route("/lines", get(filtered))
            .route("/orders/{id}", get(get_order))
            .route("/orders/{id}/lines/{line}", get(get_order_line))
            .route("/miswired/{id}", get(miswired_pair))
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
                admission_wait: Duration::from_secs(1),
                log_health_probes: false,
                trust_trace_context: false,
                draining: None,
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

    async fn problem(response: Response, status: StatusCode, code: &str) -> Value {
        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let request_id = response.headers().get(&REQUEST_ID_HEADER).cloned();
        let body = body_json(response).await;
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
        let json = Json(json!({"slug": "a"}));
        assert_eq!(json.clone().into_response().status(), StatusCode::OK);
        let response = json.with_status(StatusCode::CREATED);
        assert_eq!(response.status(), StatusCode::CREATED);
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
        let response = Json(unserializable).with_status(StatusCode::CREATED);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        let problem = body_json(response).await;
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
            br#"{"slug": submitted-secret"#.as_slice(),
            br#"{"slug":1,"lines":[]} ]"#,
            br#"{"slug":1,"lines":[}"#,
            br#"{"slug":"a","lines":[]} ]"#,
            b"{\"slug\":1,\"lines\":[{\"quantity\":\"\xff\"}]}",
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
    async fn a_repeated_query_parameter_fills_a_list() {
        for (uri, sum) in [
            ("/lines", "0"),
            ("/lines?quantity=3", "3"),
            ("/lines?quantity=3&quantity=4", "7"),
        ] {
            let response = app().oneshot(get_uri(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                sum,
                "{uri}"
            );
        }

        let response = app()
            .oneshot(get_uri("/lines?quantity=3&quantity=submitted-secret"))
            .await
            .unwrap();
        let body = problem(response, StatusCode::BAD_REQUEST, "bad_request").await;
        assert_eq!(
            body["invalid_params"],
            json!([{"name": "query.quantity", "reason": SCHEMA_REASON}])
        );
        assert!(!body.to_string().contains("submitted-secret"), "{body}");
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
            inner: Vec<u32>,
        }
        for (input, pointer) in [
            (r#"{"a/b~c":["x"]}"#, "/a~1b~0c/0"),
            (r#"{"a/b~c":[0,1,2,3,4,5,6,7,8,9,"x"]}"#, "/a~1b~0c/10"),
        ] {
            let mut deserializer = serde_json::Deserializer::from_str(input);
            let error = serde_path_to_error::deserialize::<_, Outer>(&mut deserializer)
                .err()
                .expect("the member does not fit");
            assert_eq!(body_pointer(error.path()).as_deref(), Some(pointer));
        }
    }
}
