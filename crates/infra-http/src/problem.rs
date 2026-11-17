use std::time::Duration;

use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use compact_str::CompactString;
use serde::Serialize;
pub use service_failure::Code;
pub use service_failure::SANITIZED_DETAIL;
use utoipa::ToSchema;

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

/// RFC 9457 problem details with a stable `code` from the closed failure catalog.
#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    /// Stable machine-readable error code.
    #[schema(value_type = String, example = json!(Code::BadRequest.as_str()))]
    code: Code,
    /// Always `about:blank`: the problem has no semantics beyond its HTTP
    /// status, and `code` names the specific failure.
    #[serde(rename = "type")]
    #[schema(format = "uri-reference", example = json!(ABOUT_BLANK))]
    type_uri: &'static str,
    /// The HTTP status phrase.
    #[schema(example = json!(http_title(Code::BadRequest)))]
    title: &'static str,
    #[schema(example = json!(http_status(Code::BadRequest).as_u16()))]
    status: u16,
    #[schema(value_type = Option<String>, nullable = false, example = "invalid request framing")]
    detail: Option<CompactString>,
    /// URI reference identifying this occurrence when the service exposes
    /// one.
    #[schema(value_type = Option<String>, nullable = false, format = "uri-reference")]
    instance: Option<CompactString>,
    #[schema(value_type = Option<String>, nullable = false, example = "9ccecdfd-92c9-4665-a464-06f8ba73cd77")]
    request_id: Option<CompactString>,
    /// Which parts of the request failed validation, following the RFC 9457
    /// extension-member example. Present only on a validation rejection,
    /// and never carrying the submitted value.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    invalid_params: Vec<InvalidParam>,
    #[serde(skip)]
    retry_after: Option<Duration>,
}

impl Problem {
    #[must_use]
    pub fn new(code: Code) -> Self {
        Self {
            code,
            type_uri: ABOUT_BLANK,
            title: http_title(code),
            status: http_status(code).as_u16(),
            detail: None,
            instance: None,
            request_id: None,
            invalid_params: Vec::new(),
            retry_after: None,
        }
    }

    #[must_use]
    pub fn code(&self) -> Code {
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
    fn with_request_id(mut self, id: &str) -> Self {
        self.request_id = Some(CompactString::from(id));
        self
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
        let retry_after = self.retry_after;
        let mut response = (
            http_status(self.code),
            [(
                CONTENT_TYPE,
                HeaderValue::from_static("application/problem+json"),
            )],
            axum::Json(&self),
        )
            .into_response();
        response.extensions_mut().insert(self);
        if let Some(after) = retry_after {
            response
                .headers_mut()
                .insert(RETRY_AFTER, retry_after_value(after));
        }
        response
    }
}

pub(crate) fn complete_problem(response: &mut Response, request_id: &str) {
    let Some(problem) = response.extensions_mut().remove::<Problem>() else {
        return;
    };
    let problem = problem.with_request_id(request_id);
    *response.body_mut() = axum::Json(&problem).into_response().into_body();
    response.headers_mut().remove(CONTENT_LENGTH);
    response.extensions_mut().insert(problem);
}

fn retry_after_value(after: Duration) -> HeaderValue {
    let seconds = after
        .as_secs()
        .checked_add(u64::from(after.subsec_nanos() != 0));
    match seconds {
        Some(seconds) => HeaderValue::from(seconds.max(1)),
        None => HeaderValue::from_static("18446744073709551616"),
    }
}

#[must_use]
pub fn body_too_large() -> Problem {
    Problem::new(Code::RequestEntityTooLarge)
        .detail_static("request body exceeds the configured limit")
}

#[must_use]
pub fn caused_by_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    shared_rules::error_chain::causes(error)
        .any(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
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
        let problem = Problem::new(Code::ServiceUnavailable)
            .detail(service_failure::AT_CAPACITY_DETAIL)
            .with_request_id("req-1")
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
