//! The trust registry: each trusted caller and its public signing keys.

use std::{collections::BTreeMap, fmt};

use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, ParsedPublicKey};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use serde::{Deserialize, Deserializer, de};

use crate::{CallerName, admissible_key_id};

/// The callers a receiver trusts, read once from one JSON object whose
/// members are caller names and whose values are JWK Sets (RFC 7517) of EC
/// P-256 public signing keys. Every entry must be usable: a registry is
/// operator input, and a skipped key would surface only as refused calls.
pub struct TrustRegistry {
    callers: BTreeMap<CallerName, Vec<TrustedKey>>,
}

pub(crate) struct TrustedKey {
    pub(crate) kid: String,
    pub(crate) key: ParsedPublicKey,
}

/// Why a registry was refused. Names only a caller that passed the name
/// grammar and the 1-based key position, never key material.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{}{reason}", place(.caller.as_ref(), *.key))]
pub struct TrustError {
    caller: Option<CallerName>,
    key: Option<usize>,
    reason: &'static str,
}

fn place(caller: Option<&CallerName>, key: Option<usize>) -> String {
    match (caller, key) {
        (Some(caller), Some(key)) => format!("caller {caller} key {key}: "),
        (Some(caller), None) => format!("caller {caller}: "),
        _ => String::new(),
    }
}

impl TrustError {
    const fn document(reason: &'static str) -> Self {
        Self {
            caller: None,
            key: None,
            reason,
        }
    }
}

impl TrustRegistry {
    /// Admits a registry document.
    ///
    /// # Errors
    /// Returns [`TrustError`] for a document that is not one JSON object, a
    /// repeated or ill-formed caller name, no caller, a caller without keys,
    /// a repeated `kid` within a caller, or any key that is not an EC P-256
    /// public key for ES256 signatures with a `kid`.
    pub fn parse(document: &str) -> Result<Self, TrustError> {
        let entries: Entries = serde_json::from_str(document).map_err(|_| {
            TrustError::document("the trust registry is not one JSON object of JWK Sets")
        })?;
        let entries = entries
            .0
            .map_err(|repeated| match CallerName::parse(&repeated) {
                Ok(caller) => TrustError {
                    caller: Some(caller),
                    key: None,
                    reason: "is repeated in the trust registry",
                },
                Err(_) => TrustError::document("the trust registry repeats a caller"),
            })?;
        if entries.is_empty() {
            return Err(TrustError::document("the trust registry names no caller"));
        }
        let mut callers = BTreeMap::new();
        for (name, set) in entries {
            let caller = CallerName::parse(&name).map_err(|_| {
                TrustError::document(
                    "a caller name is not 1 to 63 lowercase letters, digits and hyphens",
                )
            })?;
            let error = |key, reason| TrustError {
                caller: Some(caller.clone()),
                key,
                reason,
            };
            let set: JwkSet = serde_json::from_value(set)
                .map_err(|_| error(None, "is not a JWK Set with a keys array"))?;
            if set.keys.is_empty() {
                return Err(error(None, "has no key"));
            }
            let mut keys: Vec<TrustedKey> = Vec::with_capacity(set.keys.len());
            for (index, value) in set.keys.into_iter().enumerate() {
                let key = admit(value).map_err(|reason| error(Some(index + 1), reason))?;
                if keys.iter().any(|known| known.kid == key.kid) {
                    return Err(error(Some(index + 1), "repeats a kid of this caller"));
                }
                keys.push(key);
            }
            callers.insert(caller, keys);
        }
        Ok(Self { callers })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.callers.len()
    }

    /// Always false for an admitted registry, which names at least one caller.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.callers.is_empty()
    }

    pub fn callers(&self) -> impl Iterator<Item = &CallerName> {
        self.callers.keys()
    }

    /// The registered name and keys of a caller, by its spelling in `iss`.
    pub(crate) fn caller(&self, name: &str) -> Option<(&CallerName, &[TrustedKey])> {
        let name = CallerName::parse(name).ok()?;
        self.callers
            .get_key_value(&name)
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
struct Entries(Result<Vec<(String, serde_json::Value)>, String>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Entries;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of JWK Sets")
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries: Vec<(String, serde_json::Value)> = Vec::new();
                let mut repeated = None;
                // The whole document is read, so a repeat anywhere is named.
                while let Some((name, value)) = map.next_entry::<String, serde_json::Value>()? {
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

/// A JWK Set; other members are ignored, as RFC 7517 section 5 requires.
#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<serde_json::Value>,
}

/// Admits one public ES256 key with a `kid`.
fn admit(value: serde_json::Value) -> Result<TrustedKey, &'static str> {
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
    if common.key_operations.as_ref().is_some_and(|operations| {
        operations.is_empty()
            || !operations
                .iter()
                .all(|operation| *operation == KeyOperations::Verify)
    }) {
        return Err("allows operations other than verify");
    }
    let kid = common
        .key_id
        .clone()
        .filter(|kid| admissible_key_id(kid))
        .ok_or("has no kid of 1 to 256 visible ASCII characters")?;
    let coordinate = |encoded: &str| {
        URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .filter(|bytes| bytes.len() == 32)
    };
    let (Some(x), Some(y)) = (coordinate(&parameters.x), coordinate(&parameters.y)) else {
        return Err("has malformed coordinates");
    };
    let point = [[4].as_slice(), &x, &y].concat();
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
            crate::CallerName::parse("catalog").unwrap(),
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
            assert_eq!(caller.name(), issuer, "{}", key.kid());
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
            (r#"{"Orders": {"keys": []}}"#.to_owned(), "a caller name is not 1 to 63 lowercase letters, digits and hyphens"),
            (r#"{"orders": []}"#.to_owned(), "caller orders: is not a JWK Set with a keys array"),
            (r#"{"orders": {"keys": []}}"#.to_owned(), "caller orders: has no key"),
            (with(&|_| {}), "caller orders key 2: repeats a kid of this caller"),
            (with(&|jwk| jwk["d"] = "AAAA".into()), "caller orders key 2: contains private key material"),
            (with(&|jwk| jwk["crv"] = "P-384".into()), "caller orders key 2: is not an EC P-256 key"),
            (with(&|jwk| jwk["alg"] = "ES384".into()), "caller orders key 2: names an algorithm other than ES256"),
            (with(&|jwk| jwk["use"] = "enc".into()), "caller orders key 2: is not a signature key"),
            (with(&|jwk| jwk["key_ops"] = serde_json::json!(["verify", "sign"])), "caller orders key 2: allows operations other than verify"),
            (with(&|jwk| jwk["key_ops"] = serde_json::json!([])), "caller orders key 2: allows operations other than verify"),
            (with(&|jwk| { jwk.as_object_mut().unwrap().remove("kid"); }), "caller orders key 2: has no kid of 1 to 256 visible ASCII characters"),
            (with(&|jwk| jwk["kid"] = "two words".into()), "caller orders key 2: has no kid of 1 to 256 visible ASCII characters"),
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
