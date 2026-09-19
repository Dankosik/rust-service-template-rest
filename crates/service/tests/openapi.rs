//! Contract proof over the assembled API document.
//!
//! The committed `api/openapi/service.yaml` must equal what the `openapi`
//! binary renders, every operation must declare its security decision and
//! the problem responses that decision requires, and the problem schemas
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
    serde_json::to_value(service::api::document()).expect("document serializes")
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

#[test]
fn every_operation_declares_its_security_decision() {
    let document = document();
    for (method, path, operation) in operations(&document) {
        let decision = &operation["x-security-decision"];
        let exposure = decision["exposure"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default();
        let rationale = decision["rationale"]
            .as_str()
            .map(str::trim)
            .unwrap_or_default();
        assert!(
            !exposure.is_empty() && !rationale.is_empty(),
            "{method} {path}: x-security-decision needs exposure and rationale"
        );
        match exposure {
            "public" => assert!(
                is_public(&document, operation),
                "{method} {path} inherits or declares security while marked public"
            ),
            "protected" => {
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
            "blocked" => {}
            other => {
                panic!("{method} {path}: exposure {other:?} is not public, protected, or blocked")
            }
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
