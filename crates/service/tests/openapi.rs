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
// The idempotent declaration rules, walked independently of the seam that
// enforces them at composition and startup: every expected value below is
// this oracle's own literal.

/// Methods an idempotent operation may use.
const IDEMPOTENT_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

/// The key's header parameter name, compared case-insensitively.
const IDEMPOTENCY_KEY: &str = "Idempotency-Key";

/// The key parameter's exact schema pattern: one or more RFC 9110 `tchar`.
const IDEMPOTENCY_KEY_PATTERN: &str = "^[!#$%&'*+.^_`|~0-9A-Za-z-]+$";

/// The only headers a 2xx response of an idempotent operation may declare:
/// the ones a replay reproduces, compared case-insensitively.
const REPLAYABLE_HEADERS: [&str; 5] = [
    "Content-Type",
    "Content-Encoding",
    "Content-Language",
    "Content-Disposition",
    "Location",
];

/// The response component each of these statuses must be, exactly.
const IDEMPOTENT_PROBLEM_RESPONSES: [(&str, &str); 8] = [
    ("400", "IdempotencyBadRequest"),
    ("401", "AuthenticationUnauthorized"),
    ("409", "IdempotencyRequestInProgress"),
    ("422", "IdempotencyKeyMismatch"),
    ("431", "AuthenticationOversize"),
    ("500", "InternalServerError"),
    ("503", "IdempotencyUnavailable"),
    ("504", "AuthenticationTimeout"),
];

/// The response components the idempotent family adds to the document.
const IDEMPOTENCY_COMPONENTS: [&str; 4] = [
    "IdempotencyBadRequest",
    "IdempotencyRequestInProgress",
    "IdempotencyKeyMismatch",
    "IdempotencyUnavailable",
];

/// A declaration rule of an idempotent operation, in the specification's
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rule {
    /// 1: `x-idempotent` is the boolean `true`.
    Marker,
    /// 2: the method is POST, PUT, PATCH, or DELETE.
    Method,
    /// 3: a protected decision with a rationale, and explicit bearer-only
    /// security.
    Protected,
    /// 4: exactly one required `Idempotency-Key` header with the exact
    /// schema.
    Key,
    /// 5: a 2xx response, no 1xx or 3xx, and only replayable 2xx headers.
    Success,
    /// 6: the idempotent Problem responses, with an optional `Retry-After`
    /// at 409 and 503.
    Problems,
}

/// The first rule `operation`, served under `method`, breaks.
fn broken_rule(document: &Value, method: &str, operation: &Value) -> Option<Rule> {
    [
        (Rule::Marker, operation["x-idempotent"] == true),
        (Rule::Method, IDEMPOTENT_METHODS.contains(&method)),
        (
            Rule::Protected,
            is_explicitly_protected(document, operation),
        ),
        (Rule::Key, has_one_valid_key_parameter(operation)),
        (Rule::Success, has_replayable_successes(document, operation)),
        (
            Rule::Problems,
            has_idempotent_problem_responses(document, operation),
        ),
    ]
    .into_iter()
    .find_map(|(rule, holds)| (!holds).then_some(rule))
}

fn is_explicitly_protected(document: &Value, operation: &Value) -> bool {
    let decision = &operation["x-security-decision"];
    decision["exposure"] == "protected"
        && decision["rationale"]
            .as_str()
            .is_some_and(|rationale| !rationale.trim().is_empty())
        && operation.get("security").is_some()
        && uses_bearer_only(document, operation)
}

/// Header parameters named `Idempotency-Key` in any letter case.
fn key_parameters(operation: &Value) -> Vec<&Value> {
    operation["parameters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|parameter| {
            parameter["in"] == "header"
                && parameter["name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(IDEMPOTENCY_KEY))
        })
        .collect()
}

fn has_one_valid_key_parameter(operation: &Value) -> bool {
    let parameters = key_parameters(operation);
    let [parameter] = parameters.as_slice() else {
        return false;
    };
    let schema = &parameter["schema"];
    parameter["required"] == true
        && schema["type"] == "string"
        && schema["minLength"] == 1
        && schema["maxLength"] == 255
        && schema["pattern"] == IDEMPOTENCY_KEY_PATTERN
}

fn has_replayable_successes(document: &Value, operation: &Value) -> bool {
    let Some(responses) = operation["responses"].as_object() else {
        return false;
    };
    let mut has_success = false;
    for status in responses.keys() {
        if status.starts_with(['1', '3']) {
            return false;
        }
        if status.starts_with('2') {
            has_success = true;
            let replayable =
                response(document, operation, status).is_some_and(declares_only_replayable_headers);
            if !replayable {
                return false;
            }
        }
    }
    has_success
}

fn declares_only_replayable_headers(response: &Value) -> bool {
    response["headers"].as_object().is_none_or(|headers| {
        headers.keys().all(|name| {
            REPLAYABLE_HEADERS
                .iter()
                .any(|replayable| replayable.eq_ignore_ascii_case(name))
        })
    })
}

/// Every fixed status is exactly its family component, 403 is any Problem
/// response, and the 409 and 503 responses document an optional
/// `Retry-After`.
fn has_idempotent_problem_responses(document: &Value, operation: &Value) -> bool {
    IDEMPOTENT_PROBLEM_RESPONSES
        .iter()
        .all(|(status, component)| {
            operation["responses"][*status]["$ref"] == format!("#/components/responses/{component}")
                && has_problem_response(document, operation, status)
        })
        && has_problem_response(document, operation, "403")
        && ["409", "503"].into_iter().all(|status| {
            response(document, operation, status).is_some_and(declares_optional_retry_after)
        })
}

/// A `Retry-After` header, in any letter case, that is not required.
fn declares_optional_retry_after(response: &Value) -> bool {
    response["headers"].as_object().is_some_and(|headers| {
        headers.iter().any(|(name, header)| {
            name.eq_ignore_ascii_case("Retry-After") && header["required"] != true
        })
    })
}

/// Rules 1–6 for every operation that declares `x-idempotent`, and the
/// reverse rule for every other one: the contract never advertises a key
/// the service would ignore.
#[test]
fn idempotent_declarations_hold_in_both_directions() {
    let document = document();
    for (method, path, operation) in operations(&document) {
        if operation.get("x-idempotent").is_some() {
            assert_eq!(
                broken_rule(&document, &method, operation),
                None,
                "{method} {path} breaks an idempotent declaration rule"
            );
        } else {
            assert!(
                key_parameters(operation).is_empty(),
                "{method} {path} declares {IDEMPOTENCY_KEY} without x-idempotent: true"
            );
        }
    }
}

/// The family's components are in every retained document, referenced or
/// not, as Problem responses.
#[test]
fn retained_idempotency_document_declares_the_family_components() {
    let document = document();
    let responses = &document["components"]["responses"];
    for component in IDEMPOTENCY_COMPONENTS {
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

/// The composer serves exactly the operations the assembled document
/// declares idempotent: none, and so inactive, until one opts in.
#[test]
fn assembled_contract_agrees_with_its_idempotent_declarations() {
    use infra_http::idempotency::{Activation, Composer};

    let declared = operations(&document())
        .iter()
        .filter(|(_, _, operation)| operation.get("x-idempotent").is_some())
        .count();
    let mut composer = Composer::inert();
    let contract = service::api::contract(&mut composer);
    match composer.agree(contract.document()) {
        Ok(Activation::Inactive) => assert_eq!(declared, 0),
        Ok(Activation::Active {
            operations: served, ..
        }) => assert_eq!(served.get(), declared),
        Err(err) => panic!("the assembled contract disagrees: {err}"),
    }
}

/// Components that satisfy rule 6, and the bearer scheme rule 3 names.
fn idempotency_fixture() -> Value {
    let problem = json!({
        "description": "problem",
        "content": {"application/problem+json": {"schema": {"$ref": "#/components/schemas/Problem"}}}
    });
    let retryable = json!({
        "description": "retryable problem",
        "headers": {"Retry-After": {"schema": {"type": "integer"}}},
        "content": {"application/problem+json": {"schema": {"$ref": "#/components/schemas/Problem"}}}
    });
    json!({
        "components": {
            "securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}},
            "responses": {
                "IdempotencyBadRequest": problem,
                "AuthenticationUnauthorized": problem,
                "AuthenticationForbidden": problem,
                "IdempotencyRequestInProgress": retryable,
                "RequestEntityTooLarge": problem,
                "IdempotencyKeyMismatch": problem,
                "AuthenticationOversize": problem,
                "InternalServerError": problem,
                "IdempotencyUnavailable": retryable,
                "AuthenticationTimeout": problem
            }
        },
        "security": [{"bearerAuth": []}]
    })
}

/// An operation that meets every rule against `idempotency_fixture`.
fn valid_idempotent_operation() -> Value {
    let component = |name: &str| json!({"$ref": format!("#/components/responses/{name}")});
    json!({
        "operationId": "createWidget",
        "x-idempotent": true,
        "x-security-decision": {"exposure": "protected", "rationale": "creates one widget per key"},
        "security": [{"bearerAuth": []}],
        "parameters": [{
            "name": "Idempotency-Key",
            "in": "header",
            "required": true,
            "schema": {
                "type": "string",
                "minLength": 1,
                "maxLength": 255,
                "pattern": IDEMPOTENCY_KEY_PATTERN
            }
        }],
        "responses": {
            "201": {"description": "created", "headers": {"Location": {"schema": {"type": "string"}}}},
            "400": component("IdempotencyBadRequest"),
            "401": component("AuthenticationUnauthorized"),
            "403": component("AuthenticationForbidden"),
            "409": component("IdempotencyRequestInProgress"),
            "413": component("RequestEntityTooLarge"),
            "422": component("IdempotencyKeyMismatch"),
            "431": component("AuthenticationOversize"),
            "500": component("InternalServerError"),
            "503": component("IdempotencyUnavailable"),
            "504": component("AuthenticationTimeout")
        }
    })
}

fn remove(object: &mut Value, key: &str) {
    object.as_object_mut().expect("object").remove(key);
}

fn push(array: &mut Value, item: Value) {
    array.as_array_mut().expect("array").push(item);
}

/// Each case changes the valid operation in one way: the classifier accepts
/// the changes a rule allows and names the one rule a change breaks.
#[rstest::rstest]
#[case::post("POST", |_: &mut Value| {}, None)]
#[case::put("PUT", |_: &mut Value| {}, None)]
#[case::patch("PATCH", |_: &mut Value| {}, None)]
#[case::delete("DELETE", |_: &mut Value| {}, None)]
#[case::key_name_in_other_case(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["name"] = json!("idempotency-key"),
    None
)]
#[case::other_header_parameter(
    "POST",
    |operation: &mut Value| push(
        &mut operation["parameters"],
        json!({"name": "X-Widget-Tag", "in": "header", "schema": {"type": "string"}}),
    ),
    None
)]
#[case::replayable_header_in_other_case(
    "POST",
    |operation: &mut Value| operation["responses"]["201"]["headers"]["content-language"] =
        json!({"schema": {"type": "string"}}),
    None
)]
#[case::operation_owned_forbidden_problem(
    "POST",
    |operation: &mut Value| operation["responses"]["403"] = json!({
        "description": "the caller may not create widgets",
        "content": {"application/problem+json": {"schema": {"$ref": "#/components/schemas/Problem"}}}
    }),
    None
)]
#[case::marker_string(
    "POST",
    |operation: &mut Value| operation["x-idempotent"] = json!("true"),
    Some(Rule::Marker)
)]
#[case::marker_false(
    "POST",
    |operation: &mut Value| operation["x-idempotent"] = json!(false),
    Some(Rule::Marker)
)]
#[case::get("GET", |_: &mut Value| {}, Some(Rule::Method))]
#[case::head("HEAD", |_: &mut Value| {}, Some(Rule::Method))]
#[case::public_decision(
    "POST",
    |operation: &mut Value| operation["x-security-decision"]["exposure"] = json!("public"),
    Some(Rule::Protected)
)]
#[case::blank_rationale(
    "POST",
    |operation: &mut Value| operation["x-security-decision"]["rationale"] = json!(" "),
    Some(Rule::Protected)
)]
#[case::inherited_security(
    "POST",
    |operation: &mut Value| remove(operation, "security"),
    Some(Rule::Protected)
)]
#[case::anonymous_alternative(
    "POST",
    |operation: &mut Value| operation["security"] = json!([{"bearerAuth": []}, {}]),
    Some(Rule::Protected)
)]
#[case::missing_key(
    "POST",
    |operation: &mut Value| operation["parameters"] = json!([]),
    Some(Rule::Key)
)]
#[case::repeated_key(
    "POST",
    |operation: &mut Value| {
        let mut repeated = operation["parameters"][0].clone();
        repeated["name"] = json!("IDEMPOTENCY-KEY");
        push(&mut operation["parameters"], repeated);
    },
    Some(Rule::Key)
)]
#[case::key_in_query(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["in"] = json!("query"),
    Some(Rule::Key)
)]
#[case::optional_key(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["required"] = json!(false),
    Some(Rule::Key)
)]
#[case::nullable_key(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["schema"]["type"] =
        json!(["string", "null"]),
    Some(Rule::Key)
)]
#[case::empty_key_admitted(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["schema"]["minLength"] = json!(0),
    Some(Rule::Key)
)]
#[case::longer_key_admitted(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["schema"]["maxLength"] = json!(256),
    Some(Rule::Key)
)]
#[case::other_key_pattern(
    "POST",
    |operation: &mut Value| operation["parameters"][0]["schema"]["pattern"] =
        json!("^[A-Za-z0-9-]+$"),
    Some(Rule::Key)
)]
#[case::no_success(
    "POST",
    |operation: &mut Value| remove(&mut operation["responses"], "201"),
    Some(Rule::Success)
)]
#[case::informational(
    "POST",
    |operation: &mut Value| operation["responses"]["100"] = json!({"description": "continue"}),
    Some(Rule::Success)
)]
#[case::redirect(
    "POST",
    |operation: &mut Value| operation["responses"]["303"] = json!({"description": "see other"}),
    Some(Rule::Success)
)]
#[case::unreplayable_success_header(
    "POST",
    |operation: &mut Value| operation["responses"]["201"]["headers"]["ETag"] =
        json!({"schema": {"type": "string"}}),
    Some(Rule::Success)
)]
#[case::shared_bad_request(
    "POST",
    |operation: &mut Value| operation["responses"]["400"] =
        json!({"$ref": "#/components/responses/BadRequest"}),
    Some(Rule::Problems)
)]
#[case::inline_conflict(
    "POST",
    |operation: &mut Value| operation["responses"]["409"] = json!({
        "description": "conflict",
        "content": {"application/problem+json": {"schema": {"$ref": "#/components/schemas/Problem"}}}
    }),
    Some(Rule::Problems)
)]
#[case::missing_timeout(
    "POST",
    |operation: &mut Value| remove(&mut operation["responses"], "504"),
    Some(Rule::Problems)
)]
#[case::forbidden_without_problem(
    "POST",
    |operation: &mut Value| operation["responses"]["403"] = json!({"description": "forbidden"}),
    Some(Rule::Problems)
)]
fn idempotent_operation_rules(
    #[case] method: &str,
    #[case] change: fn(&mut Value),
    #[case] expected: Option<Rule>,
) {
    let mut operation = valid_idempotent_operation();
    change(&mut operation);
    assert_eq!(
        broken_rule(&idempotency_fixture(), method, &operation),
        expected
    );
}

/// Rule 6's `Retry-After` clause holds on the components the 409 and 503
/// responses reference.
#[rstest::rstest]
#[case::optional(|_: &mut Value| {}, None)]
#[case::name_in_other_case(
    |document: &mut Value| {
        let responses = &mut document["components"]["responses"];
        responses["IdempotencyUnavailable"]["headers"] =
            json!({"retry-after": {"schema": {"type": "integer"}}});
    },
    None
)]
#[case::required_on_conflict(
    |document: &mut Value| {
        let responses = &mut document["components"]["responses"];
        responses["IdempotencyRequestInProgress"]["headers"]["Retry-After"]["required"] =
            json!(true);
    },
    Some(Rule::Problems)
)]
#[case::missing_on_unavailable(
    |document: &mut Value| remove(
        &mut document["components"]["responses"]["IdempotencyUnavailable"]["headers"],
        "Retry-After",
    ),
    Some(Rule::Problems)
)]
fn idempotent_retry_after_rule(#[case] change: fn(&mut Value), #[case] expected: Option<Rule>) {
    let mut document = idempotency_fixture();
    change(&mut document);
    assert_eq!(
        broken_rule(&document, "POST", &valid_idempotent_operation()),
        expected
    );
}

/// The reverse rule's classifier finds the key among header parameters
/// only, in any letter case.
#[rstest::rstest]
#[case::no_parameters(&json!([]), false)]
#[case::key_header(&json!([{"name": "Idempotency-Key", "in": "header"}]), true)]
#[case::key_header_in_other_case(&json!([{"name": "idempotency-key", "in": "header"}]), true)]
#[case::key_in_query(&json!([{"name": "Idempotency-Key", "in": "query"}]), false)]
#[case::other_header(&json!([{"name": "X-Request-ID", "in": "header"}]), false)]
fn reverse_rule_finds_key_header_parameters(#[case] parameters: &Value, #[case] declared: bool) {
    let operation = json!({"parameters": parameters});
    assert_eq!(!key_parameters(&operation).is_empty(), declared);
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
