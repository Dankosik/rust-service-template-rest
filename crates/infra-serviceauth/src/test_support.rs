//! Test keys and hand-built tokens for tests that exercise a real verifier.
//! Signing still needs a key the trust registry holds: nothing here
//! constructs a [`Caller`](crate::Caller) or bypasses verification.

#![expect(
    clippy::expect_used,
    reason = "test fixtures fail with their setup context"
)]

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, DecodingKey,
    jwk::{Jwk, PublicKeyUse},
};

pub struct TestKey {
    kid: String,
    pair: EcdsaKeyPair,
    pkcs8: Vec<u8>,
}

impl std::fmt::Debug for TestKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl TestKey {
    #[must_use]
    pub fn new(kid: &str) -> Self {
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .expect("P-256 key generation")
                .as_ref()
                .to_vec();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8)
            .expect("generated key parses");
        Self {
            kid: kid.to_owned(),
            pair,
            pkcs8,
        }
    }

    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    #[must_use]
    pub fn jwk(&self) -> serde_json::Value {
        let key = DecodingKey::from_ec_der(self.pair.public_key().as_ref());
        let mut jwk =
            Jwk::from_decoding_key(&key, Some(Algorithm::ES256)).expect("P-256 public key");
        jwk.common.key_id = Some(self.kid.clone());
        jwk.common.public_key_use = Some(PublicKeyUse::Signature);
        serde_json::to_value(jwk).expect("public JWK serialization")
    }

    #[must_use]
    pub fn pem(&self) -> String {
        pem::encode_config(
            &pem::Pem::new("PRIVATE KEY", self.pkcs8.as_slice()),
            pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
        )
    }

    #[must_use]
    pub fn sign(&self, header: &serde_json::Value, claims: &serde_json::Value) -> String {
        self.sign_raw(header.to_string().as_bytes(), claims.to_string().as_bytes())
    }

    #[must_use]
    pub fn sign_raw(&self, header: &[u8], claims: &[u8]) -> String {
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let signature = self
            .pair
            .sign(&SystemRandom::new(), message.as_bytes())
            .expect("signing");
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
    }
}

#[must_use]
pub fn registry(callers: &[(&str, &[&TestKey])]) -> String {
    let mut document = serde_json::Map::new();
    for (name, keys) in callers {
        let keys: Vec<serde_json::Value> = keys.iter().map(|key| key.jwk()).collect();
        document.insert((*name).to_owned(), serde_json::json!({ "keys": keys }));
    }
    serde_json::Value::Object(document).to_string()
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::KeyPair as _;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::TestKey;

    #[test]
    fn registry_key_publishes_only_the_signers_public_coordinates_and_metadata() {
        let key = TestKey::new("orders/1");
        let point = key.pair.public_key().as_ref();
        assert_eq!(
            key.jwk(),
            serde_json::json!({
                "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": "orders/1",
                "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
                "y": URL_SAFE_NO_PAD.encode(&point[33..]),
            }),
        );
    }
}
