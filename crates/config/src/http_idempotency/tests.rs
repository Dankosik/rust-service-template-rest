use super::*;
use crate::LoadOptions;
use crate::load::load_from;
use crate::test_support::{BUILD, env};

fn parse(toml: &str) -> Result<HttpIdempotencyConfig, toml::de::Error> {
    toml::from_str(toml)
}

#[test]
fn defaults_are_vacant() {
    let config = HttpIdempotencyConfig::default();
    assert_eq!(config.retention, None);
    config.validate().unwrap();
}

#[rstest::rstest]
#[case::seconds("90s", Duration::from_secs(90))]
#[case::minutes("5m", Duration::from_mins(5))]
#[case::hours_and_minutes("2h 30m", Duration::from_mins(2 * 60 + 30))]
#[case::days_no_space("7days", Duration::from_hours(7 * 24))]
#[case::padded_with_whitespace("  1h  ", Duration::from_secs(3600))]
fn accepted_forms_parse_to_the_named_duration(#[case] text: &str, #[case] expected: Duration) {
    let config = parse(&format!("retention = {text:?}")).unwrap();
    assert_eq!(config.retention, Some(expected));
}

#[test]
fn a_malformed_value_fails_to_deserialize() {
    assert!(parse("retention = \"not-a-duration\"").is_err());
}

#[rstest::rstest]
#[case::just_below_minimum("59s", false)]
#[case::minimum("1m", true)]
#[case::maximum("30days", true)]
#[case::just_above_maximum("30days 1s", false)]
fn retention_bounds_are_inclusive(#[case] text: &str, #[case] valid: bool) {
    let config = parse(&format!("retention = {text:?}")).unwrap();
    assert_eq!(config.validate().is_ok(), valid, "{text}");
}

#[rstest::rstest]
#[case::missing("")]
#[case::empty("retention = \"\"")]
#[case::whitespace("retention = \"   \"")]
fn vacancy_forms_are_none(#[case] toml: &str) {
    let config = parse(toml).unwrap();
    assert_eq!(config.retention, None);
    config.validate().unwrap();
}

#[test]
fn unknown_keys_in_the_section_fail() {
    assert!(parse("retention = \"1m\"\nbogus = \"x\"").is_err());
}

#[test]
fn required_retention_names_postgres_enabled_first() {
    let config = HttpIdempotencyConfig::default();
    let err = config.required_retention(false).unwrap_err();
    assert_eq!(err.key, "postgres.enabled");
}

#[test]
fn required_retention_names_the_retention_once_postgres_is_enabled() {
    let config = HttpIdempotencyConfig::default();
    let err = config.required_retention(true).unwrap_err();
    assert_eq!(err.key, "http_idempotency.retention");
}

#[test]
fn required_retention_succeeds_once_both_are_set() {
    let config = HttpIdempotencyConfig {
        retention: Some(Duration::from_secs(3600)),
    };
    assert_eq!(
        config.required_retention(true).unwrap(),
        Duration::from_secs(3600)
    );
}

#[test]
fn a_sub_microsecond_part_loads_unchanged() {
    let config = parse("retention = \"1m 1ns\"").unwrap();
    assert_eq!(config.retention, Some(Duration::new(60, 1)));
}

#[test]
fn the_retention_key_is_not_treated_as_secret_like() {
    assert!(!crate::secret_policy::is_secret_like_key(
        "http_idempotency.retention"
    ));
}

#[test]
fn http_idempotency_environment_sets_the_retention() {
    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[("APP__HTTP_IDEMPOTENCY__RETENTION", "2h")]),
    )
    .unwrap();
    assert_eq!(
        cfg.http_idempotency.retention,
        Some(Duration::from_hours(2))
    );
}
