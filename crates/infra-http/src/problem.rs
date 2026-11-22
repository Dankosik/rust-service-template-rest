use std::borrow::Cow;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{Extensions, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::{BufMut as _, BytesMut};
use compact_str::CompactString;
use http_body::Body as _;
use serde::Serialize;
pub use service_failure::Code;
pub use service_failure::SANITIZED_DETAIL;
use tower_http::request_id::RequestId;
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

#[allow(
    clippy::match_same_arms,
    reason = "Optional profiles remove complete match arms independently."
)]
pub(crate) const fn http_status(code: Code) -> StatusCode {
    match code {
        Code::BadRequest => StatusCode::BAD_REQUEST,
        Code::Unauthorized => StatusCode::UNAUTHORIZED,
        // template:begin authn:http-authentication-code-meta
        Code::AuthenticationRequired | Code::AuthenticationInvalid => StatusCode::UNAUTHORIZED,
        Code::AuthenticationMalformed => StatusCode::BAD_REQUEST,
        Code::AuthenticationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        // template:end authn:http-authentication-code-meta
        // template:begin http-idempotency:http-idempotency-code-meta
        Code::IdempotencyRequestInProgress => StatusCode::CONFLICT,
        Code::IdempotencyKeyMismatch => StatusCode::UNPROCESSABLE_ENTITY,
        Code::IdempotencyUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        // template:end http-idempotency:http-idempotency-code-meta
        // template:begin inbound-webhooks:http-webhook-code-meta
        Code::WebhookRejected => StatusCode::BAD_REQUEST,
        // template:end inbound-webhooks:http-webhook-code-meta
        Code::Forbidden => StatusCode::FORBIDDEN,
        Code::NotFound => StatusCode::NOT_FOUND,
        Code::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        Code::Conflict | Code::AlreadyExists => StatusCode::CONFLICT,
        Code::RequestEntityTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        Code::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
        Code::UnprocessableContent => StatusCode::UNPROCESSABLE_ENTITY,
        Code::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
        Code::InternalServerError => StatusCode::INTERNAL_SERVER_ERROR,
        Code::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        Code::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
        Code::GatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
    }
}

pub(crate) const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";

// RFC 9457 §4.2.1
const ABOUT_BLANK: &str = "about:blank";

fn http_title(code: Code) -> &'static str {
    match http_status(code) {
        StatusCode::PAYLOAD_TOO_LARGE => "Content Too Large",
        StatusCode::UNPROCESSABLE_ENTITY => "Unprocessable Content",
        status => status.canonical_reason().unwrap_or_default(),
    }
}

/// Which part of a request failed validation, following the RFC 9457
/// extension-member example. Never carries the submitted value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InvalidParam {
    /// An RFC 6901 JSON pointer for a request-body member, or a
    /// `location.name` pair such as `query.limit` for a parameter.
    #[schema(value_type = String, example = "/slug")]
    pub name: CompactString,
    /// Why the contract rejected it. Names the constraint that failed and
    /// never echoes the submitted value.
    #[schema(value_type = String, example = "maximum string length is 64")]
    pub reason: CompactString,
}

#[derive(Clone, Debug)]
pub struct Problem {
    code: Code,
    detail: Option<CompactString>,
    instance: Option<CompactString>,
    request_id: Option<CompactString>,
    invalid_params: Vec<InvalidParam>,
    retry_after: Option<Duration>,
}

const _: () = assert!(std::mem::size_of::<Problem>() <= 128);

#[allow(
    single_use_lifetimes,
    reason = "utoipa::ToSchema derive emits impl<'a> without #[automatically_derived]"
)]
mod wire {
    use serde::Serialize;
    use utoipa::ToSchema;

    use super::{ABOUT_BLANK, Code, InvalidParam, http_status, http_title};

    /// RFC 9457 problem details with a stable `code` from the closed failure catalog.
    #[serde_with::skip_serializing_none]
    #[derive(Serialize, ToSchema)]
    #[serde(deny_unknown_fields)]
    pub(super) struct ProblemWire<'a> {
        /// Stable machine-readable error code.
        #[schema(value_type = String, example = json!(Code::BadRequest.as_str()))]
        pub(super) code: Code,
        /// Always `about:blank`: the problem has no semantics beyond its HTTP
        /// status, and `code` names the specific failure.
        #[serde(rename = "type")]
        #[schema(format = "uri-reference", example = json!(ABOUT_BLANK))]
        pub(super) type_uri: &'static str,
        /// The HTTP status phrase.
        #[schema(example = json!(http_title(Code::BadRequest)))]
        pub(super) title: &'static str,
        #[schema(example = json!(http_status(Code::BadRequest).as_u16()))]
        pub(super) status: u16,
        #[schema(value_type = Option<String>, nullable = false, example = "invalid request framing")]
        pub(super) detail: Option<&'a str>,
        /// URI reference identifying this occurrence when the service exposes
        /// one.
        #[schema(value_type = Option<String>, nullable = false, format = "uri-reference")]
        pub(super) instance: Option<&'a str>,
        #[schema(value_type = Option<String>, nullable = false, example = "9ccecdfd-92c9-4665-a464-06f8ba73cd77")]
        pub(super) request_id: Option<&'a str>,
        /// Which parts of the request failed validation, following the RFC 9457
        /// extension-member example. Present only on a validation rejection,
        /// and never carrying the submitted value.
        #[serde(skip_serializing_if = "is_empty_params")]
        pub(super) invalid_params: &'a [InvalidParam],
    }

    const fn is_empty_params(params: &[InvalidParam]) -> bool {
        params.is_empty()
    }
}

impl Serialize for Problem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        wire::ProblemWire {
            code: self.code,
            type_uri: ABOUT_BLANK,
            title: http_title(self.code),
            status: http_status(self.code).as_u16(),
            detail: self.detail.as_deref(),
            instance: self.instance.as_deref(),
            request_id: self.request_id.as_deref(),
            invalid_params: &self.invalid_params,
        }
        .serialize(serializer)
    }
}

impl PartialSchema for Problem {
    fn schema() -> RefOr<Schema> {
        <wire::ProblemWire<'static> as PartialSchema>::schema()
    }
}

impl ToSchema for Problem {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Problem")
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        <wire::ProblemWire<'static> as ToSchema>::schemas(schemas);
    }
}

impl Problem {
    #[must_use]
    pub const fn new(code: Code) -> Self {
        Self {
            code,
            detail: None,
            instance: None,
            request_id: None,
            invalid_params: Vec::new(),
            retry_after: None,
        }
    }

    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<CompactString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub(crate) fn detail_static(mut self, detail: &'static str) -> Self {
        self.detail = Some(CompactString::const_new(detail));
        self
    }

    #[must_use]
    pub fn instance(mut self, instance: impl Into<CompactString>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    #[must_use]
    pub(crate) fn with_request_id(mut self, request_id: Option<&RequestId>) -> Self {
        if let Some(id) = request_id.and_then(|value| value.header_value().to_str().ok()) {
            self.request_id = Some(CompactString::from(id));
        }
        self
    }

    #[must_use]
    pub(crate) fn with_request_id_from(self, extensions: &Extensions) -> Self {
        self.with_request_id(extensions.get::<RequestId>())
    }

    #[must_use]
    pub fn invalid_param(
        mut self,
        name: impl Into<CompactString>,
        reason: impl Into<CompactString>,
    ) -> Self {
        self.invalid_params.push(InvalidParam {
            name: name.into(),
            reason: reason.into(),
        });
        self
    }

    #[must_use]
    pub fn retry_after(mut self, after: Duration) -> Self {
        self.retry_after = Some(after);
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let body = self.encode_body().unwrap_or_else(Body::empty);
        self.attach_to(Response::new(body))
    }
}

impl Problem {
    fn encode_body(&self) -> Option<Body> {
        let mut writer = BytesMut::with_capacity(256).writer();
        if serde_json::to_writer(&mut writer, self).is_err() {
            tracing::error!(
                code = self.code.as_str(),
                "http_problem_serialization_failed"
            );
            return None;
        }
        Some(Body::from(writer.into_inner().freeze()))
    }

    fn attach_to(self, mut response: Response) -> Response {
        *response.status_mut() = http_status(self.code);
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(PROBLEM_MEDIA_TYPE));
        if let Some(after) = self.retry_after {
            response
                .headers_mut()
                .insert(RETRY_AFTER, retry_after_value(after));
        }
        response.extensions_mut().insert(self);
        response
    }
}

pub(crate) fn pending_problem(problem: Problem) -> Response {
    problem.attach_to(Response::new(Body::empty()))
}

pub(crate) fn complete_problem(
    response: &mut Response,
    request_id: Option<&HeaderValue>,
) -> Option<Code> {
    if response.extensions().is_empty() {
        return None;
    }
    let body_empty = response.body().is_end_stream();
    let problem = response.extensions_mut().get_mut::<Problem>()?;
    let code = problem.code;
    let id = request_id.and_then(|value| value.to_str().ok());
    if !body_empty && (id.is_none() || problem.request_id.as_deref() == id) {
        return Some(code);
    }
    if let Some(id) = id {
        problem.request_id = Some(CompactString::from(id));
    }
    if let Some(body) = problem.encode_body() {
        *response.body_mut() = body;
        response.headers_mut().remove(CONTENT_LENGTH);
    }
    Some(code)
}

const U64_MAX_PLUS_ONE_SECS: &str = "18446744073709551616";

fn retry_after_value(after: Duration) -> HeaderValue {
    let seconds = after
        .as_secs()
        .checked_add(u64::from(after.subsec_nanos() != 0));
    match seconds {
        Some(0 | 1) => HeaderValue::from_static("1"),
        Some(seconds) => HeaderValue::from(seconds),
        None => HeaderValue::from_static(U64_MAX_PLUS_ONE_SECS),
    }
}

#[must_use]
pub fn body_too_large() -> Problem {
    Problem::new(Code::RequestEntityTooLarge)
        .detail_static("request body exceeds the configured limit")
}

#[must_use]
pub fn caused_by_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::contains::<http_body_util::LengthLimitError>(error)
}

pub(crate) fn body_unreadable() -> Problem {
    Problem::new(Code::BadRequest).detail_static("request body could not be read")
}

#[must_use]
pub fn sanitized_problem() -> Problem {
    Problem::new(Code::InternalServerError).detail_static(SANITIZED_DETAIL)
}

pub(crate) fn sanitized_internal_error() -> Response {
    sanitized_problem().into_response()
}

pub mod responses {
    use utoipa::{OpenApi, ToResponse};

    use super::Problem;

    /// request is malformed or invalid
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct BadRequest(pub Problem);

    /// request body exceeds configured limit
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct RequestEntityTooLarge(pub Problem);

    /// Content-Type is missing or is not `application/json` or an `application/*+json` type
    #[derive(Debug, ToResponse)]
    #[expect(
        dead_code,
        reason = "The response body exists only for the OpenAPI derive"
    )]
    #[response(content_type = "application/problem+json")]
    pub(crate) struct UnsupportedMediaType(pub Problem);

    /// request body was not received within the request budget
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json", headers(("Connection" = String, description = "close when the upload times out over HTTP/1.x")))]
    pub struct RequestTimeout(pub Problem);

    /// unexpected server failure
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct InternalServerError(pub Problem);

    /// the request budget was exhausted
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct GatewayTimeout(pub Problem);

    /// request admission is at capacity
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json", headers(("Retry-After" = u64, description = "seconds to wait before retrying when admission rejects the request at capacity")))]
    pub struct AdmissionUnavailable(pub Problem);

    #[derive(OpenApi)]
    #[openapi(components(
        schemas(Problem),
        responses(BadRequest, RequestEntityTooLarge, RequestTimeout, GatewayTimeout,
            InternalServerError, AdmissionUnavailable
            // template:begin authn:http-authentication-problem-components
            , crate::authn::AuthenticationMalformed, crate::authn::AuthenticationUnauthorized,
            crate::authn::AuthenticationForbidden, crate::authn::AuthenticationUnavailable
            // template:end authn:http-authentication-problem-components
        )
    ))]
    pub(crate) struct ProblemComponents;
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_support::body_json;

    #[test]
    fn every_code_maps_to_an_error_status_and_a_title() {
        for code in Code::ALL {
            assert!(http_status(*code).is_client_error() || http_status(*code).is_server_error());
            assert_ne!(http_title(*code), "");
        }
        assert_eq!(http_status(Code::AlreadyExists), StatusCode::CONFLICT);
        assert_eq!(http_title(Code::RequestEntityTooLarge), "Content Too Large");
        assert_eq!(
            http_title(Code::UnprocessableContent),
            "Unprocessable Content"
        );
        assert_eq!(
            http_status(Code::GatewayTimeout),
            StatusCode::GATEWAY_TIMEOUT
        );
    }

    #[tokio::test]
    async fn renders_problem_json_with_optional_members() {
        let request_id = RequestId::new(HeaderValue::from_static("req-1"));
        let problem = Problem::new(Code::ServiceUnavailable)
            .detail(service_failure::AT_CAPACITY_DETAIL)
            .with_request_id(Some(&request_id))
            .retry_after(Duration::from_millis(200));
        let response = problem.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "1");
        assert_eq!(
            response.extensions().get::<Problem>().map(Problem::code),
            Some(Code::ServiceUnavailable)
        );
        let json = body_json(response).await;
        assert_eq!(json["code"], "service_unavailable");
        assert_eq!(json["status"], 503);
        assert_eq!(json["title"], "Service Unavailable");
        assert_eq!(json["detail"], service_failure::AT_CAPACITY_DETAIL);
        assert_eq!(json["request_id"], "req-1");
        assert_eq!(json["type"], "about:blank");
        assert!(json.get("invalid_params").is_none());
        assert!(json.get("instance").is_none());
    }

    #[tokio::test]
    async fn complete_problem_reuses_encoded_body_when_request_id_matches_and_rewrites_otherwise() {
        use http_body_util::BodyExt as _;

        let req_1 = HeaderValue::from_static("req-1");
        let req_2 = HeaderValue::from_static("req-2");
        let request_id = RequestId::new(req_1.clone());

        let mut plain = Response::new(Body::from("ok"));
        assert_eq!(complete_problem(&mut plain, Some(&req_1)), None);

        let mut already_encoded = Problem::new(Code::BadRequest)
            .with_request_id(Some(&request_id))
            .attach_to(Response::new(Body::from("pre-encoded")));
        assert_eq!(
            complete_problem(&mut already_encoded, Some(&req_1)),
            Some(Code::BadRequest)
        );
        let bytes = already_encoded
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(bytes.as_ref(), b"pre-encoded");

        let mut stale_id = Problem::new(Code::BadRequest)
            .with_request_id(Some(&request_id))
            .attach_to(Response::new(Body::from("pre-encoded")));
        assert_eq!(
            complete_problem(&mut stale_id, Some(&req_2)),
            Some(Code::BadRequest)
        );
        assert_eq!(body_json(stale_id).await["request_id"], "req-2");

        let mut pending =
            pending_problem(Problem::new(Code::BadRequest).with_request_id(Some(&request_id)));
        assert_eq!(
            complete_problem(&mut pending, Some(&req_1)),
            Some(Code::BadRequest)
        );
        assert_eq!(body_json(pending).await["request_id"], "req-1");

        let mut pending_without_id = pending_problem(Problem::new(Code::BadRequest));
        assert_eq!(
            complete_problem(&mut pending_without_id, None),
            Some(Code::BadRequest)
        );
        let body = body_json(pending_without_id).await;
        assert_eq!(body["code"], "bad_request");
        assert!(body.get("request_id").is_none());
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds_with_a_minimum_of_one() {
        for (duration, expected) in [
            (Duration::ZERO, "1"),
            (Duration::from_nanos(1), "1"),
            (Duration::from_nanos(999_999_999), "1"),
            (Duration::from_secs(1), "1"),
            (Duration::new(1, 1), "2"),
            (Duration::from_millis(1_999), "2"),
            (Duration::from_secs(2), "2"),
            (Duration::new(u64::MAX - 1, 1), "18446744073709551615"),
            (Duration::from_secs(u64::MAX), "18446744073709551615"),
            (Duration::new(u64::MAX, 1), "18446744073709551616"),
            (Duration::MAX, "18446744073709551616"),
        ] {
            let response = Problem::new(Code::TooManyRequests)
                .retry_after(duration)
                .into_response();
            assert_eq!(
                response.headers().get(RETRY_AFTER).unwrap(),
                expected,
                "duration: {duration:?}"
            );
        }
        assert!(
            Problem::new(Code::BadRequest)
                .into_response()
                .headers()
                .get(RETRY_AFTER)
                .is_none()
        );
    }

    #[tokio::test]
    async fn invalid_params_never_carry_values() {
        let response = Problem::new(Code::BadRequest)
            .invalid_param("/slug", "maximum string length is 64")
            .into_response();
        let json = body_json(response).await;
        assert_eq!(json["invalid_params"][0]["name"], "/slug");
        assert_eq!(
            json["invalid_params"][0]["reason"],
            "maximum string length is 64"
        );
    }
}
