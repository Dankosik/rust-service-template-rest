//! Contract proof over the assembled API document.
//!
//! The committed `api/openapi/service.yaml` must equal what the `openapi`
//! binary renders, every operation must have supported effective security and
//! the problem responses that policy requires, and the problem schemas
//! must stay closed. The tests walk the document as JSON, so they hold for
//! feature operations merged later without knowing their types.

// Integration tests are test code; the workspace's production lint levels
// for unwrap/expect/panic do not apply to them.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::process::Command;

use serde_json::{Value, json};

const COMMITTED: &str = include_str!("../../../api/openapi/service.yaml");

const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Problem responses a protected operation must declare: request validation,
/// both authentication outcomes, oversized credentials, and the two
/// unavailability answers.
const PROTECTED_PROBLEM_STATUSES: &[&str] = &["400", "401", "403", "431", "503", "504"];

fn document() -> Value {
    serde_json::to_value(service::api::document().expect("document composes"))
        .expect("document serializes")
}

/// `(method, path, operation)` for every operation in `document`.
fn operations(document: &Value) -> Vec<(String, String, &Value)> {
    let mut found = Vec::new();
    for (path, item) in document["paths"].as_object().expect("paths object") {
        for method in HTTP_METHODS {
            if let Some(operation) = item.get(*method) {
                found.push((method.to_uppercase(), path.clone(), operation));
            }
        }
    }
    found
}

/// The operation's security requirements, falling back to the document's.
fn effective_security<'a>(document: &'a Value, operation: &'a Value) -> &'a [Value] {
    operation
        .get("security")
        .or_else(|| document.get("security"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn is_public(document: &Value, operation: &Value) -> bool {
    effective_security(document, operation).is_empty()
}

/// Every alternative is exactly one `http`/`bearer` scheme without scopes.
/// An anonymous alternative, a scope, an unknown scheme, another scheme
/// type, or an AND-combination is not the wired bearer path.
fn uses_bearer_only(document: &Value, operation: &Value) -> bool {
    let requirements = effective_security(document, operation);
    if requirements.is_empty() {
        return false;
    }
    requirements.iter().all(|requirement| {
        let Some(requirement) = requirement.as_object() else {
            return false;
        };
        if requirement.len() != 1 {
            return false;
        }
        requirement.iter().all(|(name, scopes)| {
            let scheme = &document["components"]["securitySchemes"][name];
            scopes.as_array().is_some_and(Vec::is_empty)
                && scheme["type"]
                    .as_str()
                    .is_some_and(|t| t.eq_ignore_ascii_case("http"))
                && scheme["scheme"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        })
    })
}

/// The declared response for `status`, following a `$ref` into
/// `components/responses`.
fn response<'a>(document: &'a Value, operation: &'a Value, status: &str) -> Option<&'a Value> {
    let declared = operation["responses"].get(status)?;
    match declared["$ref"].as_str() {
        Some(reference) => {
            let name = reference.strip_prefix("#/components/responses/")?;
            document["components"]["responses"].get(name)
        }
        None => Some(declared),
    }
}

fn has_problem_response(document: &Value, operation: &Value, status: &str) -> bool {
    response(document, operation, status).is_some_and(|response| {
        response["content"]["application/problem+json"]["schema"]["$ref"]
            == "#/components/schemas/Problem"
    })
}

#[test]
fn committed_document_matches_the_generator() {
    let output = Command::new(env!("CARGO_BIN_EXE_openapi"))
        .output()
        .expect("run the openapi binary");
    assert!(
        output.status.success(),
        "openapi binary failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = String::from_utf8(output.stdout).expect("utf-8 document");
    if generated != COMMITTED {
        let first_difference = generated
            .lines()
            .zip(COMMITTED.lines())
            .position(|(generated, committed)| generated != committed)
            .map_or_else(
                || generated.lines().count().min(COMMITTED.lines().count()) + 1,
                |index| index + 1,
            );
        panic!(
            "api/openapi/service.yaml differs from the generated document at line \
             {first_difference}; run `make openapi-generate` and review the diff"
        );
    }
}

#[test]
fn document_has_the_probe_operations() {
    // Feature operations join the document beside the probes; the probes
    // themselves are platform behavior every derived service keeps.
    let document = document();
    let ids: Vec<_> = operations(&document)
        .iter()
        .map(|(_, _, operation)| operation["operationId"].as_str().unwrap().to_owned())
        .collect();
    for probe in ["healthLive", "healthReady"] {
        assert!(
            ids.contains(&probe.to_owned()),
            "missing {probe} in {ids:?}"
        );
    }
}

// template:begin inbound-webhooks:service-webhooks-contract-test
#[test]
fn webhook_ingress_is_public_to_bearer_auth_but_declares_signature_and_problem_contracts() {
    let document = document();
    let operation = &document["paths"]["/webhooks/{endpoint_id}"]["post"];
    assert_eq!(operation["operationId"], "receiveWebhook");
    let content = &operation["requestBody"]["content"]["*/*"];
    assert!(content.is_object(), "raw webhook content must be declared");
    assert!(
        content.get("schema").is_none(),
        "raw signed bytes must not acquire a JSON payload schema"
    );
    assert!(
        is_public(&document, operation),
        "webhook ingress must override root bearer security"
    );
    let parameters = operation["parameters"]
        .as_array()
        .expect("webhook parameters");
    for required in [
        "endpoint_id",
        "webhook-id",
        "webhook-timestamp",
        "webhook-signature",
    ] {
        assert!(
            parameters.iter().any(|parameter| {
                parameter["name"] == required && parameter["required"] == true
            }),
            "missing required {required}"
        );
    }
    for status in ["204", "400", "404", "409", "413", "500", "503"] {
        assert!(
            response(&document, operation, status).is_some(),
            "webhook ingress lacks {status}"
        );
    }
}
// template:end inbound-webhooks:service-webhooks-contract-test

#[test]
fn every_operation_has_supported_security_and_matching_optional_context() {
    let document = document();
    for (method, path, operation) in operations(&document) {
        let public = is_public(&document, operation);
        if !public {
            assert!(
                uses_bearer_only(&document, operation),
                "{method} {path} is protected but not every alternative is the bearer \
                 scheme without scopes"
            );
            for status in PROTECTED_PROBLEM_STATUSES {
                assert!(
                    has_problem_response(&document, operation, status),
                    "{method} {path} is protected but lacks a {status} \
                     application/problem+json response"
                );
            }
        }
        if let Some(decision) = operation.get("x-security-decision") {
            assert_eq!(
                decision["exposure"].as_str(),
                Some(if public { "public" } else { "protected" }),
                "{method} {path}: x-security-decision contradicts effective security"
            );
        }
    }
}

#[test]
fn problem_schemas_are_closed() {
    let document = document();
    for name in ["Problem", "InvalidParam"] {
        assert_eq!(
            document["components"]["schemas"][name]["additionalProperties"],
            json!(false),
            "components.schemas.{name} must set additionalProperties: false"
        );
    }
}

#[test]
fn probe_operations_declare_the_shared_problem_responses() {
    let document = document();
    for (method, path, operation) in operations(&document) {
        for status in ["400", "413", "500"] {
            assert!(
                has_problem_response(&document, operation, status),
                "{method} {path} lacks the {status} problem response"
            );
        }
    }
}

// template:begin authn:service-openapi-authn-contract
#[test]
fn retained_authentication_document_declares_one_bearer_default_and_public_probes() {
    let document = document();
    assert_eq!(
        document["components"]["securitySchemes"]["bearerAuth"]["type"],
        "http"
    );
    assert_eq!(
        document["components"]["securitySchemes"]["bearerAuth"]["scheme"],
        "bearer"
    );
    assert_eq!(document["security"], json!([{"bearerAuth": []}]));

    for operation_id in ["healthLive", "healthReady"] {
        let operation = operations(&document)
            .into_iter()
            .find(|(_, _, operation)| operation["operationId"] == operation_id)
            .expect("probe operation exists")
            .2;
        assert_eq!(operation["security"], json!([]));
        assert!(is_public(&document, operation));
    }
}
// template:end authn:service-openapi-authn-contract
// template:begin http-idempotency:service-openapi-http-idempotency-contract
/// A `Retry-After` header, in any letter case, that is not required.
fn declares_optional_retry_after(response: &Value) -> bool {
    response["headers"].as_object().is_some_and(|headers| {
        headers.iter().any(|(name, header)| {
            name.eq_ignore_ascii_case("Retry-After") && header["required"] != true
        })
    })
}

/// The family's components are in every retained document, referenced or
/// not, as Problem responses.
#[test]
fn retained_idempotency_document_declares_the_family_components() {
    let document = document();
    let responses = &document["components"]["responses"];
    for component in [
        "IdempotencyBadRequest",
        "IdempotencyRequestInProgress",
        "IdempotencyKeyMismatch",
        "IdempotencyUnavailable",
    ] {
        assert_eq!(
            responses[component]["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem",
            "components.responses.{component} is not a Problem response"
        );
    }
    for component in ["IdempotencyRequestInProgress", "IdempotencyUnavailable"] {
        assert!(
            declares_optional_retry_after(&responses[component]),
            "components.responses.{component} lacks an optional Retry-After"
        );
    }
}

/// Only `Composer::route` may declare `Idempotency-Key`: a hand-written
/// declaration on an uncomposed operation would promise a replay that no
/// boundary enforces.
#[test]
fn only_composed_operations_declare_the_idempotency_key() {
    use infra_http::idempotency::{Activation, Composer};

    let mut composer = Composer::inert();
    let contract = service::api::contract(&mut composer).expect("contract composes");
    let document = serde_json::to_value(contract.document()).expect("document serializes");
    let declared = operations(&document)
        .iter()
        .filter(|(_, _, operation)| declares_idempotency_key(operation))
        .count();
    let served = match composer.finish() {
        Activation::Inactive => 0,
        Activation::Active {
            operations: count, ..
        } => count.get(),
    };
    assert_eq!(declared, served);
}

/// An `Idempotency-Key` header parameter, in any letter case.
fn declares_idempotency_key(operation: &Value) -> bool {
    operation["parameters"]
        .as_array()
        .is_some_and(|parameters| {
            parameters.iter().any(|parameter| {
                parameter["in"] == "header"
                    && parameter["name"]
                        .as_str()
                        .is_some_and(|name| name.eq_ignore_ascii_case("Idempotency-Key"))
            })
        })
}

// template:end http-idempotency:service-openapi-http-idempotency-contract

/// The classifier fails closed on every alternative that is not exactly the
/// wired bearer scheme, so a protected operation cannot slip through with
/// an anonymous or unsupported path.
#[rstest::rstest]
#[case::inherited_bearer(&json!({}), false, true)]
#[case::explicit_bearer(&json!({"security": [{"bearerAuth": []}]}), false, true)]
#[case::explicit_public(&json!({"security": []}), true, false)]
#[case::anonymous_alternative(&json!({"security": [{"bearerAuth": []}, {}]}), false, false)]
#[case::unauthorized_scopes(&json!({"security": [{"bearerAuth": ["admin"]}]}), false, false)]
#[case::unknown_scheme(&json!({"security": [{"missingAuth": []}]}), false, false)]
#[case::unsupported_alternative(&json!({"security": [{"apiKeyAuth": []}]}), false, false)]
#[case::unsupported_and_requirement(
    &json!({"security": [{"bearerAuth": [], "apiKeyAuth": []}]}),
    false,
    false
)]
fn bearer_security_alternatives_fail_closed(
    #[case] operation: &Value,
    #[case] public: bool,
    #[case] protected: bool,
) {
    let document = json!({
        "components": {"securitySchemes": {
            "bearerAuth": {"type": "http", "scheme": "bearer"},
            "apiKeyAuth": {"type": "apiKey", "in": "header", "name": "X-API-Key"}
        }},
        "security": [{"bearerAuth": []}]
    });
    assert_eq!(is_public(&document, operation), public);
    assert_eq!(uses_bearer_only(&document, operation), protected);
}
