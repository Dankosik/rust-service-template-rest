#![expect(
    clippy::expect_used,
    reason = "a broken fixture or setup fails the test"
)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "allow-*-in-tests suppresses these inside #[test] functions"
)]

#[expect(
    clippy::disallowed_types,
    reason = "OpenAPI drift test runs the compiled openapi generator binary directly"
)]
use std::process::Command;

use serde_json::{Value, json};

const COMMITTED: &str = include_str!("../../../api/openapi/service.yaml");

const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace", "query",
];

const PROTECTED_PROBLEM_STATUSES: &[&str] = &["400", "401", "403", "503", "504"];

fn document() -> Value {
    serde_json::to_value(service::api::document().expect("document composes"))
        .expect("document serializes")
}

struct Operation<'a> {
    method: String,
    path: &'a str,
    value: &'a Value,
}

fn operations(document: &Value) -> Vec<Operation<'_>> {
    let mut found = Vec::new();
    for (path, item) in document["paths"].as_object().expect("paths object") {
        for method in HTTP_METHODS {
            if let Some(operation) = item.get(*method) {
                found.push(Operation {
                    method: method.to_uppercase(),
                    path,
                    value: operation,
                });
            }
        }
        if let Some(additional) = item["additionalOperations"].as_object() {
            found.extend(additional.iter().map(|(method, operation)| Operation {
                method: method.clone(),
                path,
                value: operation,
            }));
        }
    }
    found
}

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
            name == "bearerAuth"
                && scopes
                    .as_array()
                    .is_some_and(|scopes| scopes.iter().all(Value::is_string))
                && scheme["type"]
                    .as_str()
                    .is_some_and(|t| t.eq_ignore_ascii_case("http"))
                && scheme["scheme"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        })
    })
}

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
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "OpenAPI drift test executes the compiled openapi generator binary synchronously"
)]
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
        let diff = similar::TextDiff::from_lines(COMMITTED, generated.as_str())
            .unified_diff()
            .context_radius(3)
            .header("api/openapi/service.yaml", "generated")
            .to_string();
        panic!(
            "api/openapi/service.yaml differs from the generated document; run \
             `make openapi-generate` and review the diff:\n{diff}"
        );
    }
}

#[test]
fn document_has_the_probe_operations() {
    let document = document();
    let ids: Vec<_> = operations(&document)
        .into_iter()
        .map(|operation| operation.value["operationId"].as_str().unwrap())
        .collect();
    for probe in ["healthLive", "healthReady"] {
        assert!(ids.contains(&probe), "missing {probe} in {ids:?}");
    }
}

#[test]
fn security_gate_checks_query_and_extension_methods() {
    let fixture = json!({"paths": {"/v": {
        "description": "versioned resource",
        "query": {"security": []},
        "additionalOperations": {"COPY": {"security": [{"otherBearer": []}]}}
    }}, "components":{"securitySchemes":{"otherBearer":{"type":"http","scheme":"bearer"}}}});
    let checked: Vec<_> = operations(&fixture)
        .into_iter()
        .map(|Operation { method, value, .. }| {
            (
                method,
                is_public(&fixture, value),
                uses_bearer_only(&fixture, value),
            )
        })
        .collect();
    assert_eq!(
        checked,
        [
            ("QUERY".to_owned(), true, false),
            ("COPY".to_owned(), false, false)
        ]
    );
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
    for status in ["204", "400", "404", "408", "413", "500", "503", "504"] {
        assert!(
            response(&document, operation, status).is_some(),
            "webhook ingress lacks {status}"
        );
    }
    let unavailable = response(&document, operation, "503").unwrap();
    assert!(declares_optional_retry_after(unavailable));
    let description = unavailable["description"].as_str().unwrap();
    assert!(
        description.contains("storage") && description.contains("capacity"),
        "{description}"
    );
    assert!(
        !operation["responses"]
            .as_object()
            .unwrap()
            .contains_key("401")
    );
    assert!(
        operation["responses"].get("409").is_none(),
        "authenticated webhook replays must not declare a conflict response"
    );
    assert!(
        operation["responses"].get("415").is_none(),
        "the signed binary webhook accepts arbitrary media types"
    );
}
// template:end inbound-webhooks:service-webhooks-contract-test

#[test]
fn every_operation_has_supported_security_and_matching_optional_context() {
    let document = document();
    for Operation {
        method,
        path,
        value: operation,
    } in operations(&document)
    {
        let public = is_public(&document, operation);
        if !public {
            assert!(
                uses_bearer_only(&document, operation),
                "{method} {path} requires each security alternative to name one HTTP \
                 bearer scheme with an array of string scopes"
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
fn operations_declare_the_shared_problem_responses_and_probes_bypass_admission() {
    let document = document();
    for Operation {
        method,
        path,
        value: operation,
    } in operations(&document)
    {
        for status in ["413", "500", "504"] {
            assert!(
                has_problem_response(&document, operation, status),
                "{method} {path} lacks the {status} problem response"
            );
        }
    }
    let ready = &document["paths"]["/health/ready"]["get"];
    let unavailable = response(&document, ready, "503").unwrap();
    assert!(unavailable["content"]["text/plain"].is_object());
    assert!(
        unavailable["content"]
            .get("application/problem+json")
            .is_none()
    );
    assert!(!declares_optional_retry_after(unavailable));
    assert!(
        document["paths"]["/health/live"]["get"]["responses"]
            .get("503")
            .is_none()
    );
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
            .find(|operation| operation.value["operationId"] == operation_id)
            .expect("probe operation exists")
            .value;
        assert_eq!(operation["security"], json!([]));
        assert!(is_public(&document, operation));
    }
}
// template:end authn:service-openapi-authn-contract
fn declares_optional_retry_after(response: &Value) -> bool {
    response["headers"].as_object().is_some_and(|headers| {
        headers.iter().any(|(name, header)| {
            name.eq_ignore_ascii_case("Retry-After") && header["required"] != true
        })
    })
}

// template:begin http-idempotency:service-openapi-http-idempotency-contract
#[test]
fn only_composed_operations_declare_the_idempotency_key() {
    let contract = service::api::contract().expect("contract composes");
    let served = contract.idempotent_operations();
    let document = serde_json::to_value(contract.into_openapi().expect("valid contract"))
        .expect("document serializes");
    let declared = operations(&document)
        .iter()
        .filter(|operation| declares_idempotency_key(operation.value))
        .count();
    assert_eq!(declared, served);
}

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

#[rstest::rstest]
#[case::inherited_bearer(&json!({}), false, true)]
#[case::explicit_bearer(&json!({"security": [{"bearerAuth": []}]}), false, true)]
#[case::explicit_public(&json!({"security": []}), true, false)]
#[case::anonymous_alternative(&json!({"security": [{"bearerAuth": []}, {}]}), false, false)]
#[case::scoped_bearer(&json!({"security": [{"bearerAuth": ["admin"]}]}), false, true)]
#[case::non_string_scope(&json!({"security": [{"bearerAuth": [1]}]}), false, false)]
#[case::unknown_scheme(&json!({"security": [{"missingAuth": []}]}), false, false)]
#[case::differently_named_bearer(&json!({"security": [{"otherBearer": []}]}), false, false)]
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
            "otherBearer": {"type": "http", "scheme": "bearer"},
            "apiKeyAuth": {"type": "apiKey", "in": "header", "name": "X-API-Key"}
        }},
        "security": [{"bearerAuth": []}]
    });
    assert_eq!(is_public(&document, operation), public);
    assert_eq!(uses_bearer_only(&document, operation), protected);
}
