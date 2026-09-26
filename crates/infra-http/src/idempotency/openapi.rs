//! The idempotency contract that [`super::Composer::route`] generates.
//!
//! An operation supplies its normal protected metadata and business responses.
//! The composer adds the idempotency extension, key parameter, and response
//! family to the served tuple before it is protected or merged into a document.

use std::collections::BTreeMap;

use utoipa::openapi::path::{Parameter, ParameterIn};
use utoipa::openapi::response::Response;
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::{Ref, RefOr, Required};
use utoipa::{OpenApi, ToResponse};

use crate::problem::Problem;

/// The key's header parameter name.
pub(super) const KEY_HEADER: &str = "Idempotency-Key";

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

const KEY_DESCRIPTION: &str = "One key for one intended effect. Supply exactly one header field. An unquoted value is visible ASCII; a value beginning with `\"` is an IETF Structured Field string. After decoding, the key must contain 1 to 255 bytes. Retry the same request with the same decoded key.";
const KEY_SCHEMA_DESCRIPTION: &str = "A visible-ASCII value or an IETF Structured Field string. The decoded value must contain 1 to 255 bytes; quoted values can have a longer wire representation because of quotes and escapes.";
const UNQUOTED_KEY_EXAMPLE: &str = "retry-key/with=visible-ascii";
const QUOTED_KEY_EXAMPLE: &str = "\"retry key with spaces\"";
const AUTHENTICATION_MALFORMED_COMPONENT: &str = "AuthenticationMalformed";
const BAD_REQUEST_PROBLEM_COMPONENT: &str = "BadRequest";
const AUTHENTICATION_UNAVAILABLE_COMPONENT: &str = "AuthenticationUnavailable";
const REQUEST_ENTITY_TOO_LARGE_COMPONENT: &str = "RequestEntityTooLarge";
const INTERNAL_SERVER_ERROR_COMPONENT: &str = "InternalServerError";

/// Construct the one generated `Idempotency-Key` parameter.
pub(super) fn key_parameter() -> Parameter {
    Parameter::builder()
        .name(KEY_HEADER)
        .parameter_in(ParameterIn::Header)
        .description(Some(KEY_DESCRIPTION))
        .required(Required::True)
        .schema(Some(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .description(Some(KEY_SCHEMA_DESCRIPTION))
                .examples([UNQUOTED_KEY_EXAMPLE, QUOTED_KEY_EXAMPLE]),
        ))
        .build()
}

/// The generated responses that supplement an operation's own success and
/// protected-operation declarations.
pub(super) fn response_family() -> BTreeMap<String, RefOr<Response>> {
    [
        ("400", BAD_REQUEST_COMPONENT),
        ("409", REQUEST_IN_PROGRESS_COMPONENT),
        ("413", REQUEST_ENTITY_TOO_LARGE_COMPONENT),
        ("422", KEY_MISMATCH_COMPONENT),
        ("500", INTERNAL_SERVER_ERROR_COMPONENT),
        ("503", UNAVAILABLE_COMPONENT),
    ]
    .into_iter()
    .map(|(status, component)| (status.to_owned(), Ref::from_response_name(component).into()))
    .collect()
}

/// Whether the normal protected-operation declaration in this slot can be
/// replaced by the richer generated idempotency response.
pub(super) fn replaces_protected_response(status: &str, response: &RefOr<Response>) -> bool {
    match status {
        "400" => {
            response == &response_reference(AUTHENTICATION_MALFORMED_COMPONENT)
                || response == &response_reference(BAD_REQUEST_PROBLEM_COMPONENT)
        }
        "503" => response == &response_reference(AUTHENTICATION_UNAVAILABLE_COMPONENT),
        _ => false,
    }
}

fn response_reference(component: &str) -> RefOr<Response> {
    Ref::from_response_name(component).into()
}

/// bearer authentication is malformed, the Idempotency-Key header is missing
/// or invalid, or the request is otherwise malformed or invalid
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(content_type = "application/problem+json")]
pub(super) struct IdempotencyBadRequest(pub Problem);

/// a request with this Idempotency-Key is still in progress, or the operation
/// reports a conflict; retry with the same key after the Retry-After interval
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(
    content_type = "application/problem+json",
    headers((
        "Retry-After" = u64,
        description = "seconds to wait before retrying with the same Idempotency-Key"
    ))
)]
pub(super) struct IdempotencyRequestInProgress(pub Problem);

/// the Idempotency-Key is bound to a different request, or the operation
/// cannot process the request content
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(content_type = "application/problem+json")]
pub(super) struct IdempotencyKeyMismatch(pub Problem);

/// bearer authentication trust or idempotent request processing is unavailable,
/// or the committed outcome is uncertain; retry with the same Idempotency-Key
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(
    content_type = "application/problem+json",
    headers((
        "Retry-After" = u64,
        description = "seconds to wait before retrying with the same Idempotency-Key"
    ))
)]
pub(super) struct IdempotencyUnavailable(pub Problem);

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

    use super::*;

    #[test]
    fn generated_key_documents_both_encodings_and_decoded_byte_limit() {
        let parameter = serde_json::to_value(key_parameter()).expect("parameter serializes");
        assert_eq!(parameter["name"], KEY_HEADER);
        assert_eq!(parameter["in"], "header");
        assert_eq!(parameter["required"], true);
        assert_eq!(parameter["schema"]["type"], "string");
        assert_eq!(
            parameter["schema"]["examples"],
            json!([UNQUOTED_KEY_EXAMPLE, QUOTED_KEY_EXAMPLE])
        );
        assert!(
            parameter["description"]
                .as_str()
                .is_some_and(|text| text.contains("decoded key"))
        );
        assert!(
            parameter["schema"]["description"]
                .as_str()
                .is_some_and(|text| text.contains("decoded value"))
        );
        assert!(parameter["schema"].get("minLength").is_none());
        assert!(parameter["schema"].get("maxLength").is_none());
        assert!(parameter["schema"].get("pattern").is_none());
    }

    #[test]
    fn generated_family_uses_registered_and_shared_problem_components() {
        let family = serde_json::to_value(response_family()).expect("family serializes");
        let reference = |name: &str| json!({ "$ref": format!("#/components/responses/{name}") });
        assert_eq!(
            family,
            json!({
                "400": reference(BAD_REQUEST_COMPONENT),
                "409": reference(REQUEST_IN_PROGRESS_COMPONENT),
                "413": reference(REQUEST_ENTITY_TOO_LARGE_COMPONENT),
                "422": reference(KEY_MISMATCH_COMPONENT),
                "500": reference(INTERNAL_SERVER_ERROR_COMPONENT),
                "503": reference(UNAVAILABLE_COMPONENT),
            })
        );

        let components = IdempotencyComponents::openapi()
            .components
            .expect("idempotency response components");
        assert_eq!(
            components
                .responses
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "IdempotencyBadRequest",
                "IdempotencyKeyMismatch",
                "IdempotencyRequestInProgress",
                "IdempotencyUnavailable",
            ])
        );
    }
}
