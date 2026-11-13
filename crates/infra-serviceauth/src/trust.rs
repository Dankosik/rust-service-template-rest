//! The trust registry: each trusted caller and its public signing keys.

use std::{collections::BTreeMap, fmt};

use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, ParsedPublicKey};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use serde::{Deserialize, Deserializer, de};

use crate::{InvalidKeyId, InvalidServiceName, KeyId, ServiceName};

/// The callers a receiver trusts, read once from one JSON object whose
/// members are caller names and whose values are JWK Sets (RFC 7517) of EC
/// P-256 public signing keys. Every entry must be usable: a registry is
/// operator input, and a skipped key would surface only as refused calls.
pub struct TrustRegistry {
    callers: BTreeMap<ServiceName, Vec<TrustedKey>>,
}

pub(crate) struct TrustedKey {
    pub(crate) kid: KeyId,
    pub(crate) key: ParsedPublicKey,
}

/// Why a registry was refused. Locates a JSON error by position or an entry
/// by its validated caller name and key position, without exposing key material.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{place}{reason}")]
pub struct TrustError {
    place: Place,
    reason: &'static str,
}

/// Where in the registry document a refusal applies.
#[derive(Clone, Debug, Eq, PartialEq, derive_more::Display)]
enum Place {
    #[display("")]
    Document,
    #[display("line {line}, column {column}: ")]
    Json { line: usize, column: usize },
    #[display("caller {_0}: ")]
    Caller(ServiceName),
    /// A caller's key by its 1-based position in the caller's `keys`.
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
    /// Admits a registry document.
    ///
    /// # Errors
    /// Returns [`TrustError`] for a document that is not one JSON object, a
    /// repeated or ill-formed caller name, no caller, a caller without keys,
    /// a repeated `kid` within a caller, a repeated member name anywhere in a
    /// caller's JWK Set, or any key that is not an EC P-256
    /// public key for ES256 signatures with a `kid`.
    pub fn parse(document: &str) -> Result<Self, TrustError> {
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
        let entries = entries
            .0
            .map_err(|repeated| match ServiceName::parse(&repeated) {
                Ok(caller) => {
                    TrustError::new(Place::Caller(caller), "is repeated in the trust registry")
                }
                Err(_) => TrustError::document("the trust registry repeats a caller"),
            })?;
        if entries.is_empty() {
            return Err(TrustError::document("the trust registry names no caller"));
        }
        let mut callers = BTreeMap::new();
        for (name, set) in entries {
            let (caller, keys) = parse_caller(&name, set)?;
            callers.insert(caller, keys);
        }
        Ok(Self { callers })
    }

    #[must_use]
    pub fn caller_count(&self) -> usize {
        self.callers.len()
    }

    pub(crate) fn callers(&self) -> impl Iterator<Item = &ServiceName> {
        self.callers.keys()
    }

    /// The registered name and keys of a caller, by its spelling in `iss`.
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
            .field("callers", &self.callers.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The document's members in order, or the first repeated member: a repeat
/// is a refusal, since JSON parsers disagree about which duplicate wins.
struct Entries(Result<Vec<(String, DuplicateCheckedJson)>, String>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Entries;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of JWK Sets")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries: Vec<(String, DuplicateCheckedJson)> = Vec::new();
                let mut repeated = None;
                // Read to the end even after a repeat: serde_json's `end_map`
                // refuses a visitor that stops early, and `parse` turns any
                // serde error into the unnamed document error.
                while let Some((name, value)) = map.next_entry::<String, DuplicateCheckedJson>()? {
                    if repeated.is_none() && entries.iter().any(|(known, _)| *known == name) {
                        repeated = Some(name.clone());
                    }
                    entries.push((name, value));
                }
                Ok(Entries(repeated.map_or(Ok(entries), Err)))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

/// A JSON value, or `None` when an object anywhere in it repeats a member
/// name: [`serde_json::Value`] silently keeps the last duplicate, which would
/// drop a key set or a `kid` the operator wrote.
struct DuplicateCheckedJson(Option<serde_json::Value>);

impl<'de> Deserialize<'de> for DuplicateCheckedJson {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = DuplicateCheckedJson;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value")
            }
            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(value.into())))
            }
            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(value.into())))
            }
            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(value.into())))
            }
            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(value.into())))
            }
            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(value.into())))
            }
            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(value.into())))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(DuplicateCheckedJson(Some(serde_json::Value::Null)))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut items = Some(Vec::new());
                while let Some(DuplicateCheckedJson(item)) = seq.next_element()? {
                    items = items.zip(item).map(|(mut items, item)| {
                        items.push(item);
                        items
                    });
                }
                Ok(DuplicateCheckedJson(items.map(serde_json::Value::Array)))
            }
            // Reads to the end after a repeat, for the reason `Entries` does.
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut members = Some(serde_json::Map::new());
                while let Some((name, DuplicateCheckedJson(value))) = map.next_entry()? {
                    members = members
                        .filter(|members| !members.contains_key(&name))
                        .zip(value)
                        .map(|(mut members, value)| {
                            members.insert(name, value);
                            members
                        });
                }
                Ok(DuplicateCheckedJson(members.map(serde_json::Value::Object)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// A JWK Set; other members are ignored, as RFC 7517 section 5 requires.
#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<serde_json::Value>,
}

fn parse_caller(
    name: &str,
    set: DuplicateCheckedJson,
) -> Result<(ServiceName, Vec<TrustedKey>), TrustError> {
    let caller =
        ServiceName::parse(name).map_err(|_| TrustError::document(InvalidServiceName::MESSAGE))?;
    let caller_error = |reason| TrustError::new(Place::Caller(caller.clone()), reason);
    let set = set
        .0
        .ok_or_else(|| caller_error("repeats a member name within its JWK Set"))?;
    let set: JwkSet = serde_json::from_value(set)
        .map_err(|_| caller_error("is not a JWK Set with a keys array"))?;
    if set.keys.is_empty() {
        return Err(caller_error("has no key"));
    }
    let mut keys: Vec<TrustedKey> = Vec::with_capacity(set.keys.len());
    for (index, value) in set.keys.into_iter().enumerate() {
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
    // A private (`d`) or symmetric (`k`) member means a secret was distributed.
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
    let key = ParsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, point)
        .map_err(|_| "is not a point on P-256")?;
    Ok(TrustedKey { kid, key })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestKey;

    #[test]
    fn admits_each_callers_keys_under_its_own_name() {
        let orders = TestKey::new("orders/production/2026-10");
        let rotated = TestKey::new("orders/production/2027-01");
        let billing = TestKey::new("billing/production/2026-10");
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
