//! External data recipes. Schema and business policy still belong to the consuming feature.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::time::Duration;

use infra_http::problem::{Code, Problem};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_with::{DisplayFromStr, DurationMilliSeconds, MapPreventDuplicates, serde_as};
use validator::Validate;

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderPayload {
    #[serde_as(as = "Vec<DisplayFromStr>")]
    ids: Vec<u64>,
    #[serde_as(as = "DurationMilliSeconds<u64>")]
    timeout: Duration,
}

#[test]
fn nested_adapters_replace_a_post_deserialization_conversion_pass() {
    let value: ProviderPayload = serde_json::from_value(json!({
        "ids": ["12", "34"], "timeout": 1500
    }))
    .unwrap();
    assert_eq!(value.ids, [12, 34]);
    assert_eq!(value.timeout, Duration::from_millis(1500));
    assert!(
        serde_json::from_value::<ProviderPayload>(json!({
            "ids": ["not-a-number"], "timeout": 1500
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ProviderPayload>(json!({
            "ids": [], "timeout": 1500, "unexpected": true
        }))
        .is_err()
    );
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct UpdateName {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_with::rust::double_option"
    )]
    name: Option<Option<String>>,
}

#[test]
fn patch_fields_keep_missing_null_and_value_distinct() {
    for (wire, expected) in [
        (json!({}), None),
        (json!({"name": null}), Some(None)),
        (json!({"name": "Ada"}), Some(Some("Ada".to_owned()))),
    ] {
        let update: UpdateName = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(update.name, expected);
        assert_eq!(serde_json::to_value(update).unwrap(), wire);
    }
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct UniqueLimits {
    #[serde_as(as = "MapPreventDuplicates<_, _>")]
    limits: BTreeMap<String, u32>,
}

#[test]
fn maps_can_reject_duplicate_input_instead_of_silently_overwriting_it() {
    let valid: UniqueLimits = serde_json::from_str(r#"{"limits":{"batch":10}}"#).unwrap();
    assert_eq!(valid.limits["batch"], 10);
    assert!(serde_json::from_str::<UniqueLimits>(r#"{"limits":{"batch":10,"batch":20}}"#).is_err());
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct NameRequest {
    #[validate(length(min = 1, max = 4))]
    name: String,
}

// Intentionally specific to this DTO. Do not stringify ValidationErrors or
// its params: those can contain the submitted value. A real operation also
// supplies request_id, field paths and matching OpenAPI constraints.
fn validate_name(input: &NameRequest) -> Result<(), Box<Problem>> {
    input.validate().map_err(|_| {
        Box::new(
            Problem::new(Code::UnprocessableContent)
                .invalid_param("/name", "length must be between 1 and 4 characters"),
        )
    })
}

#[test]
fn validation_checks_boundaries_without_echoing_submitted_values() {
    assert!(validate_name(&NameRequest { name: "a".into() }).is_ok());
    assert!(
        validate_name(&NameRequest {
            name: "abcd".into()
        })
        .is_ok()
    );
    assert!(
        validate_name(&NameRequest {
            name: String::new()
        })
        .is_err()
    );
    let submitted = "DO_NOT_ECHO_THIS_VALUE";
    let problem = validate_name(&NameRequest {
        name: submitted.into(),
    })
    .unwrap_err();
    let wire = serde_json::to_value(problem).unwrap();
    assert_eq!(wire["code"], "unprocessable_content");
    assert_eq!(wire["invalid_params"][0]["name"], "/name");
    assert!(!wire.to_string().contains(submitted));
}

#[test]
fn merge_patch_uses_null_to_remove_and_absence_to_preserve() {
    let mut document = json!({"name": "old", "keep": true, "remove": 1});
    json_patch::merge(&mut document, &json!({"name": "new", "remove": null}));
    assert_eq!(document, json!({"name": "new", "keep": true}));
}

#[test]
fn fallible_json_patch_is_applied_to_a_candidate_before_publication() {
    let original = json!({"name": "old"});
    let patch: json_patch::Patch = serde_json::from_value(json!([
        {"op": "replace", "path": "/name", "value": "new"},
        {"op": "remove", "path": "/missing"}
    ]))
    .unwrap();
    let mut candidate = original.clone();
    assert!(json_patch::patch(&mut candidate, &patch).is_err());
    // Never publish a partially changed candidate after a failed patch.
    assert_eq!(original, json!({"name": "old"}));
}

#[test]
fn a_small_problem_snapshot_is_not_a_second_openapi_authority() {
    let problem = serde_json::to_value(Problem::new(Code::BadRequest)).unwrap();
    assert_eq!(problem["status"], 400);
    insta::assert_json_snapshot!(problem, @r###"
    {
      "code": "bad_request",
      "status": 400,
      "title": "bad request",
      "type": "https://www.rfc-editor.org/rfc/rfc9110#section-15.5.1"
    }
    "###);
}
