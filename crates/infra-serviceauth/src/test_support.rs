//! Test keys and hand-built tokens for tests that exercise a real verifier.
//! Signing still needs a key the trust registry holds: nothing here
//! constructs a [`Caller`](crate::Caller) or bypasses verification.

#![allow(
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "test fixtures fail with their setup context"
)]

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};

/// A generated EC P-256 key and its `kid`.
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
    /// Generates a key.
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

    /// The public JWK a trust registry publishes.
    #[must_use]
    pub fn jwk(&self) -> serde_json::Value {
        let point = self.pair.public_key().as_ref();
        serde_json::json!({
            "kty": "EC",
            "crv": "P-256",
            "kid": self.kid,
            "alg": "ES256",
            "use": "sig",
            "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
            "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
        })
    }

    /// The PKCS#8 private key as the environment carries it.
    #[must_use]
    pub fn pem(&self) -> String {
        let body = STANDARD.encode(&self.pkcs8);
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|line| std::str::from_utf8(line).expect("base64 is ASCII"))
            .collect();
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            lines.join("\n")
        )
    }

    /// A compact JWS over the given header and claims, signed as given.
    #[must_use]
    pub fn sign(&self, header: &serde_json::Value, claims: &serde_json::Value) -> String {
        self.sign_raw(header.to_string().as_bytes(), claims.to_string().as_bytes())
    }

    /// A compact JWS over raw header and claims bytes.
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

/// A registry document trusting each key under its caller name.
#[must_use]
pub fn registry(callers: &[(&str, &[&TestKey])]) -> String {
    let mut document = serde_json::Map::new();
    for (name, keys) in callers {
        let keys: Vec<serde_json::Value> = keys.iter().map(|key| key.jwk()).collect();
        document.insert((*name).to_owned(), serde_json::json!({ "keys": keys }));
    }
    serde_json::Value::Object(document).to_string()
}
