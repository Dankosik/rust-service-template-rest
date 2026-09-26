//! RFC 9457 problem details: the closed catalog of client-visible failures.
//!
//! One catalog on purpose: a second copy drifts, and a status advertising the
//! wrong type URI is what a client keys its retry policy off. `Code` is the
//! stable machine-readable identity; this module projects it into status,
//! title, and type URI. A code with no matching response in a service's
//! contract is
//! unreachable, not wrong. Connection-layer outcomes (hyper 431, the
//! accept-cap close, a silent first-byte close) are not `Problem` values
//! even when a matching `Code` exists in the catalog.
//!
//! The same types describe themselves in the OpenAPI document: `ToSchema`
//! renders the `Problem` and `InvalidParam` schemas from the serializer, and
//! [`responses`] holds the reusable `application/problem+json` responses
//! operations reference. Doc comments on those items are contract text.

use std::time::Duration;

use axum::http::header::{CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
pub use service_failure::Code;
pub use service_failure::{AT_CAPACITY_DETAIL, SANITIZED_DETAIL};
use utoipa::ToSchema;

/// HTTP's RFC 9457 projection of the shared failure identity.
const fn http_meta(code: Code) -> HttpCodeMeta {
    match code {
            Code::BadRequest => HttpCodeMeta {
                status: StatusCode::BAD_REQUEST,
                title: "bad request",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.1"),
            },
            Code::Unauthorized => HttpCodeMeta {
                status: StatusCode::UNAUTHORIZED,
                title: "unauthorized",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.2"),
            },
            // template:begin authn:http-authentication-code-meta
            Code::AuthenticationRequired => HttpCodeMeta {
                status: StatusCode::UNAUTHORIZED,
                title: "authentication required",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.2"),
            },
            Code::AuthenticationMalformed => HttpCodeMeta {
                status: StatusCode::BAD_REQUEST,
                title: "authentication malformed",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.1"),
            },
            Code::AuthenticationInvalid => HttpCodeMeta {
                status: StatusCode::UNAUTHORIZED,
                title: "authentication invalid",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.2"),
            },
            Code::AuthenticationUnavailable => HttpCodeMeta {
                status: StatusCode::SERVICE_UNAVAILABLE,
                title: "authentication unavailable",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.4"),
            },
            // template:end authn:http-authentication-code-meta
            // template:begin http-idempotency:http-idempotency-code-meta
            Code::IdempotencyRequestInProgress => HttpCodeMeta {
                status: StatusCode::CONFLICT,
                title: "conflict",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.10"),
            },
            Code::IdempotencyKeyMismatch => HttpCodeMeta {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                title: "unprocessable content",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.21"),
            },
            Code::IdempotencyUnavailable => HttpCodeMeta {
                status: StatusCode::SERVICE_UNAVAILABLE,
                title: "service unavailable",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.4"),
            },
            // template:end http-idempotency:http-idempotency-code-meta
            // template:begin inbound-webhooks:http-webhook-code-meta
            Code::WebhookRejected => HttpCodeMeta {
                status: StatusCode::BAD_REQUEST,
                title: "webhook rejected",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.1"),
            },
            Code::WebhookConflict => HttpCodeMeta {
                status: StatusCode::CONFLICT,
                title: "webhook conflict",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.10"),
            },
            // template:end inbound-webhooks:http-webhook-code-meta
            Code::Forbidden => HttpCodeMeta {
                status: StatusCode::FORBIDDEN,
                title: "forbidden",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.4"),
            },
            Code::NotFound => HttpCodeMeta {
                status: StatusCode::NOT_FOUND,
                title: "not found",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.5"),
            },
            Code::MethodNotAllowed => HttpCodeMeta {
                status: StatusCode::METHOD_NOT_ALLOWED,
                title: "method not allowed",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.6"),
            },
            Code::Conflict => HttpCodeMeta {
                status: StatusCode::CONFLICT,
                title: "conflict",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.10"),
            },
            Code::AlreadyExists => HttpCodeMeta {
                status: StatusCode::CONFLICT,
                title: "conflict",
                // 409 has one RFC type URI and title; Conflict vs AlreadyExists
                // are sibling `code` values in that class, not two problem types.
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.10"),
            },
            Code::RequestEntityTooLarge => HttpCodeMeta {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                title: "request entity too large",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.14"),
            },
            // RFC 9110 stops at 426; 431 and 429 are defined by RFC 6585.
            Code::RequestHeaderFieldsTooLarge => HttpCodeMeta {
                status: StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
                title: "request header fields too large",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc6585", "#section-5"),
            },
            Code::UnprocessableContent => HttpCodeMeta {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                title: "unprocessable content",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.5.21"),
            },
            Code::TooManyRequests => HttpCodeMeta {
                status: StatusCode::TOO_MANY_REQUESTS,
                title: "too many requests",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc6585", "#section-4"),
            },
            Code::InternalServerError => HttpCodeMeta {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                title: "internal server error",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.1"),
            },
            Code::ServiceUnavailable => HttpCodeMeta {
                status: StatusCode::SERVICE_UNAVAILABLE,
                title: "service unavailable",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.4"),
            },
            Code::RequestTimeout => HttpCodeMeta {
                status: StatusCode::GATEWAY_TIMEOUT,
                title: "request timeout",
                type_uri: concat!("https://www.rfc-editor.org/rfc/rfc9110", "#section-15.6.5"),
            },
    }
}

struct HttpCodeMeta {
    status: StatusCode,
    title: &'static str,
    type_uri: &'static str,
}

pub(crate) const fn http_status(code: Code) -> StatusCode {
    http_meta(code).status
}

const fn http_title(code: Code) -> &'static str {
    http_meta(code).title
}

const fn http_type_uri(code: Code) -> &'static str {
    http_meta(code).type_uri
}

/// Which part of a request failed validation, following the RFC 9457
/// extension-member example. Never carries the submitted value.
// `deny_unknown_fields` on a type that is only serialized has no serde
// effect; it is what renders `additionalProperties: false` in the schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InvalidParam {
    /// An RFC 6901 JSON pointer for a request-body member, or a
    /// `location.name` pair such as `query.limit` for a parameter.
    #[schema(example = "/slug")]
    pub name: String,
    /// Why the contract rejected it. Names the constraint that failed and
    /// never echoes the submitted value.
    #[schema(example = "maximum string length is 64")]
    pub reason: String,
}

/// RFC 9457 problem details: the failure envelope of every non-success
/// response, keyed by a stable `code` from one closed catalog.
// Build with `Problem::new`, add context with the builder methods, and
// return it from a handler or middleware. Where the schema deliberately
// differs from the Rust type: optional members are omitted, never `null`,
// which the `nullable = false` annotations tell the contract; `code` is a
// plain `string` rather than the closed `Code` enum, because oasdiff treats
// a new enum value in a response as a breaking change and the catalog grows
// with features; `deny_unknown_fields` has no serde effect on a type that is
// only serialized and exists to render `additionalProperties: false`. The
// examples are `Code::BadRequest` as the catalog renders it, so a catalog
// edit regenerates them instead of leaving a stale copy.
#[serde_with::skip_serializing_none]
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    /// Stable machine-readable error code.
    #[schema(value_type = String, example = json!(Code::BadRequest.as_str()))]
    code: Code,
    /// Stable URI reference identifying the problem class.
    #[serde(rename = "type")]
    #[schema(
        format = "uri-reference",
        example = json!(http_type_uri(Code::BadRequest))
    )]
    type_uri: &'static str,
    #[schema(example = json!(http_title(Code::BadRequest)))]
    title: &'static str,
    #[schema(example = json!(http_status(Code::BadRequest).as_u16()))]
    status: u16,
    #[schema(nullable = false, example = "invalid request framing")]
    detail: Option<String>,
    /// URI reference identifying this occurrence when the service exposes
    /// one.
    #[schema(nullable = false, format = "uri-reference")]
    instance: Option<String>,
    #[schema(nullable = false, example = "9ccecdfd-92c9-4665-a464-06f8ba73cd77")]
    request_id: Option<String>,
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
            type_uri: http_type_uri(code),
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
        let code = self.code;
        let retry_after = self.retry_after;
        // Problem contains only infallibly serializable strings and integers.
        // The extension lets the access log distinguish codes sharing a status.
        let mut response = (
            http_status(code),
            [(
                CONTENT_TYPE,
                HeaderValue::from_static("application/problem+json"),
            )],
            axum::Extension(code),
            axum::Json(self),
        )
            .into_response();
        if let Some(after) = retry_after {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(after.as_secs().max(1)));
        }
        response
    }
}

/// Reusable problem responses (`#/components/responses/<Name>`) and their
/// registration in the document.
///
/// Each newtype is the declared shape of one status an operation can answer
/// with a [`Problem`]; an operation references it as
/// `(status = <code>, response = <Name>)`. The types exist for the document
/// and are not constructed at runtime: the runtime value is always a
/// `Problem`, whose code fixes the status. Their names are distinct from
/// [`Code`] on purpose: `Code::BadRequest` is what a handler returns,
/// `responses::BadRequest` is what the contract declares. A new status joins
/// this module with its first operation; the set every operation declares is
/// [`TransportProblemResponses`]. A response family owned by one optional
/// pack may instead live with that pack's seam, which registers it. Doc
/// comments on the newtypes are the response descriptions in the contract.
pub mod responses {
    use utoipa::{IntoResponses, OpenApi, ToResponse};

    use super::{InvalidParam, Problem};

    /// request is malformed or invalid
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct BadRequest(pub Problem);

    /// request body exceeds configured limit
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct RequestEntityTooLarge(pub Problem);

    /// unexpected server failure
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct InternalServerError(pub Problem);

    // template:begin inbound-webhooks:http-webhook-problem-responses
    /// webhook endpoint is unknown
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct NotFound(pub Problem);

    /// webhook signature evidence is missing, malformed, stale, or invalid
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct WebhookRejected(pub Problem);

    /// a repeated webhook identity supplied different bytes
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct WebhookConflict(pub Problem);

    /// durable webhook receipt storage is unavailable
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct ServiceUnavailable(pub Problem);

    /// The non-success webhook admission outcomes.  The route itself owns
    /// the signature requirement; OpenAPI's empty security array means only
    /// that bearer authentication is not the operation's authentication.
    #[derive(Debug, IntoResponses)]
    pub enum WebhookProblemResponses {
        #[response(status = 400)]
        Rejected(#[ref_response] WebhookRejected),
        #[response(status = 404)]
        NotFound(#[ref_response] NotFound),
        #[response(status = 409)]
        Conflict(#[ref_response] WebhookConflict),
        #[response(status = 413)]
        RequestEntityTooLarge(#[ref_response] RequestEntityTooLarge),
        #[response(status = 500)]
        InternalServerError(#[ref_response] InternalServerError),
        #[response(status = 503)]
        Unavailable(#[ref_response] ServiceUnavailable),
    }
    // template:end inbound-webhooks:http-webhook-problem-responses

    // template:begin authn:http-authentication-problem-responses
    /// bearer authentication is malformed
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct AuthenticationMalformed(pub Problem);

    /// bearer authentication is required or invalid
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct AuthenticationUnauthorized(pub Problem);

    /// downstream authorization denied the verified principal
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct AuthenticationForbidden(pub Problem);

    /// bearer authentication trust or provider is unavailable
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct AuthenticationUnavailable(pub Problem);

    /// the enclosing request budget expired before a response committed
    #[derive(Debug, ToResponse)]
    #[response(content_type = "application/problem+json")]
    pub struct RequestTimeout(pub Problem);

    /// Responses every protected operation declares in addition to its own
    /// success shape. Authentication never returns 403 itself; that status is
    /// reserved for authorization that follows a verified principal.
    #[derive(Debug, IntoResponses)]
    pub enum ProtectedOperationProblemResponses {
        #[response(status = 400)]
        Malformed(#[ref_response] AuthenticationMalformed),
        #[response(status = 401)]
        Unauthorized(#[ref_response] AuthenticationUnauthorized),
        #[response(status = 403)]
        Forbidden(#[ref_response] AuthenticationForbidden),
        #[response(status = 503)]
        Unavailable(#[ref_response] AuthenticationUnavailable),
        #[response(status = 504)]
        Timeout(#[ref_response] RequestTimeout),
    }
    // template:end authn:http-authentication-problem-responses

    /// The problem responses the OpenAPI document declares on every
    /// operation as shared transport answers. This is the Problem set every
    /// operation can name without colliding with probe-owned 503 `text/plain`.
    /// The hardened chain also emits Problem 503/504/405 at runtime; those
    /// stay out of this group (ready already documents 503 as `text/plain`).
    /// Header overflow is hyper-native 431 before the router, not a
    /// `Problem`. Name this type in `responses(...)` beside the operation's
    /// own answers; a new shared *Problem* status joins here once instead
    /// of in every operation.
    #[derive(Debug, IntoResponses)]
    pub enum TransportProblemResponses {
        #[response(status = 400)]
        BadRequest(#[ref_response] BadRequest),
        #[response(status = 413)]
        RequestEntityTooLarge(#[ref_response] RequestEntityTooLarge),
        #[response(status = 500)]
        InternalServerError(#[ref_response] InternalServerError),
    }

    /// Components referenced by responses rather than by a handler's `body`,
    /// which utoipa does not collect on its own; the router seeds its
    /// document with them. `info` is irrelevant here: the service document
    /// keeps its own when it merges this one.
    #[derive(OpenApi)]
    #[openapi(components(
        schemas(Problem, InvalidParam),
        responses(BadRequest, RequestEntityTooLarge, InternalServerError
            // template:begin inbound-webhooks:http-webhook-problem-components
            , NotFound, WebhookRejected, WebhookConflict, ServiceUnavailable
            // template:end inbound-webhooks:http-webhook-problem-components
            // template:begin authn:http-authentication-problem-components
            , AuthenticationMalformed, AuthenticationUnauthorized, AuthenticationForbidden,
            AuthenticationUnavailable, RequestTimeout
            // template:end authn:http-authentication-problem-components
        )
    ))]
    pub(crate) struct ProblemComponents;
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    #[test]
    fn every_code_has_a_distinct_wire_form_and_consistent_status() {
        use itertools::Itertools;
        assert!(Code::ALL.iter().map(|code| code.as_str()).all_unique());
        for code in Code::ALL {
            assert_eq!(code.to_string(), code.as_str());
            assert_eq!(
                serde_json::to_string(code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
            assert!(
                http_type_uri(*code)
                    .starts_with("https://www.rfc-editor.org/rfc/rfc")
            );
            assert!(!http_title(*code).is_empty());
        }
        assert_eq!(http_status(Code::AlreadyExists), StatusCode::CONFLICT);
        assert_eq!(
            http_type_uri(Code::AlreadyExists),
            http_type_uri(Code::Conflict)
        );
        assert_eq!(Code::RequestTimeout.as_str(), "request_timeout");
        assert_eq!(http_status(Code::RequestTimeout), StatusCode::GATEWAY_TIMEOUT);
    }

    #[tokio::test]
    async fn renders_problem_json_with_optional_members() {
        let problem = Problem::new(Code::ServiceUnavailable)
            .detail(AT_CAPACITY_DETAIL)
            .request_id(Some("req-1".to_owned()))
            .retry_after(Duration::from_millis(200));
        let expected = serde_json::to_vec(&problem).unwrap();
        let response = problem.into_response();
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
        assert_eq!(body.as_ref(), expected.as_slice());
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "service_unavailable");
        assert_eq!(json["status"], 503);
        assert_eq!(json["title"], "service unavailable");
        assert_eq!(json["detail"], AT_CAPACITY_DETAIL);
        assert_eq!(json["request_id"], "req-1");
        assert_eq!(json["type"], http_type_uri(Code::ServiceUnavailable));
        assert!(json.get("invalid_params").is_none());
        assert!(json.get("instance").is_none());
    }

    #[test]
    fn retry_after_keeps_whole_seconds_with_a_minimum_of_one() {
        for (duration, expected) in [
            (Duration::ZERO, "1"),
            (Duration::from_millis(1_999), "1"),
            (Duration::from_secs(2), "2"),
            (Duration::from_secs(u64::MAX), "18446744073709551615"),
        ] {
            let response = Problem::new(Code::TooManyRequests)
                .retry_after(duration)
                .into_response();
            assert_eq!(response.headers().get(RETRY_AFTER).unwrap(), expected);
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
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["invalid_params"][0]["name"], "/slug");
        assert_eq!(
            json["invalid_params"][0]["reason"],
            "maximum string length is 64"
        );
    }
}
