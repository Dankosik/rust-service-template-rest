//! OIDC discovery, library-backed JWT verification, and key admission.

use std::{fmt, sync::Arc, time::Duration};

use aws_lc_rs::signature::{self, ParsedPublicKey, RsaPublicKeyComponents};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation,
    crypto::aws_lc::DEFAULT_PROVIDER,
    decode, decode_header,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;
use tokio::time::Instant;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    BearerToken, Failure, JwtAlgorithm, JwtOptions, PreparationError, PreparationPhase,
    PreparationReason, Principal, ProviderUrl, RefreshTask, TokenProfile, Verifier,
    claims::{ClaimPolicy, JwtClaims, validate_jwt_claims},
    provider::{ProviderClient, ProviderDeadline},
    refresh::{SharedRefresh, UnknownKeyResult, run_refresh_worker},
};

const STARTUP_BUDGET: Duration = Duration::from_secs(6);

#[derive(Clone)]
pub struct JwtVerifier {
    claim_policy: ClaimPolicy,
    token_profile: TokenProfile,
    refresh: Arc<SharedRefresh>,
}

impl fmt::Debug for JwtVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JwtVerifier(..)")
    }
}

impl JwtVerifier {
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
        deadline: Instant,
    ) -> Result<Principal, Failure> {
        let header = decode_header(token.as_bytes()).map_err(|_| Failure::Invalid)?;
        if header.crit.as_ref().is_some_and(|crit| !crit.is_empty()) {
            return Err(Failure::Invalid);
        }
        let algorithm = JwtAlgorithm::from_jsonwebtoken(header.alg).ok_or(Failure::Invalid)?;
        if self.token_profile == TokenProfile::Rfc9068
            && !header.typ.as_deref().is_some_and(is_access_token_type)
        {
            return Err(Failure::Invalid);
        }
        let key = self
            .select_key(header.kid.as_deref(), algorithm, deadline)
            .await?;
        let mut validation = validation_for(algorithm, &self.claim_policy);
        validation.validate_nbf = true;
        let claims = decode::<JwtClaims>(token.as_bytes(), &key.decoding_key, &validation)
            .map_err(|_| Failure::Invalid)?
            .claims;
        let now = jsonwebtoken::get_current_timestamp();
        validate_jwt_claims(claims, &self.claim_policy, self.token_profile, now)
    }

    async fn select_key(
        &self,
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
        deadline: Instant,
    ) -> Result<JwtKey, Failure> {
        let initial = self.refresh.keys();
        match initial.select(kid, algorithm) {
            KeySelection::One(key) => Ok(key.clone()),
            KeySelection::Ambiguous => Err(Failure::Invalid),
            KeySelection::Unknown => match self.refresh.refresh_unknown(deadline).await {
                UnknownKeyResult::Refreshed => match self.refresh.keys().select(kid, algorithm) {
                    KeySelection::One(key) => Ok(key.clone()),
                    KeySelection::Unknown | KeySelection::Ambiguous => Err(Failure::Invalid),
                },
                UnknownKeyResult::CooldownSuccess => Err(Failure::Invalid),
                UnknownKeyResult::RefreshFailed | UnknownKeyResult::CooldownFailure => {
                    Err(Failure::Unavailable)
                }
                UnknownKeyResult::DeadlineElapsed => Err(Failure::Timeout),
            },
        }
    }
}

/// Prepares initial trust before traffic admission and returns the one refresh
/// future for bootstrap to spawn through its process tracker.
pub async fn prepare_jwt(
    options: JwtOptions,
    _tracker: TaskTracker,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), PreparationError> {
    ensure_crypto_provider()?;
    let provider = ProviderClient::new()?;
    prepare_with_provider(options, provider, cancel).await
}

async fn prepare_with_provider(
    options: JwtOptions,
    provider: ProviderClient,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), PreparationError> {
    if options.audiences.is_empty()
        || options.audiences.iter().any(String::is_empty)
        || options.algorithms.is_empty()
    {
        return Err(PreparationError::new(
            PreparationPhase::Options,
            PreparationReason::Parse,
        ));
    }
    let startup_deadline = Instant::now() + STARTUP_BUDGET;
    let discovery_url = discovery_url(&options.issuer);
    let discovery = fetch_discovery(&provider, &discovery_url, startup_deadline).await?;
    if discovery.issuer != options.issuer.as_str() {
        return Err(PreparationError::new(
            PreparationPhase::Discovery,
            PreparationReason::IssuerMismatch,
        ));
    }
    let jwks_uri = ProviderUrl::parse(&discovery.jwks_uri).map_err(|_| {
        PreparationError::new(PreparationPhase::Discovery, PreparationReason::InvalidUrl)
    })?;
    let bytes = provider
        .get_json(
            jwks_uri.url(),
            startup_deadline_for(Instant::now(), startup_deadline, PreparationPhase::Jwks)?,
        )
        .await
        .map_err(|_| PreparationError::new(PreparationPhase::Jwks, PreparationReason::Fetch))?;
    let keys = parse_key_set(&bytes, &options.algorithms).map_err(|_| {
        PreparationError::new(PreparationPhase::Jwks, PreparationReason::NoUsableKeys)
    })?;
    let refresh = SharedRefresh::new(Arc::new(keys));
    let verifier = JwtVerifier {
        claim_policy: ClaimPolicy::new(options.issuer.as_str().to_owned(), options.audiences),
        token_profile: options.token_profile,
        refresh: refresh.clone(),
    };
    let algorithms = options.algorithms;
    let refresh_cancel = cancel.child_token();
    let refresh_task: RefreshTask = Box::pin(async move {
        run_refresh_worker(refresh, provider, jwks_uri, algorithms, refresh_cancel).await;
    });
    Ok((Verifier::Jwt(verifier), refresh_task))
}

fn ensure_crypto_provider() -> Result<(), PreparationError> {
    match DEFAULT_PROVIDER.install_default() {
        Ok(()) => Ok(()),
        Err(installed) if std::ptr::eq(installed, &DEFAULT_PROVIDER) => Ok(()),
        Err(_) => Err(PreparationError::new(
            PreparationPhase::Client,
            PreparationReason::Client,
        )),
    }
}

fn discovery_url(issuer: &ProviderUrl) -> url::Url {
    let mut url = issuer.url().clone();
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    url.set_path(&format!("{path}/.well-known/openid-configuration"));
    url
}

async fn fetch_discovery(
    provider: &ProviderClient,
    uri: &url::Url,
    startup_deadline: Instant,
) -> Result<Discovery, PreparationError> {
    let bytes = provider
        .get_json(
            uri,
            startup_deadline_for(
                Instant::now(),
                startup_deadline,
                PreparationPhase::Discovery,
            )?,
        )
        .await
        .map_err(|_| {
            PreparationError::new(PreparationPhase::Discovery, PreparationReason::Fetch)
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| PreparationError::new(PreparationPhase::Discovery, PreparationReason::Parse))
}

fn startup_deadline_for(
    now: Instant,
    overall: Instant,
    phase: PreparationPhase,
) -> Result<ProviderDeadline, PreparationError> {
    ProviderDeadline::startup(now, overall)
        .ok_or(PreparationError::new(phase, PreparationReason::Fetch))
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

fn is_access_token_type(value: &str) -> bool {
    value.eq_ignore_ascii_case("at+jwt") || value.eq_ignore_ascii_case("application/at+jwt")
}

#[derive(Clone)]
struct JwtKey {
    kid: Option<String>,
    algorithm: JwtAlgorithm,
    decoding_key: DecodingKey,
}

pub(crate) struct KeySet {
    keys: Vec<JwtKey>,
}

impl KeySet {
    fn select(&self, kid: Option<&str>, algorithm: JwtAlgorithm) -> KeySelection<'_> {
        let matching = self
            .keys
            .iter()
            .filter(|key| {
                key.algorithm == algorithm && kid.is_none_or(|kid| key.kid.as_deref() == Some(kid))
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [key] => KeySelection::One(key),
            [] => KeySelection::Unknown,
            _ => KeySelection::Ambiguous,
        }
    }

    #[cfg(test)]
    pub(crate) fn has_kid(&self, kid: &str) -> bool {
        self.keys.iter().any(|key| key.kid.as_deref() == Some(kid))
    }
}

enum KeySelection<'a> {
    One(&'a JwtKey),
    Unknown,
    Ambiguous,
}

#[derive(Deserialize)]
struct RawJwks {
    keys: Vec<serde_json::Value>,
}

pub(crate) fn parse_key_set(bytes: &[u8], configured: &[JwtAlgorithm]) -> Result<KeySet, Failure> {
    let raw: RawJwks = serde_json::from_slice(bytes).map_err(|_| Failure::Unavailable)?;
    let mut candidates = raw
        .keys
        .into_iter()
        .filter_map(|value| {
            serde_json::from_value::<Jwk>(value)
                .ok()
                .and_then(|jwk| Candidate::admit(jwk, configured))
        })
        .collect::<Vec<_>>();
    let conflicting = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            candidates
                .iter()
                .enumerate()
                .any(|(other, item)| {
                    index != other
                        && candidate.material == item.material
                        && candidate.key.algorithm != item.key.algorithm
                })
                .then_some(index)
        })
        .collect::<std::collections::BTreeSet<_>>();
    candidates = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, candidate)| (!conflicting.contains(&index)).then_some(candidate))
        .collect();
    if candidates.is_empty() {
        return Err(Failure::Unavailable);
    }
    Ok(KeySet {
        keys: candidates
            .into_iter()
            .map(|candidate| candidate.key)
            .collect(),
    })
}

struct Candidate {
    key: JwtKey,
    material: PublicMaterial,
}

#[derive(Eq, PartialEq)]
enum PublicMaterial {
    Rsa(Vec<u8>, Vec<u8>),
    Ec(Vec<u8>, Vec<u8>),
    Ed(Vec<u8>),
}

impl Candidate {
    fn admit(jwk: Jwk, configured: &[JwtAlgorithm]) -> Option<Self> {
        if !signature_usage(&jwk) {
            return None;
        }
        let family = KeyFamily::from_jwk(&jwk)?;
        let algorithm = bind_algorithm(jwk.common.key_algorithm, family, configured)?;
        let material = admit_material(&jwk, algorithm)?;
        let decoding_key = DecodingKey::from_jwk(&jwk).ok()?;
        Some(Self {
            key: JwtKey {
                kid: jwk.common.key_id.clone(),
                algorithm,
                decoding_key,
            },
            material,
        })
    }
}

fn signature_usage(jwk: &Jwk) -> bool {
    matches!(
        jwk.common.public_key_use,
        None | Some(PublicKeyUse::Signature)
    ) && jwk.common.key_operations.as_ref().is_none_or(|operations| {
        !operations.is_empty()
            && operations
                .iter()
                .all(|operation| matches!(operation, KeyOperations::Verify))
    })
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

fn bind_algorithm(
    explicit: Option<KeyAlgorithm>,
    family: KeyFamily,
    configured: &[JwtAlgorithm],
) -> Option<JwtAlgorithm> {
    if let Some(explicit) = explicit {
        let algorithm = match explicit {
            KeyAlgorithm::RS256 => JwtAlgorithm::Rs256,
            KeyAlgorithm::PS256 => JwtAlgorithm::Ps256,
            KeyAlgorithm::ES256 => JwtAlgorithm::Es256,
            KeyAlgorithm::EdDSA => JwtAlgorithm::EdDsa,
            _ => return None,
        };
        return (configured.contains(&algorithm) && algorithm.matches(family)).then_some(algorithm);
    }
    let candidates = configured
        .iter()
        .copied()
        .filter(|algorithm| algorithm.matches(family))
        .collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates[0])
}

impl JwtAlgorithm {
    fn from_jsonwebtoken(algorithm: Algorithm) -> Option<Self> {
        match algorithm {
            Algorithm::RS256 => Some(Self::Rs256),
            Algorithm::PS256 => Some(Self::Ps256),
            Algorithm::ES256 => Some(Self::Es256),
            Algorithm::EdDSA => Some(Self::EdDsa),
            _ => None,
        }
    }

    fn jsonwebtoken(self) -> Algorithm {
        match self {
            Self::Rs256 => Algorithm::RS256,
            Self::Ps256 => Algorithm::PS256,
            Self::Es256 => Algorithm::ES256,
            Self::EdDsa => Algorithm::EdDSA,
        }
    }

    fn matches(self, family: KeyFamily) -> bool {
        matches!(
            (self, family),
            (Self::Rs256 | Self::Ps256, KeyFamily::Rsa)
                | (Self::Es256, KeyFamily::P256)
                | (Self::EdDsa, KeyFamily::Ed25519)
        )
    }
}

fn admit_material(jwk: &Jwk, algorithm: JwtAlgorithm) -> Option<PublicMaterial> {
    match &jwk.algorithm {
        AlgorithmParameters::RSA(parameters) => {
            let modulus = normalized_positive(URL_SAFE_NO_PAD.decode(&parameters.n).ok()?)?;
            let exponent = normalized_positive(URL_SAFE_NO_PAD.decode(&parameters.e).ok()?)?;
            let bits = (modulus.len() - 1) * 8 + (8 - modulus[0].leading_zeros() as usize);
            if !(2048..=8192).contains(&bits) {
                return None;
            }
            let params = match algorithm {
                JwtAlgorithm::Rs256 => &signature::RSA_PKCS1_2048_8192_SHA256,
                JwtAlgorithm::Ps256 => &signature::RSA_PSS_2048_8192_SHA256,
                _ => return None,
            };
            RsaPublicKeyComponents {
                n: modulus.as_slice(),
                e: exponent.as_slice(),
            }
            .to_parsed_public_key(params)
            .ok()?;
            Some(PublicMaterial::Rsa(modulus, exponent))
        }
        AlgorithmParameters::EllipticCurve(parameters) => {
            if algorithm != JwtAlgorithm::Es256 {
                return None;
            }
            let x = URL_SAFE_NO_PAD.decode(&parameters.x).ok()?;
            let y = URL_SAFE_NO_PAD.decode(&parameters.y).ok()?;
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let mut point = Vec::with_capacity(65);
            point.push(4);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            ParsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).ok()?;
            Some(PublicMaterial::Ec(x, y))
        }
        AlgorithmParameters::OctetKeyPair(parameters) => {
            if algorithm != JwtAlgorithm::EdDsa {
                return None;
            }
            let x = URL_SAFE_NO_PAD.decode(&parameters.x).ok()?;
            if x.len() != 32 {
                return None;
            }
            ParsedPublicKey::new(&signature::ED25519, &x).ok()?;
            Some(PublicMaterial::Ed(x))
        }
        _ => None,
    }
}

fn normalized_positive(mut bytes: Vec<u8>) -> Option<Vec<u8>> {
    while bytes.first() == Some(&0) {
        bytes.remove(0);
    }
    (!bytes.is_empty()).then_some(bytes)
}

fn validation_for(algorithm: JwtAlgorithm, policy: &ClaimPolicy) -> Validation {
    let mut validation = Validation::new(algorithm.jsonwebtoken());
    validation.set_required_spec_claims(&["iss", "aud", "exp"]);
    validation.set_issuer(&[policy.issuer()]);
    validation.set_audience(policy.audiences());
    validation.leeway = 30;
    validation.validate_nbf = true;
    validation
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};
    use tokio::time::Instant;

    use super::{
        ClaimPolicy, JwtAlgorithm, JwtVerifier, KeyFamily, SharedRefresh, TokenProfile,
        bind_algorithm, ensure_crypto_provider, parse_key_set,
    };
    use crate::{Failure, parse_bearer};

    const JWT_SIGNING_DER: &[u8] = include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");

    fn key_set(kid: &str) -> Arc<super::KeySet> {
        ensure_crypto_provider().unwrap();
        let signing = EncodingKey::from_rsa_der(JWT_SIGNING_DER);
        let mut jwk = Jwk::from_encoding_key(&signing, Algorithm::RS256).unwrap();
        jwk.common.key_id = Some(kid.to_owned());
        Arc::new(
            parse_key_set(
                serde_json::to_vec(&serde_json::json!({"keys": [jwk]}))
                    .unwrap()
                    .as_slice(),
                &[JwtAlgorithm::Rs256],
            )
            .unwrap(),
        )
    }

    fn signed_token(kid: &str, extra: serde_json::Value) -> String {
        let mut claims = serde_json::json!({
            "iss": "https://issuer.example", "aud": "api",
            "exp": jsonwebtoken::get_current_timestamp() + 60, "sub": "subject",
        });
        claims
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        encode(
            &header,
            &claims,
            &EncodingKey::from_rsa_der(JWT_SIGNING_DER),
        )
        .unwrap()
    }

    #[test]
    fn omitted_jwk_algorithm_requires_one_configured_family_match() {
        assert_eq!(
            bind_algorithm(None, KeyFamily::Rsa, &[JwtAlgorithm::Rs256]),
            Some(JwtAlgorithm::Rs256)
        );
        assert_eq!(
            bind_algorithm(
                None,
                KeyFamily::Rsa,
                &[JwtAlgorithm::Rs256, JwtAlgorithm::Ps256]
            ),
            None
        );
    }

    #[tokio::test]
    async fn library_decode_rejects_bad_signatures_and_typed_null_nbf() {
        let verifier = JwtVerifier {
            claim_policy: ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            ),
            token_profile: TokenProfile::ResourceServer,
            refresh: SharedRefresh::new(key_set("fixture")),
        };
        let token = signed_token("fixture", serde_json::json!({"nbf": null}));
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()], 32 * 1024).unwrap();
        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await,
            Err(Failure::Invalid)
        );

        let token = signed_token("fixture", serde_json::json!({}));
        let mut bytes = token.into_bytes();
        *bytes.last_mut().unwrap() = if *bytes.last().unwrap() == b'A' {
            b'B'
        } else {
            b'A'
        };
        let header = format!("Bearer {}", String::from_utf8(bytes).unwrap());
        let token = parse_bearer([header.as_bytes()], 32 * 1024).unwrap();
        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await,
            Err(Failure::Invalid)
        );
    }

    #[test]
    fn mixed_jwks_skips_a_backend_rejected_ec_point_without_losing_usable_material() {
        ensure_crypto_provider().unwrap();
        let signing = EncodingKey::from_rsa_der(JWT_SIGNING_DER);
        let mut valid = Jwk::from_encoding_key(&signing, Algorithm::RS256).unwrap();
        valid.common.key_id = Some("usable".to_owned());
        let invalid_point = URL_SAFE_NO_PAD.encode([0_u8; 32]);
        let jwks = serde_json::json!({"keys": [
            valid,
            {"kty": "EC", "crv": "P-256", "kid": "bad-point", "alg": "ES256", "x": invalid_point, "y": invalid_point}
        ]});
        let keys = parse_key_set(
            &serde_json::to_vec(&jwks).unwrap(),
            &[JwtAlgorithm::Rs256, JwtAlgorithm::Es256],
        )
        .unwrap();
        assert!(keys.has_kid("usable"));
        assert!(!keys.has_kid("bad-point"));
    }
}
