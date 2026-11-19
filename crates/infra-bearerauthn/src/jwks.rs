use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use aws_lc_rs::signature::{self, ParsedPublicKey, RsaParameters, RsaPublicKeyComponents};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use enum_map::EnumMap;
use jsonwebtoken::{
    Algorithm,
    jwk::{
        AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse,
        RSAKeyParameters,
    },
};
use serde::Deserialize;
use tokio::time::Instant;

use crate::{
    DiscoveryError, EndpointUrl, IssuerUrl, JsonObject, JwksError, PreparationError,
    PreparationPhase, ProviderFailure, VerificationError, VerificationReason,
    jwt::discover_jwks_uri,
    provider::{Document, ProviderClient},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, enum_map::Enum)]
pub enum JwtAlgorithm {
    Rs256,
    Es256,
    Ps256,
    EdDsa,
}

pub(crate) type AllowedAlgorithms = EnumMap<JwtAlgorithm, bool>;

pub(crate) fn allowed_algorithms(algorithms: &[JwtAlgorithm]) -> AllowedAlgorithms {
    let mut allowed = AllowedAlgorithms::default();
    for &algorithm in algorithms {
        allowed[algorithm] = true;
    }
    allowed
}

const MIN_RSA_MODULUS_BITS: usize = 2048;
const MAX_RSA_MODULUS_BITS: usize = 8192;

pub(crate) const MAX_SIGNATURE_BYTES: usize = MAX_RSA_MODULUS_BITS / 8;

pub(crate) const KEY_REJECTIONS_METRIC: &str = "authn_jwks_key_rejections_total";

pub(crate) enum SignatureVerificationError {
    NoCandidate,
    BadSignature,
}

impl From<SignatureVerificationError> for VerificationError {
    fn from(error: SignatureVerificationError) -> Self {
        match error {
            SignatureVerificationError::NoCandidate => {
                Self::invalid(VerificationReason::UnknownKey)
            }
            SignatureVerificationError::BadSignature => {
                Self::invalid(VerificationReason::Signature)
            }
        }
    }
}

pub(super) struct SignatureInput<'a> {
    pub(super) message: &'a [u8],
    pub(super) signature: &'a [u8],
    pub(super) kid: Option<&'a str>,
    pub(super) algorithm: JwtAlgorithm,
}

struct JwtKey {
    kid: Option<String>,
    algorithm: JwtAlgorithm,
    public_key: ParsedPublicKey,
}

pub(crate) struct KeySet {
    by_algorithm: EnumMap<JwtAlgorithm, AlgorithmKeys>,
    generation: u64,
    acquired_at: tokio::time::Instant,
}

#[derive(Default)]
struct AlgorithmKeys {
    all: Vec<ParsedPublicKey>,
    by_kid: HashMap<String, Vec<usize>>,
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

impl KeySet {
    pub(crate) fn is_empty(&self) -> bool {
        self.by_algorithm.values().all(|keys| keys.all.is_empty())
    }

    pub(crate) fn require_fresh(
        &self,
        max_age: std::time::Duration,
    ) -> Result<(), VerificationError> {
        if self.acquired_at.elapsed() >= max_age {
            return Err(VerificationError::unavailable(
                VerificationReason::StaleJwks,
            ));
        }
        Ok(())
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn verify(
        &self,
        input: &SignatureInput<'_>,
    ) -> Result<(), SignatureVerificationError> {
        let keys = &self.by_algorithm[input.algorithm];
        if keys.all.is_empty() {
            return Err(SignatureVerificationError::NoCandidate);
        }
        let verifies =
            |key: &ParsedPublicKey| key.verify_sig(input.message, input.signature).is_ok();
        let signature_matches = if let Some(kid) = input.kid {
            keys.by_kid
                .get(kid)
                .ok_or(SignatureVerificationError::NoCandidate)?
                .iter()
                .any(|&index| verifies(&keys.all[index]))
        } else {
            keys.all.iter().any(verifies)
        };
        if signature_matches {
            Ok(())
        } else {
            Err(SignatureVerificationError::BadSignature)
        }
    }

    #[cfg(test)]
    pub(crate) fn has_kid(&self, kid: &str) -> bool {
        self.by_algorithm
            .values()
            .any(|keys| keys.by_kid.contains_key(kid))
    }
}

#[derive(Deserialize)]
struct RawJwks {
    keys: Vec<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
enum KeyRejection {
    MalformedEntry,
    EncryptionOnly,
    IncompatibleUsage,
    UnsupportedFamily,
    AlgorithmBinding,
    InvalidMaterial,
}

impl KeyRejection {
    fn label(self) -> &'static str {
        match self {
            Self::MalformedEntry => "malformed_entry",
            Self::EncryptionOnly => "encryption_only",
            Self::IncompatibleUsage => "incompatible_usage",
            Self::UnsupportedFamily => "unsupported_family",
            Self::AlgorithmBinding => "algorithm_binding",
            Self::InvalidMaterial => "invalid_material",
        }
    }
}

struct ParsedKeySet {
    keys: Vec<JwtKey>,
    rejected: EnumMap<KeyRejection, u64>,
}

impl ParsedKeySet {
    fn has_non_encryption_rejections_without_keys(&self) -> bool {
        self.keys.is_empty()
            && self
                .rejected
                .iter()
                .any(|(reason, &count)| count > 0 && reason != KeyRejection::EncryptionOnly)
    }
}

fn parse_jwks_entries(
    bytes: &[u8],
    configured: AllowedAlgorithms,
) -> Result<ParsedKeySet, KeySetFailure> {
    let JsonObject(raw) =
        serde_json::from_slice::<JsonObject<RawJwks>>(bytes).map_err(|_| KeySetFailure::Parse)?;
    let mut rejected = EnumMap::<KeyRejection, u64>::default();
    let mut keys = Vec::new();
    for value in raw.keys {
        let key = Jwk::deserialize(&value)
            .map_err(|_| KeyRejection::MalformedEntry)
            .and_then(|jwk| {
                admit_key(
                    jwk,
                    value.get("alg").and_then(serde_json::Value::as_str),
                    configured,
                )
            });
        match key {
            Ok(key) => keys.push(key),
            Err(reason) => rejected[reason] += 1,
        }
    }
    if configured
        .into_iter()
        .filter(|&(algorithm, enabled)| enabled && algorithm.family() == KeyFamily::Rsa)
        .nth(1)
        .is_some()
    {
        let conflicts = reject_conflicting_rsa_keys(&mut keys);
        if conflicts != 0 {
            rejected[KeyRejection::AlgorithmBinding] += conflicts;
        }
    }
    Ok(ParsedKeySet { keys, rejected })
}

fn build_key_set(parsed: ParsedKeySet, acquired_at: Instant) -> Result<KeySet, KeySetFailure> {
    for (reason, &count) in &parsed.rejected {
        if count == 0 {
            continue;
        }
        tracing::debug!(
            reason = reason.label(),
            count,
            "authn_jwks_entries_rejected"
        );
        metrics::counter!(KEY_REJECTIONS_METRIC, "reason" => reason.label()).increment(count);
    }
    if parsed.has_non_encryption_rejections_without_keys() {
        return Err(KeySetFailure::NoUsableKeys);
    }
    let mut by_algorithm = EnumMap::<JwtAlgorithm, AlgorithmKeys>::default();
    for key in parsed.keys {
        let keys = &mut by_algorithm[key.algorithm];
        if let Some(kid) = key.kid {
            keys.by_kid.entry(kid).or_default().push(keys.all.len());
        }
        keys.all.push(key.public_key);
    }
    Ok(KeySet {
        by_algorithm,
        generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        acquired_at,
    })
}

#[cfg(any(test, feature = "test-support"))]
fn parse_key_set(bytes: &[u8], configured: &[JwtAlgorithm]) -> Result<KeySet, KeySetFailure> {
    let parsed = parse_jwks_entries(bytes, allowed_algorithms(configured))?;
    build_key_set(parsed, Instant::now())
}

fn reject_conflicting_rsa_keys(keys: &mut Vec<JwtKey>) -> u64 {
    #[derive(Eq, PartialEq)]
    enum RsaBinding {
        Single(JwtAlgorithm),
        Conflicting,
    }

    let mut bindings =
        HashMap::<Box<[u8]>, RsaBinding, foldhash::quality::RandomState>::with_hasher(
            foldhash::quality::RandomState::default(),
        );
    for key in keys
        .iter()
        .filter(|key| key.algorithm.family() == KeyFamily::Rsa)
    {
        let bound = bindings
            .entry(key.public_key.as_ref().into())
            .or_insert(RsaBinding::Single(key.algorithm));
        if *bound != RsaBinding::Single(key.algorithm) {
            *bound = RsaBinding::Conflicting;
        }
    }
    let original_len = keys.len();
    keys.retain(|key| {
        key.algorithm.family() != KeyFamily::Rsa
            || bindings.get(key.public_key.as_ref()) != Some(&RsaBinding::Conflicting)
    });
    (original_len - keys.len()) as u64
}

fn admit_key(
    jwk: Jwk,
    raw_alg: Option<&str>,
    configured: AllowedAlgorithms,
) -> Result<JwtKey, KeyRejection> {
    if is_encryption_only(&jwk, raw_alg) {
        return Err(KeyRejection::EncryptionOnly);
    }
    if !permits_signature_verification(&jwk) {
        return Err(KeyRejection::IncompatibleUsage);
    }
    let family = KeyFamily::from_jwk(&jwk).ok_or(KeyRejection::UnsupportedFamily)?;
    let algorithm = bind_algorithm(jwk.common.key_algorithm, family, configured)
        .ok_or(KeyRejection::AlgorithmBinding)?;
    let public_key = algorithm
        .parse_public_key(&jwk.algorithm)
        .ok_or(KeyRejection::InvalidMaterial)?;
    Ok(JwtKey {
        kid: jwk.common.key_id,
        algorithm,
        public_key,
    })
}

fn bind_algorithm(
    explicit: Option<KeyAlgorithm>,
    family: KeyFamily,
    configured: AllowedAlgorithms,
) -> Option<JwtAlgorithm> {
    if let Some(explicit) = explicit {
        Algorithm::try_from(explicit)
            .ok()
            .and_then(JwtAlgorithm::from_jsonwebtoken)
            .filter(|&algorithm| configured[algorithm] && algorithm.family() == family)
    } else {
        let mut matching = configured.into_iter().filter_map(|(algorithm, enabled)| {
            (enabled && algorithm.family() == family).then_some(algorithm)
        });
        let algorithm = matching.next()?;
        matching.next().is_none().then_some(algorithm)
    }
}

fn is_encryption_only(jwk: &Jwk, raw_alg: Option<&str>) -> bool {
    let encryption_use = match jwk.common.public_key_use {
        Some(PublicKeyUse::Encryption) => true,
        None => false,
        _ => return false,
    };
    let operations = jwk.common.key_operations.as_deref();
    if operations.is_some_and(|ops| !has_only_encryption_operations(ops)) {
        return false;
    }
    if !encryption_use && operations.is_none() && raw_alg.is_none() {
        return false;
    }

    if matches!(jwk.algorithm, AlgorithmParameters::RSA(_)) {
        return matches!(
            raw_alg,
            None | Some("RSA1_5" | "RSA-OAEP" | "RSA-OAEP-256" | "RSA-OAEP-384" | "RSA-OAEP-512")
        ) && JwtAlgorithm::Rs256
            .parse_public_key(&jwk.algorithm)
            .is_some();
    }
    matches!(
        raw_alg,
        None | Some("ECDH-ES" | "ECDH-ES+A128KW" | "ECDH-ES+A192KW" | "ECDH-ES+A256KW")
    ) && has_key_agreement_material(&jwk.algorithm)
}

fn has_only_encryption_operations(operations: &[KeyOperations]) -> bool {
    !operations.is_empty()
        && operations.iter().enumerate().all(|(index, operation)| {
            matches!(
                operation,
                KeyOperations::Encrypt
                    | KeyOperations::Decrypt
                    | KeyOperations::WrapKey
                    | KeyOperations::UnwrapKey
                    | KeyOperations::DeriveKey
                    | KeyOperations::DeriveBits
            ) && !operations[..index].contains(operation)
        })
}

fn has_key_agreement_material(parameters: &AlgorithmParameters) -> bool {
    match parameters {
        AlgorithmParameters::EllipticCurve(ec) => {
            let verifier = match ec.curve {
                EllipticCurve::P256 => &signature::ECDSA_P256_SHA256_FIXED,
                EllipticCurve::P384 => &signature::ECDSA_P384_SHA384_FIXED,
                EllipticCurve::P521 => &signature::ECDSA_P521_SHA512_FIXED,
                _ => return false,
            };
            ec.x.len() == ec.y.len()
                && jsonwebtoken::DecodingKey::from_ec_components(&ec.x, &ec.y).is_ok_and(|key| {
                    key.try_get_as_bytes()
                        .is_ok_and(|point| ParsedPublicKey::new(verifier, point).is_ok())
                })
        }
        AlgorithmParameters::Other(key)
            if key.fields.get("kty").and_then(serde_json::Value::as_str) == Some("OKP") =>
        {
            const X25519_PUBLIC_KEY_BYTES: usize = 32;
            const X448_PUBLIC_KEY_BYTES: usize = 56;
            let length = match key.fields.get("crv").and_then(serde_json::Value::as_str) {
                Some("X25519") => X25519_PUBLIC_KEY_BYTES,
                Some("X448") => X448_PUBLIC_KEY_BYTES,
                _ => return false,
            };
            let Some(x) = key.fields.get("x").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let mut bytes = [0_u8; X448_PUBLIC_KEY_BYTES];
            URL_SAFE_NO_PAD.decode_slice(x, &mut bytes) == Ok(length)
                && bytes[..length].iter().any(|&byte| byte != 0)
        }
        _ => false,
    }
}

fn permits_signature_verification(jwk: &Jwk) -> bool {
    matches!(
        jwk.common.public_key_use,
        None | Some(PublicKeyUse::Signature)
    ) && jwk
        .common
        .key_operations
        .as_ref()
        // RFC 7517 §4.3
        .is_none_or(|operations| operations.contains(&KeyOperations::Verify))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum KeyFamily {
    Rsa,
    P256,
    Ed25519,
}

impl KeyFamily {
    fn from_jwk(jwk: &Jwk) -> Option<Self> {
        match &jwk.algorithm {
            AlgorithmParameters::RSA(_) => Some(Self::Rsa),
            AlgorithmParameters::EllipticCurve(parameters)
                if parameters.curve == EllipticCurve::P256 =>
            {
                Some(Self::P256)
            }
            AlgorithmParameters::OctetKeyPair(parameters)
                if parameters.curve == EllipticCurve::Ed25519 =>
            {
                Some(Self::Ed25519)
            }
            _ => None,
        }
    }
}

impl JwtAlgorithm {
    pub(crate) fn from_jsonwebtoken(algorithm: Algorithm) -> Option<Self> {
        match algorithm {
            Algorithm::RS256 => Some(Self::Rs256),
            Algorithm::PS256 => Some(Self::Ps256),
            Algorithm::ES256 => Some(Self::Es256),
            Algorithm::EdDSA => Some(Self::EdDsa),
            _ => None,
        }
    }

    fn family(self) -> KeyFamily {
        match self {
            Self::Rs256 | Self::Ps256 => KeyFamily::Rsa,
            Self::Es256 => KeyFamily::P256,
            Self::EdDsa => KeyFamily::Ed25519,
        }
    }

    fn parse_public_key(self, parameters: &AlgorithmParameters) -> Option<ParsedPublicKey> {
        match (self, parameters) {
            (Self::Rs256, AlgorithmParameters::RSA(key)) => {
                parse_rsa_key(key, &signature::RSA_PKCS1_2048_8192_SHA256)
            }
            (Self::Ps256, AlgorithmParameters::RSA(key)) => {
                parse_rsa_key(key, &signature::RSA_PSS_2048_8192_SHA256)
            }
            (Self::Es256, AlgorithmParameters::EllipticCurve(key)) => {
                let x = URL_SAFE_NO_PAD.decode(&key.x).ok()?;
                let y = URL_SAFE_NO_PAD.decode(&key.y).ok()?;
                let point = shared_rules::rfc7518::p256_point(&x, &y)?;
                ParsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).ok()
            }
            (Self::EdDsa, AlgorithmParameters::OctetKeyPair(key)) => {
                let bytes = URL_SAFE_NO_PAD.decode(&key.x).ok()?;
                ParsedPublicKey::new(&signature::ED25519, bytes).ok()
            }
            _ => None,
        }
    }
}

fn parse_rsa_key(
    key: &RSAKeyParameters,
    algorithm: &'static RsaParameters,
) -> Option<ParsedPublicKey> {
    let n = URL_SAFE_NO_PAD.decode(&key.n).ok()?;
    let e = URL_SAFE_NO_PAD.decode(&key.e).ok()?;
    let n = without_leading_zeros(&n)?;
    let e = without_leading_zeros(&e)?;
    let bits = n.len() * 8 - n[0].leading_zeros() as usize;
    if !(MIN_RSA_MODULUS_BITS..=MAX_RSA_MODULUS_BITS).contains(&bits) {
        return None;
    }
    RsaPublicKeyComponents { n, e }
        .to_parsed_public_key(algorithm)
        .ok()
}

fn without_leading_zeros(bytes: &[u8]) -> Option<&[u8]> {
    let start = bytes.iter().position(|byte| *byte != 0)?;
    Some(&bytes[start..])
}

pub(crate) enum JwksSource {
    Configured(EndpointUrl),
    Discovery(IssuerUrl),
}

impl JwksSource {
    pub(crate) async fn fetch(
        &self,
        provider: &ProviderClient,
        algorithms: AllowedAlgorithms,
        deadline: Instant,
    ) -> Result<Arc<KeySet>, KeySetFailure> {
        let (uri, acquired_at) = match self {
            Self::Configured(uri) => (Cow::Borrowed(uri), None),
            Self::Discovery(issuer) => {
                let started = Instant::now();
                let discovered = discover_jwks_uri(provider, issuer, deadline)
                    .await
                    .map_err(KeySetFailure::Discovery)?;
                if Instant::now() >= deadline {
                    return Err(KeySetFailure::DiscoveryTimeout);
                }
                (Cow::Owned(discovered), Some(started))
            }
        };
        let bytes = provider
            .get_document(uri.url(), Document::Jwks, Some(deadline))
            .await
            .map_err(KeySetFailure::Fetch)?;
        let parsed = parse_jwks_entries(&bytes, algorithms)?;
        let keys = build_key_set(parsed, acquired_at.unwrap_or_else(Instant::now))?;
        Ok(Arc::new(keys))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum KeySetFailure {
    Discovery(DiscoveryError),
    DiscoveryTimeout,
    Fetch(ProviderFailure),
    Parse,
    NoUsableKeys,
}

impl From<KeySetFailure> for PreparationError {
    fn from(failure: KeySetFailure) -> Self {
        match failure {
            KeySetFailure::Discovery(error) => Self::Discovery(error),
            KeySetFailure::DiscoveryTimeout => Self::Timeout(PreparationPhase::Discovery),
            KeySetFailure::Fetch(failure) => Self::Jwks(JwksError::Fetch(failure)),
            KeySetFailure::Parse => Self::Jwks(JwksError::Parse),
            KeySetFailure::NoUsableKeys => Self::Jwks(JwksError::NoUsableKeys),
        }
    }
}

impl KeySetFailure {
    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Self::Discovery(_) | Self::DiscoveryTimeout => "discovery",
            Self::Fetch(_) => "fetch",
            Self::Parse => "parse",
            Self::NoUsableKeys => "no_usable_keys",
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[expect(
    clippy::unwrap_used,
    reason = "test fixture failures surface at key generation and parsing"
)]
pub(crate) mod fixtures {
    use std::sync::Arc;

    use jsonwebtoken::{Algorithm, EncodingKey, jwk::Jwk};

    use super::{JwtAlgorithm, KeySet, parse_key_set};

    pub(crate) const JWT_SIGNING_DER: &[u8] =
        include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");

    pub(crate) fn ensure_crypto_provider() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }

    pub(crate) fn rsa_signing() -> EncodingKey {
        EncodingKey::from_rsa_der(JWT_SIGNING_DER)
    }

    pub(crate) fn jwk(
        signing: &EncodingKey,
        algorithm: Algorithm,
        kid: Option<&str>,
        include_alg: bool,
    ) -> serde_json::Value {
        let mut jwk = Jwk::from_encoding_key(signing, algorithm).unwrap();
        jwk.common.key_id = kid.map(str::to_owned);
        if !include_alg {
            jwk.common.key_algorithm = None;
        }
        serde_json::to_value(jwk).unwrap()
    }

    pub(crate) fn key_set(keys: &[serde_json::Value], configured: &[JwtAlgorithm]) -> Arc<KeySet> {
        ensure_crypto_provider();
        let bytes = serde_json::to_vec(&serde_json::json!({ "keys": keys })).unwrap();
        Arc::new(parse_key_set(&bytes, configured).unwrap())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey};

    pub(crate) use super::fixtures::{
        JWT_SIGNING_DER, ensure_crypto_provider, jwk, key_set, rsa_signing,
    };
    use super::{
        JwtAlgorithm, KeySetFailure, SignatureInput, SignatureVerificationError, parse_key_set,
    };
    use crate::test_metrics::{Diagnostics, labeled};

    pub(crate) fn ec_signing() -> EncodingKey {
        EncodingKey::from_ec_der(&rcgen::KeyPair::generate().unwrap().serialize_der())
    }

    pub(crate) fn ed_signing(kid: &str) -> (EncodingKey, serde_json::Value) {
        use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};

        let pair = Ed25519KeyPair::generate().unwrap();
        let signing = EncodingKey::from_ed_der(pair.to_pkcs8v1().unwrap().as_ref());
        let x = URL_SAFE_NO_PAD.encode(pair.public_key().as_ref());
        let jwk = serde_json::json!({"kty": "OKP", "crv": "Ed25519", "kid": kid, "x": x});
        (signing, jwk)
    }

    #[test]
    fn mixed_jwks_skips_unusable_entries_without_losing_usable_material() {
        let invalid_point = URL_SAFE_NO_PAD.encode([0_u8; 32]);
        let mut padded = jwk(&rsa_signing(), Algorithm::RS256, Some("padded"), true);
        let modulus = URL_SAFE_NO_PAD
            .decode(padded["n"].as_str().unwrap())
            .unwrap();
        padded["n"] = URL_SAFE_NO_PAD
            .encode([[0].as_slice(), &modulus].concat())
            .into();
        let keys = key_set(
            &[
                padded,
                serde_json::json!({"kty": "EC", "crv": "P-256", "kid": "bad-point", "alg": "ES256", "x": invalid_point, "y": invalid_point}),
                jwk(&ec_signing(), Algorithm::ES256, Some("unconfigured"), true),
            ],
            &[JwtAlgorithm::Rs256],
        );
        assert!(keys.has_kid("padded"));
        assert!(!keys.has_kid("bad-point"));
        assert!(!keys.has_kid("unconfigured"));
    }

    #[test]
    fn signature_verification_keeps_duplicate_kids_and_matches_only_the_token_algorithm() {
        let first = ec_signing();
        let second = ec_signing();
        let unnamed = ec_signing();
        let keys = key_set(
            &[
                jwk(&first, Algorithm::ES256, Some("shared"), true),
                jwk(&rsa_signing(), Algorithm::RS256, Some("rsa"), true),
                jwk(&unnamed, Algorithm::ES256, None, true),
                jwk(&second, Algorithm::ES256, Some("shared"), true),
            ],
            &[JwtAlgorithm::Es256, JwtAlgorithm::Rs256],
        );
        let message = b"header.payload";
        let signature = |signing| {
            let encoded = jsonwebtoken::crypto::sign(message, signing, Algorithm::ES256).unwrap();
            URL_SAFE_NO_PAD.decode(encoded).unwrap()
        };
        let verify = |kid: Option<&str>, signature: &[u8]| {
            keys.verify(&SignatureInput {
                message,
                signature,
                kid,
                algorithm: JwtAlgorithm::Es256,
            })
        };
        let first_signature = signature(&first);
        let second_signature = signature(&second);
        let unnamed_signature = signature(&unnamed);

        assert!(verify(Some("shared"), &first_signature).is_ok());
        assert!(verify(Some("shared"), &second_signature).is_ok());
        assert!(verify(None, &second_signature).is_ok());
        assert!(verify(None, &unnamed_signature).is_ok());
        assert!(matches!(
            verify(Some("shared"), &unnamed_signature),
            Err(SignatureVerificationError::BadSignature)
        ));
        assert!(matches!(
            verify(Some("rsa"), &second_signature),
            Err(SignatureVerificationError::NoCandidate)
        ));
    }

    #[test]
    fn rsa_aliases_must_bind_the_same_key_material_to_the_same_algorithm() {
        let signing = rsa_signing();
        let primary = jwk(&signing, Algorithm::RS256, Some("primary"), true);
        let unrelated = jwk(&ec_signing(), Algorithm::ES256, Some("unrelated"), true);
        let algorithms = [
            JwtAlgorithm::Rs256,
            JwtAlgorithm::Ps256,
            JwtAlgorithm::Es256,
        ];
        for (algorithm, admitted) in [(Algorithm::RS256, true), (Algorithm::PS256, false)] {
            let mut alias = jwk(&signing, algorithm, Some("alias"), true);
            for field in ["n", "e"] {
                let bytes = URL_SAFE_NO_PAD
                    .decode(alias[field].as_str().unwrap())
                    .unwrap();
                alias[field] = URL_SAFE_NO_PAD
                    .encode([[0].as_slice(), &bytes].concat())
                    .into();
            }
            for (first, second) in [(&primary, &alias), (&alias, &primary)] {
                let keys = key_set(
                    &[first.clone(), second.clone(), unrelated.clone()],
                    &algorithms,
                );
                assert_eq!(keys.has_kid("primary"), admitted, "{algorithm:?}");
                assert_eq!(keys.has_kid("alias"), admitted, "{algorithm:?}");
                assert!(keys.has_kid("unrelated"));
            }
        }
    }

    #[test]
    fn key_operations_admit_a_key_only_when_they_permit_verification() {
        ensure_crypto_provider();
        for (operations, admitted) in [
            (serde_json::json!(["verify"]), true),
            (serde_json::json!(["sign", "verify"]), true),
            (serde_json::json!(["sign"]), false),
            (serde_json::json!([]), false),
        ] {
            let mut key = jwk(&rsa_signing(), Algorithm::RS256, Some("k"), true);
            key["key_ops"] = operations.clone();
            let bytes = serde_json::to_vec(&serde_json::json!({ "keys": [key] })).unwrap();
            assert_eq!(
                parse_key_set(&bytes, &[JwtAlgorithm::Rs256]).is_ok(),
                admitted,
                "{operations}"
            );
        }
    }

    #[test]
    fn only_a_key_published_for_signatures_is_admitted() {
        ensure_crypto_provider();
        for (key_use, admitted) in [(None, true), (Some("sig"), true), (Some("enc"), false)] {
            let mut key = jwk(&rsa_signing(), Algorithm::RS256, Some("k"), true);
            let entry = key.as_object_mut().unwrap();
            entry.remove("use");
            if let Some(key_use) = key_use {
                entry.insert("use".to_owned(), key_use.into());
            }
            let bytes = serde_json::to_vec(&serde_json::json!({ "keys": [key] })).unwrap();
            assert_eq!(
                parse_key_set(&bytes, &[JwtAlgorithm::Rs256]).is_ok(),
                admitted,
                "{key_use:?}"
            );
        }
    }

    #[test]
    fn encryption_only_publications_require_unambiguous_key_metadata() {
        ensure_crypto_provider();
        let rsa = jwk(&rsa_signing(), Algorithm::RS256, Some("enc"), false);
        for (metadata, withdrawn) in [
            (serde_json::json!({"key_ops":["wrapKey"]}), true),
            (serde_json::json!({"key_ops":["wrapKey","wrapKey"]}), false),
            (serde_json::json!({"alg":"RSA-OAEP-256"}), true),
            (serde_json::json!({"use":"enc","key_ops":["verify"]}), false),
            (serde_json::json!({"use":"sig","alg":"RSA-OAEP"}), false),
            (serde_json::json!({"use":"enc","n":"","e":""}), false),
        ] {
            let mut key = rsa.clone();
            key.as_object_mut()
                .unwrap()
                .extend(metadata.as_object().unwrap().clone());
            let bytes = serde_json::to_vec(&serde_json::json!({"keys":[key]})).unwrap();
            let result = parse_key_set(&bytes, &[JwtAlgorithm::Rs256]);
            assert_eq!(
                result.map(|set| set.is_empty()),
                if withdrawn {
                    Ok(true)
                } else {
                    Err(KeySetFailure::NoUsableKeys)
                },
                "{metadata}"
            );
        }
        let keys = serde_json::json!({"keys": [
            {"use":"enc","kty":"RSA"},
            {"use":"enc","kty":"RSA","n":rsa["n"],"e":rsa["e"]}
        ]});
        assert_eq!(
            parse_key_set(&serde_json::to_vec(&keys).unwrap(), &[JwtAlgorithm::Rs256]).err(),
            Some(KeySetFailure::NoUsableKeys)
        );
    }

    #[test]
    fn only_valid_key_agreement_material_can_withdraw_signing_keys() {
        ensure_crypto_provider();
        let ec = jwk(&ec_signing(), Algorithm::ES256, Some("agreement"), false);
        let x25519 =
            serde_json::json!({"kty":"OKP","crv":"X25519", "x":URL_SAFE_NO_PAD.encode([9_u8; 32])});
        for (mut key, algorithm, expected) in [
            (ec.clone(), "ECDH-ES", Ok(true)),
            (x25519, "ECDH-ES+A128KW", Ok(true)),
            (
                serde_json::json!({"use":"enc", "kty":"EC","crv":"P-256","x":ec["x"],"y":ec["y"]}),
                "ES512",
                Err(KeySetFailure::NoUsableKeys),
            ),
            (
                serde_json::json!({"kty":"OKP","crv":"X25519","x":"!"}),
                "ECDH-ES",
                Err(KeySetFailure::NoUsableKeys),
            ),
        ] {
            key["alg"] = algorithm.into();
            let bytes = serde_json::to_vec(&serde_json::json!({"keys":[key]})).unwrap();
            assert_eq!(
                parse_key_set(&bytes, &[JwtAlgorithm::Es256]).map(|set| set.is_empty()),
                expected,
                "{key}"
            );
        }
    }

    #[test]
    fn an_rsa_modulus_outside_the_verifier_bounds_is_not_admitted() {
        ensure_crypto_provider();
        for (bits, admitted) in [
            (1024_usize, false),
            (2047, false),
            (2048, true),
            (8192, true),
            (8200, false),
        ] {
            let mut modulus = vec![0xff_u8; bits.div_ceil(8)];
            modulus[0] = 0xff >> (modulus.len() * 8 - bits);
            let key = serde_json::json!({
                "kty": "RSA", "kid": "k", "alg": "RS256",
                "n": URL_SAFE_NO_PAD.encode(&modulus), "e": "AQAB",
            });
            let bytes = serde_json::to_vec(&serde_json::json!({ "keys": [key] })).unwrap();
            assert_eq!(
                parse_key_set(&bytes, &[JwtAlgorithm::Rs256]).is_ok(),
                admitted,
                "{bits} bits"
            );
        }
    }

    #[test]
    fn an_unreadable_key_set_is_refused() {
        assert_eq!(
            parse_key_set(br#"{"keys":false}"#, &[JwtAlgorithm::Rs256]).err(),
            Some(KeySetFailure::Parse)
        );
    }

    #[test]
    fn rejected_entries_are_counted_per_reason_in_one_event_without_key_ids() {
        let diagnostics = Diagnostics::default();
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = diagnostics.dispatch();
        let entries = vec![serde_json::json!({"kid":"private-key-id","use":false}); 100];
        let bytes = serde_json::to_vec(&serde_json::json!({"keys":entries})).unwrap();
        let parsed = metrics::with_local_recorder(diagnostics.recorder(), || {
            tracing::dispatcher::with_default(&dispatch, || {
                parse_key_set(&bytes, &[JwtAlgorithm::Rs256])
            })
        });
        assert_eq!(parsed.err(), Some(KeySetFailure::NoUsableKeys));
        let counters = diagnostics.take_counters();
        assert!(
            counters.iter().any(|(key, value)| {
                key.name() == "authn_jwks_key_rejections_total"
                    && *value == 100
                    && labeled(key, &[("reason", "malformed_entry")])
            }),
            "recorded counters: {counters:?}"
        );
        let events = diagnostics.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains("authn_jwks_entries_rejected"))
                .count(),
            1,
            "recorded events: {events:?}"
        );
        assert!(
            events.iter().all(|event| !event.contains("private-key-id")),
            "recorded events: {events:?}"
        );
    }
}
