//! Value and encoding recipes: explicit formats instead of repeated string parsing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use proptest::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[test]
fn url_safe_base64_is_encoding_not_authentication() {
    assert_eq!(URL_SAFE_NO_PAD.encode([0xfb, 0xff]), "-_8");
    assert_eq!(URL_SAFE_NO_PAD.decode("-_8").unwrap(), [0xfb, 0xff]);
    assert!(URL_SAFE_NO_PAD.decode("%not-base64").is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn encoded_bytes_round_trip_without_padding(input in proptest::collection::vec(any::<u8>(), 0..512)) {
        let encoded = URL_SAFE_NO_PAD.encode(&input);
        prop_assert!(!encoded.contains('='));
        prop_assert!(encoded.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        prop_assert_eq!(URL_SAFE_NO_PAD.decode(&encoded).unwrap(), input);
    }
}

#[test]
fn semantic_versions_include_prerelease_rules() {
    let compatible = semver::VersionReq::parse("^1.2").unwrap();
    assert!(compatible.matches(&semver::Version::parse("1.8.0").unwrap()));
    assert!(!compatible.matches(&semver::Version::parse("2.0.0").unwrap()));
    assert!(!compatible.matches(&semver::Version::parse("1.3.0-alpha.1").unwrap()));
}

#[test]
fn cidr_membership_does_not_use_handwritten_masks() {
    let network: ipnet::IpNet = "10.20.0.0/16".parse().unwrap();
    assert!(network.contains(&"10.20.1.2".parse::<std::net::IpAddr>().unwrap()));
    assert!(!network.contains(&"10.21.1.2".parse::<std::net::IpAddr>().unwrap()));
    assert!("10.20.0.0/99".parse::<ipnet::IpNet>().is_err());
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RecordedValue {
    id: uuid::Uuid,
    #[serde(with = "rust_decimal::serde::str")]
    amount: Decimal,
    #[serde(with = "time::serde::rfc3339")]
    at: time::OffsetDateTime,
}

#[test]
fn value_types_round_trip_with_explicit_wire_formats() {
    let wire = json!({
        "id": "550e8400-e29b-41d4-a716-446655440000",
        "amount": "12.340",
        "at": "2026-09-19T00:00:00Z"
    });
    let value: RecordedValue = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(value.id.get_version_num(), 4);
    assert_eq!(value.amount, Decimal::new(1234, 2));
    assert_eq!(value.at, time::macros::datetime!(2026-09-19 00:00 UTC));
    assert_eq!(serde_json::to_value(&value).unwrap(), wire);
    let mut invalid = wire;
    invalid["id"] = json!("not-a-uuid");
    assert!(serde_json::from_value::<RecordedValue>(invalid).is_err());
    let generated = uuid::Uuid::new_v4();
    assert_eq!(generated.get_version_num(), 4);
}

#[test]
fn decimal_arithmetic_keeps_overflow_explicit() {
    let total = Decimal::new(10, 2).checked_add(Decimal::new(20, 2)).unwrap();
    assert_eq!(total, Decimal::new(30, 2));
    assert!(Decimal::MAX.checked_add(Decimal::ONE).is_none());
}
