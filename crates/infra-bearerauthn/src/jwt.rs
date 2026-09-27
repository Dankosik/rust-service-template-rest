//! OIDC discovery, library-backed JWT verification, and key admission.

use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use aws_lc_rs::signature::{self, ParsedPublicKey, RsaPublicKeyComponents};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation,
    crypto::aws_lc::DEFAULT_PROVIDER,
    decode, decode_header,
    errors::ErrorKind,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    BearerToken, EndpointUrl, Failure, IssuerUrl, PreparationError, PreparationPhase,
    PreparationReason, Principal, VerificationError, VerificationReason, Verifier,
    claims::{ClaimPolicy, validate_jwt_claims},
    provider::ProviderClient,
    refresh::{KeyStore, UnknownKeyRefresh, run_refresh_worker},
};

/// Discovery and the initial JWKS fetch share this budget.
const STARTUP_BUDGET: Duration = Duration::from_secs(6);

/// JWT profile rules selected before verifier preparation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TokenProfile {
    #[default]
    ResourceServer,
    Rfc9068,
}

/// The closed JWT algorithms accepted by authentication configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum JwtAlgorithm {
    Rs256,
    Es256,
    Ps256,
    EdDsa,
}

/// Bootstrap input for OIDC discovery and JWT verification.
#[derive(Clone, Eq, PartialEq)]
pub struct JwtOptions {
    pub issuer: IssuerUrl,
    pub audiences: Vec<String>,
    pub token_profile: TokenProfile,
    pub algorithms: Vec<JwtAlgorithm>,
}

impl fmt::Debug for JwtOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JwtOptions([REDACTED])")
    }
}

/// The process-owned task that keeps a JWT verifier's installed key set fresh.
pub type RefreshTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

pub(crate) struct JwtVerifier {
    claim_policy: ClaimPolicy,
    token_profile: TokenProfile,
    algorithms: Vec<JwtAlgorithm>,
    keys: Arc<KeyStore>,
}

impl fmt::Debug for JwtVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JwtVerifier(..)")
    }
}

/// Why no installed key yielded a verified payload.
enum DecodeError {
    /// No installed key has the token's `kid` and algorithm.
    NoCandidate,
    /// Candidate keys exist, but none verified the signature.
    BadSignature,
    /// A key verified the signature and the library rejected the claims.
    Rejected(VerificationError),
}

impl DecodeError {
    /// A new `kid`, or a kid-less token during key rotation, can need keys the
    /// installed set lacks (the go-oidc rule). A known `kid` with a bad
    /// signature cannot.
    fn may_need_new_keys(&self, kid: Option<&str>) -> bool {
        match self {
            Self::NoCandidate => true,
            Self::BadSignature => kid.is_none(),
            Self::Rejected(_) => false,
        }
    }
}

impl From<DecodeError> for VerificationError {
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NoCandidate => Self::invalid(VerificationReason::UnknownKey),
            DecodeError::BadSignature => Self::invalid(VerificationReason::Signature),
            DecodeError::Rejected(error) => error,
        }
    }
}

impl JwtVerifier {
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
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
        let kid = header.kid.as_deref();
        let payload = match self.decode(&self.keys.keys(), token, kid, algorithm) {
            Ok(payload) => payload,
            Err(miss) if miss.may_need_new_keys(kid) => {
                self.decode_after_refresh(token, kid, algorithm, miss)
                    .await?
            }
            Err(error) => return Err(error.into()),
        };
        validate_jwt_claims(
            payload.get(),
            &self.claim_policy,
            self.token_profile,
            jsonwebtoken::get_current_timestamp(),
        )
    }

    /// Tries every installed key that fits the token; the first valid signature wins.
    fn decode(
        &self,
        keys: &KeySet,
        token: &BearerToken<'_>,
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
    ) -> Result<Box<RawValue>, DecodeError> {
        let validation = validation_for(algorithm, &self.claim_policy);
        let mut miss = DecodeError::NoCandidate;
        for key in keys.candidates(kid, algorithm) {
            match decode::<Box<RawValue>>(token.as_bytes(), &key.decoding_key, &validation) {
                Ok(data) => return Ok(data.claims),
                Err(error) if *error.kind() == ErrorKind::InvalidSignature => {
                    miss = DecodeError::BadSignature;
                }
                Err(error) => {
                    return Err(DecodeError::Rejected(jwt_validation_error(error.kind())));
                }
            }
        }
        Err(miss)
    }

    async fn decode_after_refresh(
        &self,
        token: &BearerToken<'_>,
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
        miss: DecodeError,
    ) -> Result<Box<RawValue>, VerificationError> {
        match self.keys.refresh_for_unknown_key().await {
            UnknownKeyRefresh::Refreshed(keys) => Ok(self.decode(&keys, token, kid, algorithm)?),
            UnknownKeyRefresh::StillUnknown => Err(miss.into()),
            UnknownKeyRefresh::Unavailable => Err(VerificationError::new(
                Failure::Unavailable,
                VerificationReason::Refresh,
            )),
        }
    }
}

fn jwt_validation_error(error: &ErrorKind) -> VerificationError {
    let reason = match error {
        ErrorKind::MissingRequiredClaim(_) => VerificationReason::MissingClaim,
        ErrorKind::ExpiredSignature => VerificationReason::Expired,
        ErrorKind::ImmatureSignature => VerificationReason::NotYetValid,
        ErrorKind::InvalidIssuer => VerificationReason::Issuer,
        ErrorKind::InvalidAudience => VerificationReason::Audience,
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
///
/// # Errors
///
/// Returns [`PreparationError`] when options, client preparation, discovery,
/// issuer agreement, or initial key admission fail.
pub async fn prepare_jwt(
    options: JwtOptions,
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
    crate::describe_verification();
    metrics::describe_counter!(
        "authn_jwks_key_rejections_total",
        "Rejected JWKS entries by closed reason"
    );
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
    let discovery = fetch_discovery(&provider, &options.issuer, startup_deadline).await?;
    if discovery.issuer != options.issuer.as_str() {
        return Err(PreparationError::issuer_mismatch(
            &options.issuer,
            &discovery.issuer,
        ));
    }
    let jwks_uri = EndpointUrl::parse(&discovery.jwks_uri).map_err(|_| {
        PreparationError::new(PreparationPhase::Discovery, PreparationReason::InvalidUrl)
    })?;
    let jwks_error = |reason| PreparationError::new(PreparationPhase::Jwks, reason);
    let bytes = tokio::time::timeout_at(startup_deadline, provider.get_json(jwks_uri.url()))
        .await
        .map_err(|_| jwks_error(PreparationReason::Fetch))?
        .map_err(|_| jwks_error(PreparationReason::Fetch))?;
    let keys = parse_key_set(&bytes, &options.algorithms).map_err(|error| {
        jwks_error(match error {
            KeySetError::Parse => PreparationReason::Parse,
            KeySetError::NoUsableKeys => PreparationReason::NoUsableKeys,
        })
    })?;
    let keys = KeyStore::new(Arc::new(keys));
    let verifier = JwtVerifier {
        claim_policy: ClaimPolicy::new(options.issuer.as_str().to_owned(), options.audiences),
        token_profile: options.token_profile,
        algorithms: options.algorithms.clone(),
        keys: keys.clone(),
    };
    let algorithms = options.algorithms;
    let refresh_cancel = cancel.child_token();
    let refresh_task: RefreshTask = Box::pin(async move {
        run_refresh_worker(keys, provider, jwks_uri, algorithms, refresh_cancel).await;
    });
    Ok((Verifier::jwt(verifier), refresh_task))
}

impl PreparationError {
    /// Discovery named another issuer. Both values are public issuer URLs; a
    /// discovered value outside the issuer grammar is not echoed.
    fn issuer_mismatch(configured: &IssuerUrl, discovered: &str) -> Self {
        let discovered = if discovered.len() <= 256 && IssuerUrl::parse(discovered).is_ok() {
            discovered.to_owned()
        } else {
            "<not an issuer URL>".to_owned()
        };
        Self::IssuerMismatch {
            configured: configured.as_str().to_owned(),
            discovered,
        }
    }
}

fn ensure_crypto_provider() -> Result<(), PreparationError> {
    match DEFAULT_PROVIDER.install_default() {
        Ok(()) => Ok(()),
        Err(installed) if std::ptr::eq(installed, &raw const DEFAULT_PROVIDER) => Ok(()),
        Err(_) => Err(PreparationError::new(
            PreparationPhase::Client,
            PreparationReason::Client,
        )),
    }
}

async fn fetch_discovery(
    provider: &ProviderClient,
    issuer: &IssuerUrl,
    deadline: Instant,
) -> Result<Discovery, PreparationError> {
    let error = |reason| PreparationError::new(PreparationPhase::Discovery, reason);
    let mut url = issuer.url().clone();
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    url.set_path(&format!("{path}/.well-known/openid-configuration"));
    let bytes = tokio::time::timeout_at(deadline, provider.get_json(&url))
        .await
        .map_err(|_| error(PreparationReason::Fetch))?
        .map_err(|_| error(PreparationReason::Fetch))?;
    serde_json::from_slice(&bytes).map_err(|_| error(PreparationReason::Parse))
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

fn is_access_token_type(value: &str) -> bool {
    value.eq_ignore_ascii_case("at+jwt") || value.eq_ignore_ascii_case("application/at+jwt")
}

struct JwtKey {
    kid: Option<String>,
    family: KeyFamily,
    /// The JWK's own `alg`; without one the key serves every configured
    /// algorithm of its family, as Nimbus and go-oidc do.
    algorithm: Option<JwtAlgorithm>,
    decoding_key: DecodingKey,
}

impl JwtKey {
    fn accepts(&self, algorithm: JwtAlgorithm) -> bool {
        self.algorithm
            .map_or(algorithm.matches(self.family), |bound| bound == algorithm)
    }
}

pub(crate) struct KeySet {
    keys: Vec<JwtKey>,
}

impl KeySet {
    /// Keys that fit the token: its algorithm and, when present, its `kid`.
    fn candidates<'a>(
        &'a self,
        kid: Option<&'a str>,
        algorithm: JwtAlgorithm,
    ) -> impl Iterator<Item = &'a JwtKey> {
        self.keys.iter().filter(move |key| {
            key.accepts(algorithm) && kid.is_none_or(|kid| key.kid.as_deref() == Some(kid))
        })
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

/// Admits every usable signature key and skips the rest.
pub(crate) fn parse_key_set(
    bytes: &[u8],
    configured: &[JwtAlgorithm],
) -> Result<KeySet, KeySetError> {
    let raw: RawJwks = serde_json::from_slice(bytes).map_err(|_| KeySetError::Parse)?;
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
        metrics::counter!("authn_jwks_key_rejections_total", "reason" => reason.label())
            .increment(count);
    }
    if keys.is_empty() {
        return Err(KeySetError::NoUsableKeys);
    }
    Ok(KeySet { keys })
}

fn admit_key(mut jwk: Jwk, configured: &[JwtAlgorithm]) -> Result<JwtKey, KeyRejection> {
    if !signature_usage(&jwk) {
        return Err(KeyRejection::IncompatibleUsage);
    }
    let family = KeyFamily::from_jwk(&jwk).ok_or(KeyRejection::UnsupportedFamily)?;
    let algorithm = match jwk.common.key_algorithm {
        Some(explicit) => Some(
            JwtAlgorithm::from_key_algorithm(explicit)
                .filter(|algorithm| configured.contains(algorithm) && algorithm.matches(family))
                .ok_or(KeyRejection::AlgorithmBinding)?,
        ),
        None if configured.iter().any(|algorithm| algorithm.matches(family)) => None,
        None => return Err(KeyRejection::AlgorithmBinding),
    };
    admit_material(&mut jwk).ok_or(KeyRejection::InvalidMaterial)?;
    let decoding_key = DecodingKey::from_jwk(&jwk).map_err(|_| KeyRejection::InvalidMaterial)?;
    Ok(JwtKey {
        kid: jwk.common.key_id,
        family,
        algorithm,
        decoding_key,
    })
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

    fn from_key_algorithm(algorithm: KeyAlgorithm) -> Option<Self> {
        match algorithm {
            KeyAlgorithm::RS256 => Some(Self::Rs256),
            KeyAlgorithm::PS256 => Some(Self::Ps256),
            KeyAlgorithm::ES256 => Some(Self::Es256),
            KeyAlgorithm::EdDSA => Some(Self::EdDsa),
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

/// Checks key material with aws-lc at admission, so an unusable entry is
/// skipped instead of failing every token that names it.
fn admit_material(jwk: &mut Jwk) -> Option<()> {
    match &mut jwk.algorithm {
        AlgorithmParameters::RSA(parameters) => {
            let modulus = without_leading_zeros(&URL_SAFE_NO_PAD.decode(&parameters.n).ok()?)?;
            let exponent = without_leading_zeros(&URL_SAFE_NO_PAD.decode(&parameters.e).ok()?)?;
            // aws-lc parsing does not bound the size; its verifiers accept 2048–8192 bits.
            let bits = modulus.len() * 8 - modulus[0].leading_zeros() as usize;
            if !(2048..=8192).contains(&bits) {
                return None;
            }
            RsaPublicKeyComponents {
                n: modulus.as_slice(),
                e: exponent.as_slice(),
            }
            .to_parsed_public_key(&signature::RSA_PKCS1_2048_8192_SHA256)
            .ok()?;
            // Some IdPs emit a leading zero byte; jsonwebtoken 11 with aws-lc then
            // reports every signature as invalid, so install the minimal encoding.
            parameters.n = URL_SAFE_NO_PAD.encode(&modulus);
            parameters.e = URL_SAFE_NO_PAD.encode(&exponent);
        }
        AlgorithmParameters::EllipticCurve(parameters) => {
            let x = URL_SAFE_NO_PAD.decode(&parameters.x).ok()?;
            let y = URL_SAFE_NO_PAD.decode(&parameters.y).ok()?;
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let point = [[4].as_slice(), &x, &y].concat();
            ParsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).ok()?;
        }
        AlgorithmParameters::OctetKeyPair(parameters) => {
            let x = URL_SAFE_NO_PAD.decode(&parameters.x).ok()?;
            ParsedPublicKey::new(&signature::ED25519, &x).ok()?;
        }
        _ => return None,
    }
    Some(())
}

fn without_leading_zeros(bytes: &[u8]) -> Option<Vec<u8>> {
    let start = bytes.iter().position(|byte| *byte != 0)?;
    Some(bytes[start..].to_vec())
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

    use super::{
        ClaimPolicy, JwtAlgorithm, JwtVerifier, KeyStore, TokenProfile, ensure_crypto_provider,
        parse_key_set,
    };
    use crate::{Failure, VerificationReason, parse_bearer, refresh::UnknownKeyRefresh};

    const JWT_SIGNING_DER: &[u8] = include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");

    fn rsa_signing() -> EncodingKey {
        EncodingKey::from_rsa_der(JWT_SIGNING_DER)
    }

    fn ec_signing() -> EncodingKey {
        EncodingKey::from_ec_der(&rcgen::KeyPair::generate().unwrap().serialize_der())
    }

    /// The public JWK of `signing`, with `kid` and `alg` set as given.
    fn jwk(
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

    fn key_set(keys: &[serde_json::Value], configured: &[JwtAlgorithm]) -> Arc<super::KeySet> {
        ensure_crypto_provider().unwrap();
        let bytes = serde_json::to_vec(&serde_json::json!({ "keys": keys })).unwrap();
        Arc::new(parse_key_set(&bytes, configured).unwrap())
    }

    fn rsa_key_set(kid: &str, algorithm: Option<Algorithm>) -> Arc<super::KeySet> {
        let key = jwk(
            &rsa_signing(),
            algorithm.unwrap_or(Algorithm::RS256),
            Some(kid),
            algorithm.is_some(),
        );
        key_set(&[key], &[JwtAlgorithm::Rs256])
    }

    fn verifier(keys: Arc<super::KeySet>, algorithms: &[JwtAlgorithm]) -> JwtVerifier {
        JwtVerifier {
            claim_policy: ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                vec!["api".to_owned()],
            ),
            token_profile: TokenProfile::ResourceServer,
            algorithms: algorithms.to_vec(),
            keys: KeyStore::new(keys),
        }
    }

    fn signed(
        signing: &EncodingKey,
        algorithm: Algorithm,
        kid: Option<&str>,
        extra: &serde_json::Value,
    ) -> String {
        let mut claims = serde_json::json!({
            "iss": "https://issuer.example", "aud": "api",
            "exp": jsonwebtoken::get_current_timestamp() + 60, "sub": "subject",
        });
        claims
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut header = Header::new(algorithm);
        header.kid = kid.map(ToOwned::to_owned);
        encode(&header, &claims, signing).unwrap()
    }

    async fn check(
        verifier: &JwtVerifier,
        token: &str,
    ) -> Result<crate::Principal, VerificationReason> {
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        verifier.verify(&token).await.map_err(|error| error.reason)
    }

    #[tokio::test]
    async fn a_jwk_without_alg_serves_every_configured_algorithm_of_its_family() {
        let both = [JwtAlgorithm::Rs256, JwtAlgorithm::Ps256];
        let keys = key_set(
            &[jwk(&rsa_signing(), Algorithm::RS256, Some("entra"), false)],
            &both,
        );
        let verifier = verifier(keys, &both);
        for algorithm in [Algorithm::RS256, Algorithm::PS256] {
            let token = signed(
                &rsa_signing(),
                algorithm,
                Some("entra"),
                &serde_json::json!({}),
            );
            check(&verifier, &token).await.unwrap();
        }
    }

    #[tokio::test]
    async fn an_explicit_jwk_alg_binds_the_key() {
        let both = [JwtAlgorithm::Rs256, JwtAlgorithm::Ps256];
        let keys = key_set(
            &[jwk(&rsa_signing(), Algorithm::RS256, Some("bound"), true)],
            &both,
        );
        let verifier = verifier(keys, &both);
        let token = signed(
            &rsa_signing(),
            Algorithm::PS256,
            Some("bound"),
            &serde_json::json!({}),
        );
        // The only key refuses PS256; the refresh answer is "still unknown".
        assert_eq!(
            check(&verifier, &token).await.unwrap_err(),
            VerificationReason::UnknownKey
        );
    }

    #[tokio::test]
    async fn kid_less_keys_are_tried_in_turn() {
        let (old, new) = (ec_signing(), ec_signing());
        let keys = key_set(
            &[
                jwk(&old, Algorithm::ES256, None, true),
                jwk(&new, Algorithm::ES256, None, true),
            ],
            &[JwtAlgorithm::Es256],
        );
        let verifier = verifier(keys, &[JwtAlgorithm::Es256]);
        for signing in [&old, &new] {
            let token = signed(signing, Algorithm::ES256, None, &serde_json::json!({}));
            check(&verifier, &token).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_kid_less_signature_miss_waits_for_one_refresh() {
        let (installed, rotated) = (ec_signing(), ec_signing());
        let verifier = Arc::new(verifier(
            key_set(
                &[jwk(&installed, Algorithm::ES256, None, true)],
                &[JwtAlgorithm::Es256],
            ),
            &[JwtAlgorithm::Es256],
        ));
        verifier.keys.permit_unknown_refresh_for_test();
        let token = signed(&rotated, Algorithm::ES256, None, &serde_json::json!({}));
        let call = tokio::spawn({
            let verifier = Arc::clone(&verifier);
            async move { check(&verifier, &token).await.map(|_| ()) }
        });
        tokio::task::yield_now().await;
        assert_eq!(verifier.keys.pending(), Some(1));
        let rotated_set = key_set(
            &[jwk(&rotated, Algorithm::ES256, None, true)],
            &[JwtAlgorithm::Es256],
        );
        verifier.keys.finish(1, Ok(rotated_set));
        assert_eq!(call.await.unwrap(), Ok(()));
        // Within the cooldown a second miss keeps the precise reason.
        let stale = signed(&installed, Algorithm::ES256, None, &serde_json::json!({}));
        assert_eq!(
            check(&verifier, &stale).await.unwrap_err(),
            VerificationReason::Signature
        );
        assert!(matches!(
            verifier.keys.refresh_for_unknown_key().await,
            UnknownKeyRefresh::StillUnknown
        ));
    }

    #[tokio::test]
    async fn a_known_kid_with_a_bad_signature_does_not_refresh() {
        let verifier = verifier(
            rsa_key_set("fixture", Some(Algorithm::RS256)),
            &[JwtAlgorithm::Rs256],
        );
        verifier.keys.permit_unknown_refresh_for_test();
        // Another token's signature is a real mismatch; flipping a base64
        // character can land in padding bits and leave the signature valid.
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let other = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({"x": 1}),
        );
        let (signed_part, _) = token.rsplit_once('.').unwrap();
        let (_, other_signature) = other.rsplit_once('.').unwrap();
        let token = format!("{signed_part}.{other_signature}");
        assert_eq!(
            check(&verifier, &token).await.unwrap_err(),
            VerificationReason::Signature
        );
        assert_eq!(verifier.keys.pending(), None);
    }

    #[tokio::test]
    async fn jsonwebtoken_owns_registered_claims_and_null_identity_is_absent() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        for (extra, expected) in [
            (
                serde_json::json!({"nbf": null}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"exp": null}),
                Err(VerificationReason::MissingClaim),
            ),
            (
                serde_json::json!({"sub": null, "azp": "client"}),
                Ok(Some("client")),
            ),
        ] {
            let token = signed(&rsa_signing(), Algorithm::RS256, Some("fixture"), &extra);
            let result = check(&verifier, &token).await;
            let client_id = result.as_ref().map(crate::Principal::client_id);
            assert_eq!(client_id.map_err(|error| *error), expected, "{extra}");
        }
    }

    #[tokio::test]
    async fn signed_payload_supplies_typed_custom_claims_without_changing_identity() {
        #[derive(serde::Deserialize)]
        struct ApplicationClaims {
            tenant: String,
            permissions: Vec<String>,
        }
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({"tenant": "verified-tenant", "permissions": ["read", "write"]}),
        );
        let principal = check(&verifier, &token).await.unwrap();
        let claims = principal.claims::<ApplicationClaims>().unwrap();
        assert_eq!(claims.tenant, "verified-tenant");
        assert_eq!(claims.permissions, ["read", "write"]);
        assert_eq!(principal.subject(), Some("subject"));
        let error = principal.claims::<Vec<String>>().unwrap_err();
        for rendered in [
            error.to_string(),
            format!("{error:?}"),
            format!("{principal:?}"),
        ] {
            assert!(!rendered.contains("verified-tenant"));
            assert!(!rendered.contains("permissions"));
        }
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

    #[tokio::test]
    async fn a_leading_zero_modulus_still_verifies() {
        let mut padded = jwk(&rsa_signing(), Algorithm::RS256, Some("padded"), true);
        let modulus = URL_SAFE_NO_PAD
            .decode(padded["n"].as_str().unwrap())
            .unwrap();
        padded["n"] = URL_SAFE_NO_PAD
            .encode([[0].as_slice(), &modulus].concat())
            .into();
        let verifier = verifier(
            key_set(&[padded], &[JwtAlgorithm::Rs256]),
            &[JwtAlgorithm::Rs256],
        );
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("padded"),
            &serde_json::json!({}),
        );
        check(&verifier, &token).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn unconfigured_algorithm_does_not_wait_for_refresh() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        verifier.keys.permit_unknown_refresh_for_test();
        let token = signed(
            &rsa_signing(),
            Algorithm::PS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        assert_eq!(
            check(&verifier, &token).await.unwrap_err(),
            VerificationReason::Algorithm
        );
        assert_eq!(verifier.keys.pending(), None);
    }

    #[test]
    fn preparation_errors_are_closed_and_an_issuer_mismatch_names_both_issuers() {
        use crate::{IssuerUrl, PreparationError, PreparationPhase, PreparationReason};
        assert_eq!(
            PreparationError::new(PreparationPhase::Jwks, PreparationReason::Fetch).to_string(),
            "authentication preparation failed during Jwks: Fetch"
        );
        let configured = IssuerUrl::parse("https://issuer.example").unwrap();
        let error = PreparationError::issuer_mismatch(&configured, "https://issuer.example/");
        assert_eq!(error.phase(), PreparationPhase::Discovery);
        assert_eq!(error.reason(), PreparationReason::IssuerMismatch);
        assert_eq!(
            error.to_string(),
            "authentication preparation failed during Discovery: IssuerMismatch \
             (configured issuer \"https://issuer.example\", \
             discovered issuer \"https://issuer.example/\")"
        );
        for discovered in [
            "https://issuer.example/?token=private".to_owned(),
            format!("https://{}.example", "a".repeat(256)),
            "not an issuer\nprivate".to_owned(),
        ] {
            let rendered = PreparationError::issuer_mismatch(&configured, &discovered).to_string();
            assert!(rendered.contains("<not an issuer URL>"), "{rendered}");
            assert!(!rendered.contains("private") && !rendered.contains("aaaa"));
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
        let diagnostics = Diagnostics::default();
        // Two independent registrations avoid tracing's single-dispatch shortcut
        // caching a sibling thread's NoSubscriber interest for shared callsites.
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = tracing::Dispatch::new(diagnostics.clone());
        let verifier = crate::Verifier::jwt(verifier(
            rsa_key_set("fixture", None),
            &[JwtAlgorithm::Rs256],
        ));
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({"sub":null,"jti":"private-claim-value"}),
        );
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        metrics::with_local_recorder(&diagnostics, || {
            tracing::dispatcher::with_default(&dispatch, || {
                assert_eq!(
                    runtime.block_on(verifier.verify(&token)),
                    Err(Failure::Invalid)
                );
                // Unknown key parameters fall back to the library's Other family.
                // A wrongly typed consumed common field exercises malformed entries.
                let entries = vec![serde_json::json!({"kid":"private-key-id","use":false}); 100];
                assert!(matches!(
                    parse_key_set(
                        &serde_json::to_vec(&serde_json::json!({"keys":entries})).unwrap(),
                        &[JwtAlgorithm::Rs256]
                    ),
                    Err(super::KeySetError::NoUsableKeys)
                ));
            });
        });
        let counters = diagnostics.counters.lock().unwrap();
        assert!(counters.iter().any(|(key, value)| {
            key.name() == "authn_token_verifications_total"
                && *value == 1
                && key
                    .labels()
                    .any(|label| label.key() == "reason" && label.value() == "missing_claim")
        }));
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
            events
                .iter()
                .any(|event| event.contains("authn_verification_failed")
                    && event.contains("missing_claim")),
            "recorded events: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|event| !event.contains("private-claim-value")
                    && !event.contains("private-key-id")),
            "recorded events: {events:?}"
        );
    }
}
