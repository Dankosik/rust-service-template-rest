use std::collections::BTreeMap;

use axum::http::HeaderValue;
use iri_string::types::UriStr;
use utoipa::openapi::header::{Header, HeaderBuilder};
use utoipa::openapi::path::{Parameter, ParameterIn};
use utoipa::openapi::response::Response;
use utoipa::openapi::schema::{ObjectBuilder, Type};
use utoipa::openapi::{Ref, RefOr, Required};
use utoipa::{OpenApi, ToResponse};

use super::identity::{KEY_GRAMMAR, MAX_DECODED_KEY_BYTES};
use super::{KEY_HEADER, REPLAYED_VALUE};
use crate::authn::{AuthenticationMalformed, AuthenticationUnavailable};
use crate::problem::{Code, Problem, http_status, responses};

const UNQUOTED_KEY_EXAMPLE: &str = "retry-key/with=visible-ascii";
const QUOTED_KEY_EXAMPLE: &str = "\"8e03978e-40d5-43e8-bc93-6894a57f9324\"";

pub(super) fn key_rules_link(
    docs: Option<&utoipa::openapi::external_docs::ExternalDocs>,
) -> Option<HeaderValue> {
    let uri = UriStr::new(&docs?.url).ok()?;
    let authority = uri.authority_components()?;
    if !(uri.scheme_str().eq_ignore_ascii_case("https")
        || uri.scheme_str().eq_ignore_ascii_case("http"))
        || authority.host().is_empty()
        || authority.userinfo().is_some()
    {
        return None;
    }
    HeaderValue::from_str(&format!("<{uri}>; rel=\"describedby\"")).ok()
}

pub(super) fn key_parameter() -> Parameter {
    Parameter::builder()
        .name(KEY_HEADER)
        .parameter_in(ParameterIn::Header)
        .description(Some(format!(
            "One key for one intended effect. Supply exactly one header field. {KEY_GRAMMAR} After decoding, the key must contain 1 to {MAX_DECODED_KEY_BYTES} bytes. After a timeout or lost response, retry as the same caller with the same decoded key, HTTP method, raw path and query string, and exact body bytes. Resend the original JSON byte for byte. Changes in whitespace or field order count as a different request. Keep the same Content-Type, declared request header and trailer values, including the order of repeated values for each field. The retry window is one minute from the first send unless the service published a longer window when you first sent the request. The service must check the key before the window ends. Allow for transit, request processing and database queueing. After expiry the same key can execute again. If there may be too little time left, check whether the original operation took effect before sending another request."
        )))
        .required(Required::True)
        .schema(Some(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .description(Some(format!(
                    "{KEY_GRAMMAR} The decoded value must contain 1 to {MAX_DECODED_KEY_BYTES} bytes. Quotes and escapes can make the wire value longer."
                )))
                .examples([QUOTED_KEY_EXAMPLE, UNQUOTED_KEY_EXAMPLE]),
        ))
        .build()
}

pub(super) fn replay_header() -> RefOr<Header> {
    HeaderBuilder::new()
        .schema(Some(
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some([REPLAYED_VALUE])),
        ))
        .description(Some(format!(
            "present with the string `{REPLAYED_VALUE}` only when this response replays a stored idempotent success"
        )))
        .build()
        .into()
}

pub(super) fn response_family() -> BTreeMap<String, RefOr<Response>> {
    [
        (Code::BadRequest, component::<IdempotencyBadRequest>()),
        (
            Code::RequestTimeout,
            component::<responses::RequestTimeout>(),
        ),
        (
            Code::IdempotencyRequestInProgress,
            component::<IdempotencyRequestInProgress>(),
        ),
        (
            Code::RequestEntityTooLarge,
            component::<responses::RequestEntityTooLarge>(),
        ),
        (
            Code::IdempotencyKeyMismatch,
            component::<IdempotencyKeyMismatch>(),
        ),
        (
            Code::InternalServerError,
            component::<responses::InternalServerError>(),
        ),
        (
            Code::IdempotencyUnavailable,
            component::<IdempotencyUnavailable>(),
        ),
    ]
    .into_iter()
    .map(|(code, response)| (http_status(code).as_str().to_owned(), response))
    .collect()
}

pub(super) fn can_replace_with_idempotency_response(
    status: &str,
    response: &RefOr<Response>,
) -> bool {
    if status == http_status(Code::AuthenticationMalformed).as_str() {
        response == &component::<AuthenticationMalformed>()
            || response == &component::<responses::BadRequest>()
    } else if status == http_status(Code::AuthenticationUnavailable).as_str() {
        response == &component::<AuthenticationUnavailable>()
    } else {
        false
    }
}

fn component<'r, T: ToResponse<'r>>() -> RefOr<Response> {
    Ref::from_response_name(T::response().0).into()
}

/// bearer authentication is malformed, the Idempotency-Key header is missing
/// or invalid, or the request is otherwise malformed or invalid
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(content_type = "application/problem+json", headers((
    "Link" = String, description = "Idempotency documentation when the key is missing or invalid"
)))]
pub(super) struct IdempotencyBadRequest(pub Problem);

/// A request with this Idempotency-Key is still in progress, or the operation
/// reports a conflict. For `idempotency_request_in_progress`, retry with the
/// same key after the Retry-After interval if still within the published retry window.
#[derive(Debug, ToResponse)]
#[expect(
    dead_code,
    reason = "The response body exists only for the OpenAPI derive"
)]
#[response(
    content_type = "application/problem+json",
    headers((
        "Retry-After" = u64,
        description = "seconds to wait before retrying an in-progress request with the same Idempotency-Key"
    ), (
        "Idempotent-Replayed" = String,
        description = "present with the value true only for a replayed terminal business conflict"
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
#[response(content_type = "application/problem+json", headers((
    "Link" = String, description = "Idempotency documentation when the key is bound to a different request"
), (
    "Idempotent-Replayed" = String,
    description = "present with the value true only for a replayed terminal business refusal"
)))]
pub(super) struct IdempotencyKeyMismatch(pub Problem);

/// bearer authentication trust or idempotent request processing is unavailable,
/// or the committed outcome is uncertain. Retry the same request with the same
/// Idempotency-Key within the published retry window.
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

#[derive(OpenApi)]
#[openapi(components(responses(
    responses::RequestTimeout,
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
            json!([QUOTED_KEY_EXAMPLE, UNQUOTED_KEY_EXAMPLE])
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
    fn the_rules_link_uses_the_published_operation_document_when_available() {
        let docs =
            utoipa::openapi::external_docs::ExternalDocs::new("https://docs.example/orders#retry");
        assert_eq!(
            key_rules_link(Some(&docs)).unwrap(),
            "<https://docs.example/orders#retry>; rel=\"describedby\""
        );
        for invalid in [
            "../docs",
            "https://docs.example/#two words",
            "https://docs.example/#bad%escape",
        ] {
            let docs = utoipa::openapi::external_docs::ExternalDocs::new(invalid);
            assert!(key_rules_link(Some(&docs)).is_none());
        }
    }

    #[test]
    fn generated_family_uses_registered_and_shared_problem_components() {
        let family = serde_json::to_value(response_family()).expect("family serializes");
        let reference = |name: &str| json!({ "$ref": format!("#/components/responses/{name}") });
        assert_eq!(
            family,
            json!({
                "400": reference("IdempotencyBadRequest"),
                "408": reference("RequestTimeout"),
                "409": reference("IdempotencyRequestInProgress"),
                "413": reference("RequestEntityTooLarge"),
                "422": reference("IdempotencyKeyMismatch"),
                "500": reference("InternalServerError"),
                "503": reference("IdempotencyUnavailable"),
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
                "RequestTimeout",
            ])
        );
    }
}
