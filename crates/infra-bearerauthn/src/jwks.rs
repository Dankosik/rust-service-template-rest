//! JWKS acquisition and key admission: the provider keys a JWT verifier trusts.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use aws_lc_rs::signature::{self, ParsedPublicKey, RsaParameters, RsaPublicKeyComponents};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;

use crate::{
    EndpointUrl, JsonObject, PreparationReason, ProviderFailure, VerificationError,
    VerificationReason,
    provider::{Document, ProviderClient},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum JwtAlgorithm {
    Rs256,
    Es256,
    Ps256,
    EdDsa,
}

/// The bounds of an RSA modulus that aws-lc verifiers accept.
const MIN_RSA_MODULUS_BITS: usize = 2048;
const MAX_RSA_MODULUS_BITS: usize = 8192;

/// The largest signature an admitted key produces: RSA-8192.
pub(crate) const MAX_SIGNATURE_BYTES: usize = MAX_RSA_MODULUS_BITS / 8;

pub(crate) const KEY_REJECTIONS_METRIC: &str = "authn_jwks_key_rejections_total";

/// Why no installed key verified the signature.
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

struct JwtKey {
    kid: Option<String>,
    /// One aws-lc key per algorithm the key serves. A JWK without `alg` serves
    /// every configured algorithm of its family, as Nimbus and go-oidc do.
    /// Parsing once keeps RSA Montgomery setup off the request path.
    parsed_keys: Vec<(JwtAlgorithm, ParsedPublicKey)>,
}

impl JwtKey {
    fn parsed_key(&self, algorithm: JwtAlgorithm) -> Option<&ParsedPublicKey> {
        self.parsed_keys
            .iter()
            .find_map(|(bound, key)| (*bound == algorithm).then_some(key))
    }
}

pub(crate) struct KeySet {
    keys: Vec<JwtKey>,
    generation: u64,
}

/// Source of [`KeySet::generation`]; one counter for the process, so no two
/// parsed sets ever share a value.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

impl KeySet {
    /// Names this parsed set, not its content: a refresh that fetches the
    /// same keys installs a set with a new generation. A decision derived from
    /// the keys stays valid exactly while the installed set has this value.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    fn candidates(
        &self,
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
    ) -> impl Iterator<Item = &ParsedPublicKey> {
        self.keys
            .iter()
            .filter(move |key| kid.is_none_or(|kid| key.kid.as_deref() == Some(kid)))
            .filter_map(move |key| key.parsed_key(algorithm))
    }

    pub(crate) fn verify(
        &self,
        message: &[u8],
        signature: &[u8],
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
    ) -> Result<(), SignatureVerificationError> {
        let mut miss = SignatureVerificationError::NoCandidate;
        for key in self.candidates(kid, algorithm) {
            if key.verify_sig(message, signature).is_ok() {
                return Ok(());
            }
            miss = SignatureVerificationError::BadSignature;
        }
        Err(miss)
    }

    #[cfg(test)]
    pub(crate) fn has_kid(&self, kid: &str) -> bool {
        self.keys.iter().any(|key| key.kid.as_deref() == Some(kid))
    }
}

#[derive(Deserialize)]
struct RawJwks {
    keys: Vec<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum KeyRejection {
    MalformedEntry,
    IncompatibleUsage,
    UnsupportedFamily,
    AlgorithmBinding,
    InvalidMaterial,
}

impl KeyRejection {
    fn label(self) -> &'static str {
        match self {
            Self::MalformedEntry => "malformed_entry",
            Self::IncompatibleUsage => "incompatible_usage",
            Self::UnsupportedFamily => "unsupported_family",
            Self::AlgorithmBinding => "algorithm_binding",
            Self::InvalidMaterial => "invalid_material",
        }
    }
}

fn parse_key_set(bytes: &[u8], configured: &[JwtAlgorithm]) -> Result<KeySet, KeySetFailure> {
    let JsonObject(raw) =
        serde_json::from_slice::<JsonObject<RawJwks>>(bytes).map_err(|_| KeySetFailure::Parse)?;
    let mut rejected = std::collections::BTreeMap::<KeyRejection, u64>::new();
    let mut keys = Vec::new();
    for value in raw.keys {
        let key = serde_json::from_value::<Jwk>(value)
            .map_err(|_| KeyRejection::MalformedEntry)
            .and_then(|jwk| admit_key(jwk, configured));
        match key {
            Ok(key) => keys.push(key),
            Err(reason) => *rejected.entry(reason).or_default() += 1,
        }
    }
    // Aggregate per-entry rejection evidence into one event per closed reason.
    // Neither provider-controlled entry count nor key identifiers multiply logs.
    for (reason, count) in rejected {
        tracing::debug!(
            reason = reason.label(),
            count,
            "authn_jwks_entries_rejected"
        );
        metrics::counter!(KEY_REJECTIONS_METRIC, "reason" => reason.label()).increment(count);
    }
    if keys.is_empty() {
        return Err(KeySetFailure::NoUsableKeys);
    }
    Ok(KeySet {
        keys,
        generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
    })
}

fn admit_key(jwk: Jwk, configured: &[JwtAlgorithm]) -> Result<JwtKey, KeyRejection> {
    if !permits_signature_verification(&jwk) {
        return Err(KeyRejection::IncompatibleUsage);
    }
    let family = KeyFamily::from_jwk(&jwk).ok_or(KeyRejection::UnsupportedFamily)?;
    let algorithms = match jwk.common.key_algorithm {
        Some(explicit) => vec![
            Algorithm::try_from(explicit)
                .ok()
                .and_then(JwtAlgorithm::from_jsonwebtoken)
                .filter(|algorithm| configured.contains(algorithm) && algorithm.family() == family)
                .ok_or(KeyRejection::AlgorithmBinding)?,
        ],
        None => configured
            .iter()
            .copied()
            .filter(|algorithm| algorithm.family() == family)
            .collect(),
    };
    if algorithms.is_empty() {
        return Err(KeyRejection::AlgorithmBinding);
    }
    let material = key_material(&jwk.algorithm).ok_or(KeyRejection::InvalidMaterial)?;
    let parsed_keys = algorithms
        .into_iter()
        .map(|algorithm| material.parse(algorithm).map(|key| (algorithm, key)))
        .collect::<Option<_>>()
        .ok_or(KeyRejection::InvalidMaterial)?;
    Ok(JwtKey {
        kid: jwk.common.key_id,
        parsed_keys,
    })
}

fn permits_signature_verification(jwk: &Jwk) -> bool {
    matches!(
        jwk.common.public_key_use,
        None | Some(PublicKeyUse::Signature)
    ) && jwk
        .common
        .key_operations
        .as_ref()
        // RFC 7517 section 4.3 only says what the key may do; other listed
        // operations are the publisher's concern, not a verifier's.
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

    fn verification(self) -> KeyVerification {
        match self {
            Self::Rs256 => KeyVerification::Rsa(&signature::RSA_PKCS1_2048_8192_SHA256),
            Self::Ps256 => KeyVerification::Rsa(&signature::RSA_PSS_2048_8192_SHA256),
            Self::Es256 => KeyVerification::Raw(&signature::ECDSA_P256_SHA256_FIXED),
            Self::EdDsa => KeyVerification::Raw(&signature::ED25519),
        }
    }
}

enum KeyVerification {
    Rsa(&'static RsaParameters),
    Raw(&'static dyn signature::VerificationAlgorithm),
}

/// Decoded public key material, checked with aws-lc at admission so an
/// unusable entry is skipped instead of failing every token that names it.
enum KeyMaterial {
    Rsa { modulus: Vec<u8>, exponent: Vec<u8> },
    Raw(Vec<u8>),
}

impl KeyMaterial {
    fn parse(&self, algorithm: JwtAlgorithm) -> Option<ParsedPublicKey> {
        match (self, algorithm.verification()) {
            (Self::Rsa { modulus, exponent }, KeyVerification::Rsa(parameters)) => {
                RsaPublicKeyComponents {
                    n: modulus.as_slice(),
                    e: exponent.as_slice(),
                }
                .to_parsed_public_key(parameters)
                .ok()
            }
            (Self::Raw(bytes), KeyVerification::Raw(verification)) => {
                ParsedPublicKey::new(verification, bytes).ok()
            }
            (Self::Rsa { .. }, KeyVerification::Raw(_))
            | (Self::Raw(_), KeyVerification::Rsa(_)) => None,
        }
    }
}

fn key_material(parameters: &AlgorithmParameters) -> Option<KeyMaterial> {
    match parameters {
        AlgorithmParameters::RSA(parameters) => {
            // Some IdPs emit a leading zero byte; install the minimal encoding.
            let modulus = without_leading_zeros(&URL_SAFE_NO_PAD.decode(&parameters.n).ok()?)?;
            let exponent = without_leading_zeros(&URL_SAFE_NO_PAD.decode(&parameters.e).ok()?)?;
            // aws-lc parsing does not bound the size, but its verifiers do.
            let bits = modulus.len() * 8 - modulus[0].leading_zeros() as usize;
            (MIN_RSA_MODULUS_BITS..=MAX_RSA_MODULUS_BITS)
                .contains(&bits)
                .then_some(KeyMaterial::Rsa { modulus, exponent })
        }
        AlgorithmParameters::EllipticCurve(parameters) => {
            let x = URL_SAFE_NO_PAD.decode(&parameters.x).ok()?;
            let y = URL_SAFE_NO_PAD.decode(&parameters.y).ok()?;
            Some(KeyMaterial::Raw(
                shared_rules::rfc7518::p256_point(&x, &y)?.to_vec(),
            ))
        }
        AlgorithmParameters::OctetKeyPair(parameters) => Some(KeyMaterial::Raw(
            URL_SAFE_NO_PAD.decode(&parameters.x).ok()?,
        )),
        _ => None,
    }
}

fn without_leading_zeros(bytes: &[u8]) -> Option<Vec<u8>> {
    let start = bytes.iter().position(|byte| *byte != 0)?;
    Some(bytes[start..].to_vec())
}

pub(crate) async fn fetch_key_set(
    provider: &ProviderClient,
    jwks_uri: &EndpointUrl,
    algorithms: &[JwtAlgorithm],
    deadline: Option<tokio::time::Instant>,
) -> Result<Arc<KeySet>, KeySetFailure> {
    let bytes = provider
        .get_document(jwks_uri.url(), Document::Jwks, deadline)
        .await
        .map_err(KeySetFailure::Fetch)?;
    Ok(Arc::new(parse_key_set(&bytes, algorithms)?))
}

/// Why no key set was acquired, at startup or on refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeySetFailure {
    Fetch(ProviderFailure),
    Parse,
    NoUsableKeys,
}

/// Startup reports a failed key-set acquisition in the
/// [`Jwks`](crate::PreparationPhase::Jwks) phase.
impl From<KeySetFailure> for PreparationReason {
    fn from(failure: KeySetFailure) -> Self {
        match failure {
            KeySetFailure::Fetch(failure) => Self::Fetch(failure),
            KeySetFailure::Parse => Self::Parse,
            KeySetFailure::NoUsableKeys => Self::NoUsableKeys,
        }
    }
}

impl KeySetFailure {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Fetch(_) => "fetch",
            Self::Parse => "parse",
            Self::NoUsableKeys => "no_usable_keys",
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, jwk::Jwk};

    use super::{JwtAlgorithm, KeySet, KeySetFailure, parse_key_set};
    use crate::test_metrics::Diagnostics;

    pub(crate) const JWT_SIGNING_DER: &[u8] =
        include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");

    /// Fixture signing uses jsonwebtoken, which needs its crypto provider;
    /// verification itself does not.
    pub(crate) fn ensure_crypto_provider() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }

    pub(crate) fn rsa_signing() -> EncodingKey {
        EncodingKey::from_rsa_der(JWT_SIGNING_DER)
    }

    pub(crate) fn ec_signing() -> EncodingKey {
        EncodingKey::from_ec_der(&rcgen::KeyPair::generate().unwrap().serialize_der())
    }

    pub(crate) fn jwk(
        signing: &EncodingKey,
        algorithm: Algorithm,
        kid: Option<&str>,
        alg: bool,
    ) -> serde_json::Value {
        let mut jwk =
            serde_json::to_value(Jwk::from_encoding_key(signing, algorithm).unwrap()).unwrap();
        let object = jwk.as_object_mut().unwrap();
        object.remove("kid");
        if !alg {
            object.remove("alg");
        }
        if let Some(kid) = kid {
            object.insert("kid".to_owned(), kid.into());
        }
        jwk
    }

    pub(crate) fn key_set(keys: &[serde_json::Value], configured: &[JwtAlgorithm]) -> Arc<KeySet> {
        ensure_crypto_provider();
        let bytes = serde_json::to_vec(&serde_json::json!({ "keys": keys })).unwrap();
        Arc::new(parse_key_set(&bytes, configured).unwrap())
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
    fn an_unreadable_or_empty_key_set_is_refused() {
        assert_eq!(
            parse_key_set(br#"{"keys":false}"#, &[JwtAlgorithm::Rs256]).err(),
            Some(KeySetFailure::Parse)
        );
        assert_eq!(
            parse_key_set(br#"{"keys":[]}"#, &[JwtAlgorithm::Rs256]).err(),
            Some(KeySetFailure::NoUsableKeys)
        );
    }

    #[test]
    fn rejected_entries_are_counted_per_reason_in_one_event_without_key_ids() {
        let diagnostics = Diagnostics::default();
        // Two independent registrations avoid tracing's single-dispatch shortcut
        // caching a sibling thread's NoSubscriber interest for shared callsites.
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = tracing::Dispatch::new(diagnostics.clone());
        // Unknown key parameters fall back to the library's Other family.
        // A wrongly typed consumed common field exercises malformed entries.
        let entries = vec![serde_json::json!({"kid":"private-key-id","use":false}); 100];
        let bytes = serde_json::to_vec(&serde_json::json!({"keys":entries})).unwrap();
        let parsed = metrics::with_local_recorder(&diagnostics, || {
            tracing::dispatcher::with_default(&dispatch, || {
                parse_key_set(&bytes, &[JwtAlgorithm::Rs256])
            })
        });
        assert_eq!(parsed.err(), Some(KeySetFailure::NoUsableKeys));
        let counters = diagnostics.counters.lock().unwrap();
        assert!(
            counters.iter().any(|(key, value)| {
                key.name() == "authn_jwks_key_rejections_total"
                    && *value == 100
                    && key
                        .labels()
                        .any(|label| label.key() == "reason" && label.value() == "malformed_entry")
            }),
            "recorded counters: {counters:?}"
        );
        let events = diagnostics.events.lock().unwrap();
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
