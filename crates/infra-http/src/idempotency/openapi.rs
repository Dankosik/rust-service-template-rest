//! Contract types an idempotent operation names in its annotation, and the
//! values the declaration rules expect of it.
//!
//! The four response components below are registered only through
//! [`super::Composer::components`]; the reused authentication and transport
//! components come from [`crate::problem::responses`]. Doc comments on the
//! components and on [`IdempotencyKey`] are contract text.

use utoipa::{IntoParams, IntoResponses, OpenApi, ToResponse};

use crate::problem::Problem;
use crate::problem::responses::{
    AuthenticationForbidden, AuthenticationOversize, AuthenticationTimeout,
    AuthenticationUnauthorized, InternalServerError, RequestEntityTooLarge,
};

/// The key's header parameter name.
pub(super) const KEY_HEADER: &str = "Idempotency-Key";
/// The key's schema pattern: one or more RFC 9110 `tchar` characters.
pub(super) const KEY_PATTERN: &str = "^[!#$%&'*+.^_`|~0-9A-Za-z-]+$";
/// The key's shortest admitted length.
pub(super) const KEY_MIN_LENGTH: u64 = 1;
/// The key's longest admitted length.
pub(super) const KEY_MAX_LENGTH: u64 = 255;

/// Response component names the family registers.
pub(super) const BAD_REQUEST_COMPONENT: &str = "IdempotencyBadRequest";
pub(super) const REQUEST_IN_PROGRESS_COMPONENT: &str = "IdempotencyRequestInProgress";
pub(super) const KEY_MISMATCH_COMPONENT: &str = "IdempotencyKeyMismatch";
pub(super) const UNAVAILABLE_COMPONENT: &str = "IdempotencyUnavailable";
/// Every component the family registers; the document must contain them.
pub(super) const RESPONSE_COMPONENTS: [&str; 4] = [
    BAD_REQUEST_COMPONENT,
    REQUEST_IN_PROGRESS_COMPONENT,
    KEY_MISMATCH_COMPONENT,
    UNAVAILABLE_COMPONENT,
];

/// Statuses whose response must be exactly the component that
/// [`IdempotentOperationProblemResponses`] declares.
pub(super) const FIXED_PROBLEM_STATUSES: [&str; 8] =
    ["400", "401", "409", "422", "431", "500", "503", "504"];
/// The authorization status: any Problem response, so an operation may
/// describe its own authorization.
pub(super) const AUTHORIZATION_STATUS: &str = "403";
/// Headers a 2xx response of an idempotent operation may declare: the ones
/// a replay reproduces.
pub(super) const REPLAYABLE_HEADERS: [&str; 5] = [
    "Content-Type",
    "Content-Encoding",
    "Content-Language",
    "Content-Disposition",
    "Location",
];

/// The `Idempotency-Key` request header. Name it in `params(..)` of every
/// idempotent operation.
#[derive(Debug, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct IdempotencyKey {
    /// Client-chosen key for one intended effect. Retry with the same key to
    /// receive the recorded result instead of a second effect. Exactly one
    /// field of 1 to 255 RFC 9110 token characters, compared byte for byte.
    #[param(
        rename = "Idempotency-Key",
        min_length = 1,
        max_length = 255,
        pattern = "^[!#$%&'*+.^_`|~0-9A-Za-z-]+$"
    )]
    pub idempotency_key: String,
}

/// bearer authentication is malformed, the Idempotency-Key header is missing
/// or invalid, or the request is otherwise malformed or invalid
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct IdempotencyBadRequest(pub Problem);

/// a request with this Idempotency-Key is still in progress, so retry with
/// the same key later, or the operation reports its own conflict
#[derive(Debug, ToResponse)]
#[response(
    content_type = "application/problem+json",
    headers((
        "Retry-After" = u64,
        description = "seconds to wait before retrying with the same Idempotency-Key"
    ))
)]
pub struct IdempotencyRequestInProgress(pub Problem);

/// the Idempotency-Key is bound to a different request, or the operation
/// cannot process the request content
#[derive(Debug, ToResponse)]
#[response(content_type = "application/problem+json")]
pub struct IdempotencyKeyMismatch(pub Problem);

/// bearer authentication trust or provider is unavailable, idempotent request
/// processing is unavailable, or the outcome of this request is unknown;
/// retry with the same Idempotency-Key
#[derive(Debug, ToResponse)]
#[response(
    content_type = "application/problem+json",
    headers((
        "Retry-After" = u64,
        description = "seconds to wait before retrying with the same Idempotency-Key"
    ))
)]
pub struct IdempotencyUnavailable(pub Problem);

/// Responses every idempotent operation declares beside its own success
/// shape: authentication, the idempotency boundary, and the transport. The
/// 403 authorization response may be replaced by an operation-specific
/// Problem response.
#[derive(Debug, IntoResponses)]
pub enum IdempotentOperationProblemResponses {
    #[response(status = 400)]
    BadRequest(#[ref_response] IdempotencyBadRequest),
    #[response(status = 401)]
    Unauthorized(#[ref_response] AuthenticationUnauthorized),
    #[response(status = 403)]
    Forbidden(#[ref_response] AuthenticationForbidden),
    #[response(status = 409)]
    RequestInProgress(#[ref_response] IdempotencyRequestInProgress),
    #[response(status = 413)]
    RequestEntityTooLarge(#[ref_response] RequestEntityTooLarge),
    #[response(status = 422)]
    KeyMismatch(#[ref_response] IdempotencyKeyMismatch),
    #[response(status = 431)]
    Oversize(#[ref_response] AuthenticationOversize),
    #[response(status = 500)]
    InternalServerError(#[ref_response] InternalServerError),
    #[response(status = 503)]
    Unavailable(#[ref_response] IdempotencyUnavailable),
    #[response(status = 504)]
    Timeout(#[ref_response] AuthenticationTimeout),
}

/// The family's components, seeded into a router by
/// [`super::Composer::components`]: their only registration path.
#[derive(OpenApi)]
#[openapi(components(responses(
    IdempotencyBadRequest,
    IdempotencyRequestInProgress,
    IdempotencyKeyMismatch,
    IdempotencyUnavailable
)))]
pub(super) struct IdempotencyComponents;

#[cfg(test)]
mod tests {
    use serde_json::json;
    use utoipa::openapi::path::Parameter;

    use super::*;

    #[test]
    fn the_key_parameter_schema_is_pinned_to_the_expected_values() {
        let parameters: Vec<Parameter> = IdempotencyKey::into_params(|| None);
        let [parameter] = parameters.as_slice() else {
            panic!("one key parameter");
        };
        let parameter = serde_json::to_value(parameter).unwrap();
        assert_eq!(parameter["name"], KEY_HEADER);
        assert_eq!(parameter["in"], "header");
        assert_eq!(parameter["required"], true);
        assert_eq!(parameter["schema"]["type"], "string");
        assert_eq!(parameter["schema"]["minLength"], KEY_MIN_LENGTH);
        assert_eq!(parameter["schema"]["maxLength"], KEY_MAX_LENGTH);
        assert_eq!(parameter["schema"]["pattern"], KEY_PATTERN);
    }

    #[test]
    fn the_family_registers_exactly_the_four_named_components() {
        assert_eq!(
            <IdempotencyBadRequest as ToResponse>::response().0,
            BAD_REQUEST_COMPONENT
        );
        assert_eq!(
            <IdempotencyRequestInProgress as ToResponse>::response().0,
            REQUEST_IN_PROGRESS_COMPONENT
        );
        assert_eq!(
            <IdempotencyKeyMismatch as ToResponse>::response().0,
            KEY_MISMATCH_COMPONENT
        );
        assert_eq!(
            <IdempotencyUnavailable as ToResponse>::response().0,
            UNAVAILABLE_COMPONENT
        );
        let components = IdempotencyComponents::openapi().components.unwrap();
        let mut expected = RESPONSE_COMPONENTS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            components
                .responses
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected
        );
        assert!(components.schemas.is_empty());
    }

    #[test]
    fn only_the_retryable_components_declare_an_optional_retry_after() {
        let components = serde_json::to_value(IdempotencyComponents::openapi().components).unwrap();
        for (name, retryable) in [
            (BAD_REQUEST_COMPONENT, false),
            (REQUEST_IN_PROGRESS_COMPONENT, true),
            (KEY_MISMATCH_COMPONENT, false),
            (UNAVAILABLE_COMPONENT, true),
        ] {
            let response = &components["responses"][name];
            assert_eq!(
                response["content"]["application/problem+json"]["schema"]["$ref"],
                "#/components/schemas/Problem",
                "{name}"
            );
            assert!(
                response["description"]
                    .as_str()
                    .is_some_and(|description| !description.is_empty()),
                "{name}"
            );
            let retry_after = &response["headers"]["Retry-After"];
            assert_eq!(retry_after.is_object(), retryable, "{name}");
            assert!(retry_after.get("required").is_none(), "{name}");
        }
    }

    #[test]
    fn the_family_declares_the_section_five_table() {
        let reference = |name: &str| json!({ "$ref": format!("#/components/responses/{name}") });
        let responses =
            serde_json::to_value(IdempotentOperationProblemResponses::responses()).unwrap();
        assert_eq!(
            responses,
            json!({
                "400": reference(BAD_REQUEST_COMPONENT),
                "401": reference("AuthenticationUnauthorized"),
                "403": reference("AuthenticationForbidden"),
                "409": reference(REQUEST_IN_PROGRESS_COMPONENT),
                "413": reference("RequestEntityTooLarge"),
                "422": reference(KEY_MISMATCH_COMPONENT),
                "431": reference("AuthenticationOversize"),
                "500": reference("InternalServerError"),
                "503": reference(UNAVAILABLE_COMPONENT),
                "504": reference("AuthenticationTimeout"),
            })
        );
        for status in FIXED_PROBLEM_STATUSES.iter().chain([&AUTHORIZATION_STATUS]) {
            assert!(responses.get(*status).is_some(), "{status}");
        }
    }
}
