use std::fmt;

use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, ParsedPublicKey};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use foldhash::HashSet;
use indexmap::{IndexMap, map::Entry};
use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse,
    ThumbprintHash,
};
use serde::{Deserialize, Deserializer, de};

use crate::{InvalidKeyId, InvalidServiceName, KeyId, ServiceName};

type CallerKeys = IndexMap<ServiceName, Vec<TrustedKey>, foldhash::quality::RandomState>;

/// RFC 7517 §5
pub struct TrustRegistry {
    callers: CallerKeys,
}

pub(crate) struct TrustedKey {
    pub(crate) kid: KeyId,
    pub(crate) public_key: ParsedPublicKey,
    thumbprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{place}{reason}")]
pub struct TrustError {
    place: Place,
    reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, derive_more::Display)]
enum Place {
    #[display("")]
    Document,
    #[display("line {line}, column {column}: ")]
    Json { line: usize, column: usize },
    #[display("caller {_0}: ")]
    Caller(ServiceName),
    #[display("caller {_0} key {_1}: ")]
    Key(ServiceName, usize),
}

impl TrustError {
    const fn new(place: Place, reason: &'static str) -> Self {
        Self { place, reason }
    }

    const fn document(reason: &'static str) -> Self {
        Self::new(Place::Document, reason)
    }
}

impl TrustRegistry {
    pub fn parse(document: &str) -> Result<Self, TrustError> {
        let mut callers = Self::parse_caller_keys(document)?;
        let mut seen = HashSet::default();
        for (caller, keys) in &callers {
            for (index, key) in keys.iter().enumerate() {
                if !seen.insert(key.thumbprint.as_str()) {
                    return Err(TrustError::new(
                        Place::Key(caller.clone(), index + 1),
                        "repeats public key material in the trust registry",
                    ));
                }
            }
        }
        callers.sort_keys();
        Ok(Self { callers })
    }

    fn parse_caller_keys(document: &str) -> Result<CallerKeys, TrustError> {
        let entries: Entries = serde_json::from_str(document).map_err(|error| {
            let line = error.line();
            let column = error.column();
            if (error.is_syntax() || error.is_eof()) && line > 0 {
                TrustError::new(
                    Place::Json { line, column },
                    "invalid JSON in the trust registry",
                )
            } else {
                TrustError::document("the trust registry is not one JSON object of JWK Sets")
            }
        })?;
        let entries = match entries {
            Entries::Unique(entries) => entries,
            Entries::RepeatedCaller(repeated) => {
                return Err(match ServiceName::parse(&repeated) {
                    Ok(caller) => {
                        TrustError::new(Place::Caller(caller), "is repeated in the trust registry")
                    }
                    Err(_) => TrustError::document("the trust registry repeats a caller"),
                });
            }
        };
        if entries.is_empty() {
            return Err(TrustError::document("the trust registry names no caller"));
        }
        let mut callers = IndexMap::with_capacity_and_hasher(
            entries.len(),
            foldhash::quality::RandomState::default(),
        );
        for (name, set) in entries {
            let (caller, keys) = parse_caller(&name, set)?;
            callers.insert(caller, keys);
        }
        Ok(callers)
    }

    pub fn check_key_withdrawal(
        &self,
        previous_document: &str,
        old_caller: &str,
        old_kid: Option<&str>,
    ) -> Result<(), TrustError> {
        let previous = Self::parse_caller_keys(previous_document)?;
        let caller_keys = previous.get(old_caller).ok_or_else(|| {
            TrustError::document("selected caller is absent from the previous registry")
        })?;
        let withdrawn: HashSet<&str> = caller_keys
            .iter()
            .filter(|key| old_kid.is_none_or(|kid| key.kid.as_str() == kid))
            .map(|key| key.thumbprint.as_str())
            .collect();
        if withdrawn.is_empty() {
            return Err(TrustError::document(
                "selected kid is absent from the previous caller",
            ));
        }
        if self
            .callers
            .values()
            .flatten()
            .any(|key| withdrawn.contains(key.thumbprint.as_str()))
        {
            return Err(TrustError::document(
                "selected public key material remains in the candidate registry",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn caller_count(&self) -> usize {
        self.callers.len()
    }

    pub(crate) fn caller(&self, name: &str) -> Option<(&ServiceName, &[TrustedKey])> {
        self.callers
            .get_key_value(name)
            .map(|(name, keys)| (name, keys.as_slice()))
    }
}

impl fmt::Debug for TrustRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustRegistry")
            .field(
                "callers",
                &fmt::from_fn(|f| f.debug_list().entries(self.callers.keys()).finish()),
            )
            .finish()
    }
}

fn drain_map<'de, A: de::MapAccess<'de>>(map: &mut A) -> Result<(), A::Error> {
    while map
        .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
        .is_some()
    {}
    Ok(())
}

fn drain_seq<'de, A: de::SeqAccess<'de>>(seq: &mut A) -> Result<(), A::Error> {
    while seq.next_element::<de::IgnoredAny>()?.is_some() {}
    Ok(())
}

enum Entries {
    Unique(IndexMap<String, DuplicateCheckedJson, foldhash::quality::RandomState>),
    RepeatedCaller(String),
}

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Entries;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of JWK Sets")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = IndexMap::default();
                while let Some((name, value)) = map.next_entry::<String, DuplicateCheckedJson>()? {
                    match entries.entry(name) {
                        Entry::Vacant(entry) => {
                            entry.insert(value);
                        }
                        Entry::Occupied(entry) => {
                            let (repeated, _) = entry.swap_remove_entry();
                            drain_map(&mut map)?;
                            return Ok(Entries::RepeatedCaller(repeated));
                        }
                    }
                }
                Ok(Entries::Unique(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

enum DuplicateCheckedJson {
    Value(serde_json::Value),
    DuplicateMember,
}

impl<'de> Deserialize<'de> for DuplicateCheckedJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = DuplicateCheckedJson;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value")
            }
            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson::Value(value.into()))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson::Value(value.into()))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson::Value(value.into()))
            }
            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson::Value(value.into()))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson::Value(value.into()))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson::Value(serde_json::Value::Null))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(child) = seq.next_element()? {
                    let DuplicateCheckedJson::Value(value) = child else {
                        drain_seq(&mut seq)?;
                        return Ok(DuplicateCheckedJson::DuplicateMember);
                    };
                    items.push(value);
                }
                Ok(DuplicateCheckedJson::Value(serde_json::Value::Array(items)))
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut members = serde_json::Map::new();
                while let Some((name, child)) = map.next_entry()? {
                    if let DuplicateCheckedJson::Value(value) = child
                        && members.insert(name, value).is_none()
                    {
                        continue;
                    }
                    drain_map(&mut map)?;
                    return Ok(DuplicateCheckedJson::DuplicateMember);
                }
                Ok(DuplicateCheckedJson::Value(serde_json::Value::Object(
                    members,
                )))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn parse_caller(
    name: &str,
    set: DuplicateCheckedJson,
) -> Result<(ServiceName, Vec<TrustedKey>), TrustError> {
    let caller =
        ServiceName::parse(name).map_err(|_| TrustError::document(InvalidServiceName::MESSAGE))?;
    let caller_error = |reason| TrustError::new(Place::Caller(caller.clone()), reason);
    let DuplicateCheckedJson::Value(mut set) = set else {
        return Err(caller_error("repeats a member name within its JWK Set"));
    };
    // RFC 7517 §5
    let Some(serde_json::Value::Array(entries)) =
        set.as_object_mut().and_then(|set| set.remove("keys"))
    else {
        return Err(caller_error("is not a JWK Set with a keys array"));
    };
    if entries.is_empty() {
        return Err(caller_error("has no key"));
    }
    let mut keys: Vec<TrustedKey> = Vec::with_capacity(entries.len());
    for (index, value) in entries.into_iter().enumerate() {
        let key_error = |reason| TrustError::new(Place::Key(caller.clone(), index + 1), reason);
        let key = admit_key(value).map_err(key_error)?;
        if keys.iter().any(|known| known.kid == key.kid) {
            return Err(key_error("repeats a kid of this caller"));
        }
        keys.push(key);
    }
    Ok((caller, keys))
}

fn admit_key(value: serde_json::Value) -> Result<TrustedKey, &'static str> {
    let object = value.as_object().ok_or("is not a JSON object")?;
    if object.contains_key("d") || object.contains_key("k") {
        return Err("contains private key material");
    }
    let jwk: Jwk = serde_json::from_value(value).map_err(|_| "is not a valid JWK")?;
    let AlgorithmParameters::EllipticCurve(parameters) = &jwk.algorithm else {
        return Err("is not an EC P-256 key");
    };
    if parameters.curve != EllipticCurve::P256 {
        return Err("is not an EC P-256 key");
    }
    let common = &jwk.common;
    if common
        .key_algorithm
        .is_some_and(|algorithm| algorithm != KeyAlgorithm::ES256)
    {
        return Err("names an algorithm other than ES256");
    }
    if common
        .public_key_use
        .as_ref()
        .is_some_and(|key_use| *key_use != PublicKeyUse::Signature)
    {
        return Err("is not a signature key");
    }
    if let Some(operations) = &common.key_operations {
        if operations.is_empty() {
            return Err("key_ops is empty");
        }
        if operations
            .iter()
            .any(|operation| *operation != KeyOperations::Verify)
        {
            return Err("allows operations other than verify");
        }
    }
    let kid = common.key_id.as_deref().ok_or("has no kid")?;
    let kid = KeyId::parse(kid).map_err(|_| InvalidKeyId::MESSAGE)?;
    let coordinate = |encoded: &str| URL_SAFE_NO_PAD.decode(encoded).ok();
    let (Some(x), Some(y)) = (coordinate(&parameters.x), coordinate(&parameters.y)) else {
        return Err("has malformed coordinates");
    };
    let point = shared_rules::rfc7518::p256_point(&x, &y).ok_or("has malformed coordinates")?;
    let public_key = ParsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point)
        .map_err(|_| "is not a point on P-256")?;
    let thumbprint = jwk
        .thumbprint(ThumbprintHash::SHA256)
        .map_err(|_| "cannot compute the public key thumbprint")?;
    Ok(TrustedKey {
        kid,
        public_key,
        thumbprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestKey;

    #[test]
    fn registry_debug_discloses_the_caller_but_hides_key_rotation() {
        let original = TestKey::new("orders/1").jwk();
        let replacement = TestKey::new("orders/2").jwk();
        let registry = |keys: &[serde_json::Value]| {
            TrustRegistry::parse(&serde_json::json!({"orders": {"keys": keys}}).to_string())
                .unwrap()
        };
        let single = registry(std::slice::from_ref(&original));
        let rotating = registry(&[original, replacement]);
        for (before, after) in [
            (format!("{single:?}"), format!("{rotating:?}")),
            (format!("{single:#?}"), format!("{rotating:#?}")),
        ] {
            assert!(before.contains("\"orders\""));
            assert_eq!(before, after);
        }
    }

    #[test]
    fn admits_each_callers_keys_under_its_own_name() {
        let orders = TestKey::new("orders/production/2026-10");
        let rotated = TestKey::new("orders/production/2027-01");
        let billing = TestKey::new(orders.kid());
        let document = serde_json::json!({
            "orders": {"keys": [orders.jwk(), rotated.jwk()], "note": "ignored"},
            "billing": {"keys": [billing.jwk()]},
        })
        .to_string();
        let registry = TrustRegistry::parse(&document).unwrap();
        assert!(registry.caller("catalog").is_none());
        assert!(registry.caller("Orders").is_none());
        let verifier = crate::Verifier::new(
            crate::ServiceName::parse("catalog").unwrap(),
            crate::JwtType::parse("example-svc+jwt").unwrap(),
            registry,
        );
        for (key, issuer) in [
            (&orders, "orders"),
            (&rotated, "orders"),
            (&billing, "billing"),
        ] {
            let token = key.sign(
                &serde_json::json!({"alg": "ES256", "typ": "example-svc+jwt", "kid": key.kid()}),
                &serde_json::json!({"iss": issuer, "sub": issuer, "aud": "catalog", "iat": 1_000, "exp": 1_060}),
            );
            let caller = verifier.verify(token.as_bytes(), || Ok(1_000)).unwrap();
            assert_eq!(caller.name().as_str(), issuer, "{}", key.kid());
        }
    }

    #[test]
    fn withdrawal_checks_historical_material_across_the_entire_candidate() {
        let retired = TestKey::new("old").jwk();
        let retained = TestKey::new("standby").jwk();
        let mut alias = retired.clone();
        alias["kid"] = "renamed".into();
        alias.as_object_mut().unwrap().remove("use");
        let previous = serde_json::json!({
            "orders": {"keys": [retired, retained]},
            "legacy": {"keys": [alias]}
        })
        .to_string();
        assert_eq!(
            TrustRegistry::parse(&previous).unwrap_err().reason,
            "repeats public key material in the trust registry"
        );
        let candidate = |key| {
            TrustRegistry::parse(&serde_json::json!({"billing": {"keys": [key]}}).to_string())
                .unwrap()
        };
        let remapped = candidate(alias);
        assert!(
            remapped
                .check_key_withdrawal(&previous, "orders", Some("old"))
                .is_err()
        );
        let standby = candidate(retained);
        assert!(
            standby
                .check_key_withdrawal(&previous, "orders", Some("old"))
                .is_ok()
        );
        assert!(
            standby
                .check_key_withdrawal(&previous, "orders", None)
                .is_err()
        );
        let replaced = candidate(TestKey::new("old").jwk());
        assert!(
            replaced
                .check_key_withdrawal(&previous, "orders", None)
                .is_ok()
        );
        assert!(
            replaced
                .check_key_withdrawal(&previous, "unknown", None)
                .is_err()
        );
        assert!(
            replaced
                .check_key_withdrawal(&previous, "orders", Some("unknown"))
                .is_err()
        );
        let ambiguous = serde_json::json!({"orders": {"keys": [retired, retired]}}).to_string();
        assert!(
            replaced
                .check_key_withdrawal(&ambiguous, "orders", Some("old"))
                .is_err()
        );
    }

    #[test]
    fn a_jwk_set_requires_a_named_keys_array() {
        let key = TestKey::new("orders/1").jwk();
        let object = serde_json::json!({"orders": {"keys": [&key]}}).to_string();
        assert!(TrustRegistry::parse(&object).is_ok());
        for invalid in [
            serde_json::json!({"orders": [[&key]]}),
            serde_json::json!({"orders": {"keys": &key}}),
            serde_json::json!({"orders": {"other": [&key]}}),
        ] {
            let error = TrustRegistry::parse(&invalid.to_string()).unwrap_err();
            assert_eq!(
                error.to_string(),
                "caller orders: is not a JWK Set with a keys array"
            );
        }
    }

    #[test]
    fn refuses_every_entry_it_cannot_use_naming_caller_and_position() {
        let key = TestKey::new("orders/1");
        let jwk = key.jwk();
        let with = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut changed = jwk.clone();
            change(&mut changed);
            serde_json::json!({"orders": {"keys": [jwk.clone(), changed]}}).to_string()
        };
        let cases: Vec<(String, &str)> = vec![
            ("[]".to_owned(), "the trust registry is not one JSON object of JWK Sets"),
            ("{\n  x".to_owned(), "line 2, column 3: invalid JSON in the trust registry"),
            ("{\n  \"orders\": {".to_owned(), "line 2, column 13: invalid JSON in the trust registry"),
            ("{}".to_owned(), "the trust registry names no caller"),
            (
                r#"{"orders": {"keys": []}, "orders": {"keys": []}}"#.to_owned(),
                "caller orders: is repeated in the trust registry",
            ),
            (
                r#"{"orders": {}, "orders": {}, "billing": {}}"#.to_owned(),
                "caller orders: is repeated in the trust registry",
            ),
            (
                r#"{"Bad": {}, "Bad": {}}"#.to_owned(),
                "the trust registry repeats a caller",
            ),
            (r#"{"Orders": {"keys": []}}"#.to_owned(), "a service name is 1 to 63 lowercase letters, digits and hyphens, not starting or ending with a hyphen"),
            (
                format!(r#"{{"orders": {{"keys": [{jwk}], "keys": [{jwk}]}}}}"#),
                "caller orders: repeats a member name within its JWK Set",
            ),
            (
                format!(
                    r#"{{"orders": {{"keys": [{}]}}}}"#,
                    jwk.to_string().replacen('{', r#"{"kid": "orders/0", "#, 1)
                ),
                "caller orders: repeats a member name within its JWK Set",
            ),
            (r#"{"orders": []}"#.to_owned(), "caller orders: is not a JWK Set with a keys array"),
            (r#"{"orders": {"keys": []}}"#.to_owned(), "caller orders: has no key"),
            (with(&|_| {}), "caller orders key 2: repeats a kid of this caller"),
            (with(&|jwk| jwk["kid"] = "other".into()), "caller orders key 2: repeats public key material in the trust registry"),
            (with(&|jwk| jwk["d"] = "AAAA".into()), "caller orders key 2: contains private key material"),
            (with(&|jwk| jwk["crv"] = "P-384".into()), "caller orders key 2: is not an EC P-256 key"),
            (with(&|jwk| jwk["alg"] = "ES384".into()), "caller orders key 2: names an algorithm other than ES256"),
            (with(&|jwk| jwk["use"] = "enc".into()), "caller orders key 2: is not a signature key"),
            (with(&|jwk| jwk["key_ops"] = serde_json::json!(["verify", "sign"])), "caller orders key 2: allows operations other than verify"),
            (with(&|jwk| jwk["key_ops"] = serde_json::json!([])), "caller orders key 2: key_ops is empty"),
            (with(&|jwk| { jwk.as_object_mut().unwrap().remove("kid"); }), "caller orders key 2: has no kid"),
            (with(&|jwk| jwk["kid"] = "two words".into()), "caller orders key 2: the key id is not 1 to 256 visible ASCII characters"),
            (with(&|jwk| { jwk["kid"] = "other".into(); jwk["x"] = "AAAA".into(); }), "caller orders key 2: has malformed coordinates"),
            (with(&|jwk| { jwk["kid"] = "other".into(); jwk["y"] = jwk["x"].clone(); }), "caller orders key 2: is not a point on P-256"),
            (
                serde_json::json!({"orders": {"keys": [{"kty": "RSA", "kid": "r", "n": "AQAB", "e": "AQAB"}]}}).to_string(),
                "caller orders key 1: is not an EC P-256 key",
            ),
            (
                serde_json::json!({"orders": {"keys": [{"kty": "oct", "kid": "h", "k": "c2VjcmV0"}]}}).to_string(),
                "caller orders key 1: contains private key material",
            ),
        ];
        for (document, expected) in cases {
            let error = TrustRegistry::parse(&document).expect_err(&document);
            assert_eq!(error.to_string(), expected, "{document}");
        }
    }
}
