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
    PreparationReason, Principal, ProviderUrl, RefreshTask, TokenProfile, VerificationError,
    VerificationReason, Verifier,
    claims::{ClaimPolicy, JwtClaims, validate_jwt_claims},
    provider::{ProviderClient, ProviderDeadline},
    record_verification,
    refresh::{SharedRefresh, UnknownKeyResult, run_refresh_worker},
};

const STARTUP_BUDGET: Duration = Duration::from_secs(6);

#[derive(Clone)]
pub struct JwtVerifier {
    claim_policy: ClaimPolicy,
    token_profile: TokenProfile,
    algorithms: Vec<JwtAlgorithm>,
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
        record_verification("jwt", self.verify_evidence(token, deadline).await)
    }

    async fn verify_evidence(
        &self,
        token: &BearerToken<'_>,
        deadline: Instant,
    ) -> Result<Principal, VerificationError> {
        let header = decode_header(token.as_bytes())
            .map_err(|_| VerificationError::invalid(VerificationReason::Header))?;
        if header.crit.as_ref().is_some_and(|crit| !crit.is_empty()) {
            return Err(VerificationError::invalid(VerificationReason::Header));
        }
        let algorithm = JwtAlgorithm::from_jsonwebtoken(header.alg)
            .filter(|algorithm| self.algorithms.contains(algorithm))
            .ok_or_else(|| VerificationError::invalid(VerificationReason::Algorithm))?;
        if self.token_profile == TokenProfile::Rfc9068
            && !header.typ.as_deref().is_some_and(is_access_token_type)
        {
            return Err(VerificationError::invalid(VerificationReason::Profile));
        }
        let key = self
            .select_key(header.kid.as_deref(), algorithm, deadline)
            .await?;
        let claims = decode::<JwtClaims>(
            token.as_bytes(),
            &key.decoding_key,
            &validation_for(algorithm, &self.claim_policy),
        )
        .map_err(|error| jwt_validation_error(error.kind()))?
        .claims;
        validate_jwt_claims(
            claims,
            &self.claim_policy,
            self.token_profile,
            jsonwebtoken::get_current_timestamp(),
        )
    }

    async fn select_key(
        &self,
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
        deadline: Instant,
    ) -> Result<JwtKey, VerificationError> {
        let initial = self.refresh.keys();
        match initial.select(kid, algorithm) {
            KeySelection::One(key) => Ok(key.clone()),
            KeySelection::Ambiguous => {
                Err(VerificationError::invalid(VerificationReason::AmbiguousKey))
            }
            KeySelection::Unknown => match self.refresh.refresh_unknown(deadline).await {
                UnknownKeyResult::Refreshed => match self.refresh.keys().select(kid, algorithm) {
                    KeySelection::One(key) => Ok(key.clone()),
                    KeySelection::Unknown => {
                        Err(VerificationError::invalid(VerificationReason::UnknownKey))
                    }
                    KeySelection::Ambiguous => {
                        Err(VerificationError::invalid(VerificationReason::AmbiguousKey))
                    }
                },
                UnknownKeyResult::CooldownSuccess => {
                    Err(VerificationError::invalid(VerificationReason::UnknownKey))
                }
                UnknownKeyResult::RefreshFailed | UnknownKeyResult::CooldownFailure => Err(
                    VerificationError::new(Failure::Unavailable, VerificationReason::Refresh),
                ),
                UnknownKeyResult::DeadlineElapsed => Err(VerificationError::new(
                    Failure::Timeout,
                    VerificationReason::RequestTimeout,
                )),
            },
        }
    }
}

fn jwt_validation_error(error: &jsonwebtoken::errors::ErrorKind) -> VerificationError {
    use jsonwebtoken::errors::ErrorKind;
    let reason = match error {
        ErrorKind::MissingRequiredClaim(_) => VerificationReason::MissingClaim,
        ErrorKind::ExpiredSignature => VerificationReason::Expired,
        ErrorKind::ImmatureSignature => VerificationReason::NotYetValid,
        ErrorKind::InvalidIssuer => VerificationReason::Issuer,
        ErrorKind::InvalidAudience => VerificationReason::Audience,
        ErrorKind::InvalidSubject => VerificationReason::Identity,
        ErrorKind::InvalidAlgorithm
        | ErrorKind::InvalidAlgorithmName
        | ErrorKind::UnsupportedAlgorithm
        | ErrorKind::MissingAlgorithm => VerificationReason::Algorithm,
        ErrorKind::InvalidSignature => VerificationReason::Signature,
        _ => VerificationReason::MalformedClaims,
    };
    VerificationError::invalid(reason)
}

/// Prepares initial trust before traffic admission and returns the one refresh
/// future for bootstrap to spawn through its process tracker.
pub async fn prepare_jwt(
    options: JwtOptions,
    _tracker: TaskTracker,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), PreparationError> {
    ensure_crypto_provider()
        .map_err(|error| error.with_context(&options.issuer, &options.audiences, None))?;
    let provider = ProviderClient::new()
        .map_err(|error| error.with_context(&options.issuer, &options.audiences, None))?;
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
        return Err(
            PreparationError::new(PreparationPhase::Options, PreparationReason::Parse)
                .with_context(&options.issuer, &options.audiences, None),
        );
    }
    let startup_deadline = Instant::now() + STARTUP_BUDGET;
    let discovery_url = discovery_url(&options.issuer);
    let discovery = fetch_discovery(&provider, &discovery_url, startup_deadline)
        .await
        .map_err(|error| {
            error.with_context(
                &options.issuer,
                &options.audiences,
                Some(discovery_url.as_str()),
            )
        })?;
    if discovery.issuer != options.issuer.as_str() {
        return Err(PreparationError::new(
            PreparationPhase::Discovery,
            PreparationReason::IssuerMismatch,
        )
        .with_context(
            &options.issuer,
            &options.audiences,
            Some(discovery_url.as_str()),
        )
        .with_discovered_issuer(&discovery.issuer));
    }
    let jwks_uri = ProviderUrl::parse(&discovery.jwks_uri).map_err(|_| {
        PreparationError::new(PreparationPhase::Discovery, PreparationReason::InvalidUrl)
            .with_context(
                &options.issuer,
                &options.audiences,
                Some(&discovery.jwks_uri),
            )
    })?;
    let bytes = provider
        .get_json(
            jwks_uri.url(),
            startup_deadline_for(Instant::now(), startup_deadline, PreparationPhase::Jwks)
                .map_err(|error| {
                    error.with_context(&options.issuer, &options.audiences, Some(jwks_uri.as_str()))
                })?,
        )
        .await
        .map_err(|_| {
            PreparationError::new(PreparationPhase::Jwks, PreparationReason::Fetch).with_context(
                &options.issuer,
                &options.audiences,
                Some(jwks_uri.as_str()),
            )
        })?;
    let keys = parse_key_set(&bytes, &options.algorithms).map_err(|error| {
        PreparationError::new(
            PreparationPhase::Jwks,
            match error {
                KeySetError::Parse => PreparationReason::Parse,
                KeySetError::NoUsableKeys => PreparationReason::NoUsableKeys,
            },
        )
        .with_context(&options.issuer, &options.audiences, Some(jwks_uri.as_str()))
    })?;
    let refresh = SharedRefresh::new(Arc::new(keys));
    let verifier = JwtVerifier {
        claim_policy: ClaimPolicy::new(options.issuer.as_str().to_owned(), options.audiences),
        token_profile: options.token_profile,
        algorithms: options.algorithms.clone(),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KeySetError {
    Parse,
    NoUsableKeys,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum KeyRejection {
    MalformedEntry,
    IncompatibleUsage,
    UnsupportedFamily,
    AlgorithmBinding,
    InvalidMaterial,
    ConflictingAlgorithm,
}
impl KeyRejection {
    fn label(self) -> &'static str {
        match self {
            Self::MalformedEntry => "malformed_entry",
            Self::IncompatibleUsage => "incompatible_usage",
            Self::UnsupportedFamily => "unsupported_family",
            Self::AlgorithmBinding => "algorithm_binding",
            Self::InvalidMaterial => "invalid_material",
            Self::ConflictingAlgorithm => "conflicting_algorithm",
        }
    }
}

pub(crate) fn parse_key_set(
    bytes: &[u8],
    configured: &[JwtAlgorithm],
) -> Result<KeySet, KeySetError> {
    let raw: RawJwks = serde_json::from_slice(bytes).map_err(|_| KeySetError::Parse)?;
    let mut rejected = std::collections::BTreeMap::<KeyRejection, u64>::new();
    let mut candidates = Vec::new();
    for value in raw.keys {
        let candidate = serde_json::from_value::<Jwk>(value)
            .map_err(|_| KeyRejection::MalformedEntry)
            .and_then(|jwk| Candidate::admit(jwk, configured));
        match candidate {
            Ok(candidate) => candidates.push(candidate),
            Err(reason) => *rejected.entry(reason).or_default() += 1,
        }
    }
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
    if !conflicting.is_empty() {
        rejected.insert(KeyRejection::ConflictingAlgorithm, conflicting.len() as u64);
    }
    // Aggregate per-entry rejection evidence into one event per closed reason.
    // Neither provider-controlled entry count nor key identifiers multiply logs.
    metrics::describe_counter!(
        "authn_jwks_key_rejections_total",
        "Rejected JWKS entries by closed reason"
    );
    for (reason, count) in rejected {
        tracing::debug!(
            reason = reason.label(),
            count,
            "authn_jwks_entries_rejected"
        );
        metrics::counter!("authn_jwks_key_rejections_total", "reason" => reason.label())
            .increment(count);
    }
    candidates = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(index, candidate)| (!conflicting.contains(&index)).then_some(candidate))
        .collect();
    if candidates.is_empty() {
        return Err(KeySetError::NoUsableKeys);
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
    fn admit(mut jwk: Jwk, configured: &[JwtAlgorithm]) -> Result<Self, KeyRejection> {
        if !signature_usage(&jwk) {
            return Err(KeyRejection::IncompatibleUsage);
        }
        let family = KeyFamily::from_jwk(&jwk).ok_or(KeyRejection::UnsupportedFamily)?;
        let algorithm = bind_algorithm(jwk.common.key_algorithm, family, configured)
            .ok_or(KeyRejection::AlgorithmBinding)?;
        let material = admit_material(&jwk, algorithm).ok_or(KeyRejection::InvalidMaterial)?;
        if let (PublicMaterial::Rsa(modulus, exponent), AlgorithmParameters::RSA(parameters)) =
            (&material, &mut jwk.algorithm)
        {
            parameters.n = URL_SAFE_NO_PAD.encode(modulus);
            parameters.e = URL_SAFE_NO_PAD.encode(exponent);
        }
        let decoding_key =
            DecodingKey::from_jwk(&jwk).map_err(|_| KeyRejection::InvalidMaterial)?;
        Ok(Self {
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
            algorithms: vec![JwtAlgorithm::Rs256],
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
    #[tokio::test]
    async fn repair_regression_alias_only_client_identity_is_verified() {
        let verifier = JwtVerifier {
            claim_policy: ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            ),
            token_profile: TokenProfile::ResourceServer,
            algorithms: vec![JwtAlgorithm::Rs256],
            refresh: SharedRefresh::new(key_set("fixture")),
        };
        for alias in ["azp", "appid", "cid"] {
            let token = signed_token("fixture", serde_json::json!({"sub":null, alias:"client"}));
            let header = format!("Bearer {token}");
            let token = parse_bearer([header.as_bytes()], 32 * 1024).unwrap();
            let principal = verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await
                .unwrap();
            assert_eq!(principal.subject(), None);
            assert_eq!(principal.client_id(), Some("client"));
        }
        let token = signed_token(
            "fixture",
            serde_json::json!({"sub":null,"azp":"client","cid":"other"}),
        );
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()], 32 * 1024).unwrap();
        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                .await,
            Err(Failure::Invalid)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn repair_regression_unconfigured_algorithm_does_not_wait_for_refresh() {
        let refresh = SharedRefresh::new(key_set("fixture"));
        refresh.permit_unknown_refresh_for_test().await;
        let verifier = JwtVerifier {
            claim_policy: ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            ),
            token_profile: TokenProfile::ResourceServer,
            algorithms: vec![JwtAlgorithm::Rs256],
            refresh,
        };
        let token = encode(&Header::new(Algorithm::PS256), &serde_json::json!({"iss":"https://issuer.example","aud":"api","exp":jsonwebtoken::get_current_timestamp()+60,"sub":"subject"}), &EncodingKey::from_rsa_der(JWT_SIGNING_DER)).unwrap();
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()], 32 * 1024).unwrap();
        assert_eq!(
            verifier
                .verify(&token, Instant::now() + std::time::Duration::from_secs(10))
                .await,
            Err(Failure::Invalid)
        );
    }

    #[test]
    fn preparation_context_distinguishes_phases_and_sanitizes_discovery_evidence() {
        use crate::{PreparationError, PreparationPhase, PreparationReason, ProviderUrl};
        let issuer = ProviderUrl::parse("https://issuer.example").unwrap();
        let error = PreparationError::new(
            PreparationPhase::Discovery,
            PreparationReason::IssuerMismatch,
        )
        .with_context(
            &issuer,
            &["api".to_owned()],
            Some("https://issuer.example/.well-known/openid-configuration"),
        )
        .with_discovered_issuer("https://other.example");
        assert_eq!(error.issuer(), Some("https://issuer.example"));
        assert_eq!(error.discovered_issuer(), Some("https://other.example"));
        assert!(error.to_string().contains("https://other.example"));
        let unsafe_error =
            error.with_discovered_issuer("https://user:secret@other.example/?token=private");
        for rendered in [unsafe_error.to_string(), format!("{unsafe_error:?}")] {
            assert!(rendered.contains("unsafe_or_overlong_url"));
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains("private"));
        }
        assert!(matches!(
            parse_key_set(br#"{"keys":false}"#, &[JwtAlgorithm::Rs256]),
            Err(super::KeySetError::Parse)
        ));
        assert!(matches!(
            parse_key_set(br#"{"keys":[]}"#, &[JwtAlgorithm::Rs256]),
            Err(super::KeySetError::NoUsableKeys)
        ));
    }

    #[derive(Clone, Default)]
    struct Diagnostics {
        counters: Arc<std::sync::Mutex<Vec<(metrics::Key, u64)>>>,
        events: Arc<std::sync::Mutex<Vec<String>>>,
    }
    struct RecordedCounter {
        key: metrics::Key,
        diagnostics: Diagnostics,
    }
    impl metrics::CounterFn for RecordedCounter {
        fn increment(&self, value: u64) {
            self.diagnostics
                .counters
                .lock()
                .unwrap()
                .push((self.key.clone(), value));
        }
        fn absolute(&self, value: u64) {
            self.increment(value);
        }
    }
    impl metrics::Recorder for Diagnostics {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn register_counter(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            metrics::Counter::from_arc(Arc::new(RecordedCounter {
                key: key.clone(),
                diagnostics: self.clone(),
            }))
        }
        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }
        fn register_histogram(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::noop()
        }
    }
    impl tracing::Subscriber for Diagnostics {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Fields(String);
            impl tracing::field::Visit for Fields {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    use std::fmt::Write;
                    write!(self.0, "{}={value:?};", field.name()).unwrap();
                }
            }
            let mut fields = Fields(String::new());
            event.record(&mut fields);
            self.events.lock().unwrap().push(fields.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn diagnostics_report_closed_reasons_without_per_entry_log_amplification() {
        let verifier = JwtVerifier {
            claim_policy: ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            ),
            token_profile: TokenProfile::ResourceServer,
            algorithms: vec![JwtAlgorithm::Rs256],
            refresh: SharedRefresh::new(key_set("fixture")),
        };
        let token = signed_token(
            "fixture",
            serde_json::json!({"sub":null,"jti":"private-claim-value"}),
        );
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()], 32 * 1024).unwrap();
        let diagnostics = Diagnostics::default();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        metrics::with_local_recorder(&diagnostics, || {
            tracing::subscriber::with_default(diagnostics.clone(), || {
                assert_eq!(
                    runtime.block_on(
                        verifier.verify(&token, Instant::now() + std::time::Duration::from_secs(1))
                    ),
                    Err(Failure::Invalid)
                );
                let entries = vec![serde_json::json!({"kid":"private-key-id","kty":false}); 100];
                assert!(matches!(
                    parse_key_set(
                        &serde_json::to_vec(&serde_json::json!({"keys":entries})).unwrap(),
                        &[JwtAlgorithm::Rs256]
                    ),
                    Err(super::KeySetError::NoUsableKeys)
                ));
            })
        });
        let counters = diagnostics.counters.lock().unwrap();
        assert!(counters.iter().any(|(key, value)| {
            key.name() == "authn_verifications_total"
                && *value == 1
                && key
                    .labels()
                    .any(|label| label.key() == "reason" && label.value() == "missing_claim")
        }));
        assert!(counters.iter().any(|(key, value)| {
            key.name() == "authn_jwks_key_rejections_total"
                && *value == 100
                && key
                    .labels()
                    .any(|label| label.key() == "reason" && label.value() == "malformed_entry")
        }));
        let events = diagnostics.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains("authn_jwks_entries_rejected"))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .any(|event| event.contains("authn_verification_failed")
                    && event.contains("missing_claim"))
        );
        assert!(events.iter().all(
            |event| !event.contains("private-claim-value") && !event.contains("private-key-id")
        ));
    }
}
