//! RFC 9457 problem details: the closed catalog of client-visible failures.
//!
//! One catalog on purpose: a second copy drifts, and a status advertising the
//! wrong type URI is what a client keys its retry policy off. `Code` is the
//! stable machine-readable identity; status, title, and type URI derive from
//! it. A code with no matching response in a service's contract is
//! unreachable, not wrong.

use std::time::Duration;

use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Stable machine-readable failure code a client matches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    MethodNotAllowed,
    Conflict,
    AlreadyExists,
    RequestEntityTooLarge,
    RequestHeaderFieldsTooLarge,
    UnprocessableContent,
    TooManyRequests,
    InternalError,
    ServiceUnavailable,
    GatewayTimeout,
}

/// Caller-visible text for every failure the transport refuses to describe:
/// an unclassified handler error, a recovered panic, a policy rejection.
/// One constant so those stay indistinguishable to a caller.
pub const SANITIZED_DETAIL: &str = "request failed";

/// Caller-visible text when admission control sheds a request.
pub const AT_CAPACITY_DETAIL: &str = "server is at capacity";

impl Code {
    /// Every published code, for catalog rendering and coverage tests.
    pub const ALL: &'static [Code] = &[
        Code::BadRequest,
        Code::Unauthorized,
        Code::Forbidden,
        Code::NotFound,
        Code::MethodNotAllowed,
        Code::Conflict,
        Code::AlreadyExists,
        Code::RequestEntityTooLarge,
        Code::RequestHeaderFieldsTooLarge,
        Code::UnprocessableContent,
        Code::TooManyRequests,
        Code::InternalError,
        Code::ServiceUnavailable,
        Code::GatewayTimeout,
    ];

    /// The `snake_case` wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::BadRequest => "bad_request",
            Code::Unauthorized => "unauthorized",
            Code::Forbidden => "forbidden",
            Code::NotFound => "not_found",
            Code::MethodNotAllowed => "method_not_allowed",
            Code::Conflict => "conflict",
            Code::AlreadyExists => "already_exists",
            Code::RequestEntityTooLarge => "request_entity_too_large",
            Code::RequestHeaderFieldsTooLarge => "request_header_fields_too_large",
            Code::UnprocessableContent => "unprocessable_content",
            Code::TooManyRequests => "too_many_requests",
            Code::InternalError => "internal_error",
            Code::ServiceUnavailable => "service_unavailable",
            Code::GatewayTimeout => "gateway_timeout",
        }
    }

    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Code::BadRequest => StatusCode::BAD_REQUEST,
            Code::Unauthorized => StatusCode::UNAUTHORIZED,
            Code::Forbidden => StatusCode::FORBIDDEN,
            Code::NotFound => StatusCode::NOT_FOUND,
            Code::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Code::Conflict | Code::AlreadyExists => StatusCode::CONFLICT,
            Code::RequestEntityTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Code::RequestHeaderFieldsTooLarge => StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            Code::UnprocessableContent => StatusCode::UNPROCESSABLE_ENTITY,
            Code::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Code::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
            Code::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Code::GatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Code::BadRequest => "bad request",
            Code::Unauthorized => "unauthorized",
            Code::Forbidden => "forbidden",
            Code::NotFound => "not found",
            Code::MethodNotAllowed => "method not allowed",
            Code::Conflict | Code::AlreadyExists => "conflict",
            Code::RequestEntityTooLarge => "request entity too large",
            Code::RequestHeaderFieldsTooLarge => "request header fields too large",
            Code::UnprocessableContent => "unprocessable content",
            Code::TooManyRequests => "too many requests",
            Code::InternalError => "internal server error",
            Code::ServiceUnavailable => "service unavailable",
            Code::GatewayTimeout => "gateway timeout",
        }
    }

    /// Stable URI identifying the problem class.
    #[must_use]
    pub const fn type_uri(self) -> &'static str {
        match self {
            Code::BadRequest => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.1")
            }
            Code::Unauthorized => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.2")
            }
            Code::Forbidden => concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.4"),
            Code::NotFound => concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.5"),
            Code::MethodNotAllowed => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.6")
            }
            Code::Conflict | Code::AlreadyExists => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.10")
            }
            Code::RequestEntityTooLarge => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.14")
            }
            // RFC 9110 stops at 426; 431 and 429 are defined by RFC 6585.
            Code::RequestHeaderFieldsTooLarge => {
                concat!("https://www.rfc-editor.org/rfc/rfc6585", "#section-5")
            }
            Code::UnprocessableContent => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.21")
            }
            Code::TooManyRequests => {
                concat!("https://www.rfc-editor.org/rfc/rfc6585", "#section-4")
            }
            Code::InternalError => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.1")
            }
            Code::ServiceUnavailable => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.4")
            }
            Code::GatewayTimeout => {
                concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.5")
            }
        }
    }
}

/// Which part of a request failed validation, following the RFC 9457
/// extension-member example. Never carries the submitted value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InvalidParam {
    /// An RFC 6901 JSON pointer for a body member, or `location.name` for a
    /// parameter such as `query.limit`.
    pub name: String,
    /// The constraint that failed.
    pub reason: String,
}

/// One problem response. Build with [`Problem::new`], add context with the
/// builder methods, and return it from a handler or middleware.
#[derive(Clone, Debug, Serialize)]
pub struct Problem {
    code: Code,
    #[serde(rename = "type")]
    type_uri: &'static str,
    title: &'static str,
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
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
            type_uri: code.type_uri(),
            title: code.title(),
            status: code.status().as_u16(),
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
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    #[must_use]
    pub fn request_id(mut self, request_id: Option<String>) -> Self {
        self.request_id = request_id;
        self
    }

    #[must_use]
    pub fn invalid_param(mut self, name: impl Into<String>, reason: impl Into<String>) -> Self {
        self.invalid_params.push(InvalidParam {
            name: name.into(),
            reason: reason.into(),
        });
        self
    }

    /// Emit a `Retry-After` header in whole seconds (at least one).
    #[must_use]
    pub fn retry_after(mut self, after: Duration) -> Self {
        self.retry_after = Some(after);
        self
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = self.code.status();
        let code = self.code;
        let retry_after = self.retry_after;
        // Serializing a struct of strings and integers cannot fail.
        let body = serde_json::to_vec(&self).unwrap_or_default();
        let mut response = (status, body).into_response();
        response.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        if let Some(after) = retry_after {
            let seconds = after.as_secs().max(1);
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
        }
        // The access log reads the code back to separate failures that
        // share a status.
        response.extensions_mut().insert(code);
        response
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    #[test]
    fn every_code_has_a_distinct_wire_form_and_consistent_status() {
        let mut seen = std::collections::HashSet::new();
        for code in Code::ALL {
            assert!(seen.insert(code.as_str()), "{} duplicated", code.as_str());
            assert_eq!(
                serde_json::to_string(code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
            assert!(
                code.type_uri()
                    .starts_with("https://www.rfc-editor.org/rfc/rfc")
            );
            assert!(!code.title().is_empty());
        }
        assert_eq!(Code::AlreadyExists.status(), StatusCode::CONFLICT);
        assert_eq!(Code::AlreadyExists.type_uri(), Code::Conflict.type_uri());
    }

    #[tokio::test]
    async fn renders_problem_json_with_optional_members() {
        let response = Problem::new(Code::ServiceUnavailable)
            .detail(AT_CAPACITY_DETAIL)
            .request_id(Some("req-1".to_owned()))
            .retry_after(Duration::from_millis(200))
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/problem+json"
        );
        assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), "1");
        assert_eq!(
            response.extensions().get::<Code>(),
            Some(&Code::ServiceUnavailable)
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "service_unavailable");
        assert_eq!(json["status"], 503);
        assert_eq!(json["title"], "service unavailable");
        assert_eq!(json["detail"], AT_CAPACITY_DETAIL);
        assert_eq!(json["request_id"], "req-1");
        assert_eq!(json["type"], Code::ServiceUnavailable.type_uri());
        assert!(json.get("invalid_params").is_none());
        assert!(json.get("instance").is_none());
    }

    #[tokio::test]
    async fn invalid_params_never_carry_values() {
        let response = Problem::new(Code::BadRequest)
            .invalid_param("/slug", "maximum string length is 64")
            .into_response();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["invalid_params"][0]["name"], "/slug");
        assert_eq!(
            json["invalid_params"][0]["reason"],
            "maximum string length is 64"
        );
    }
}
