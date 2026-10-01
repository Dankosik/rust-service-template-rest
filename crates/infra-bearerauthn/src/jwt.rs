//! OIDC discovery, JWS verification with pre-parsed aws-lc keys, and key admission.

use std::{fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use aws_lc_rs::signature::{self, ParsedPublicKey, RsaParameters, RsaPublicKeyComponents};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{
    Algorithm, Header,
    jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, KeyOperations, PublicKeyUse},
};
use serde::Deserialize;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    BearerToken, EndpointUrl, Failure, IssuerUrl, PreparationError, PreparationPhase,
    PreparationReason, Principal, ProviderFailure, VerificationError, VerificationReason, Verifier,
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
    /// The key set location. When set, discovery is skipped; a token's `iss`
    /// is still compared with `issuer` exactly.
    pub jwks_uri: Option<EndpointUrl>,
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

/// Why no installed key verified the signature.
enum SignatureVerificationError {
    /// No installed key has the token's `kid` and algorithm.
    NoCandidate,
    /// Candidate keys exist, but none verified the signature.
    BadSignature,
}

impl SignatureVerificationError {
    /// A new `kid`, or a kid-less token during key rotation, can need keys the
    /// installed set lacks (the go-oidc rule). A known `kid` with a bad
    /// signature cannot.
    fn may_need_new_keys(&self, kid: Option<&str>) -> bool {
        match self {
            Self::NoCandidate => true,
            Self::BadSignature => kid.is_none(),
        }
    }
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

impl JwtVerifier {
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
    ) -> Result<Principal, VerificationError> {
        let malformed = || VerificationError::invalid(VerificationReason::Header);
        let bytes = token.as_bytes();
        let (message, signature) = split_last_dot(bytes).ok_or_else(malformed)?;
        let (header, payload) = split_last_dot(message).ok_or_else(malformed)?;
        let header = URL_SAFE_NO_PAD.decode(header).map_err(|_| malformed())?;
        // jsonwebtoken's `Header` keeps the library's header admission rules.
        let header: Header = serde_json::from_slice(&header).map_err(|_| malformed())?;
        if header.crit.as_ref().is_some_and(|crit| !crit.is_empty()) {
            return Err(malformed());
        }
        let algorithm = JwtAlgorithm::from_jsonwebtoken(header.alg)
            .filter(|algorithm| self.algorithms.contains(algorithm))
            .ok_or_else(|| VerificationError::invalid(VerificationReason::Algorithm))?;
        let typ = header.typ.as_deref();
        let admitted_type = match self.token_profile {
            TokenProfile::Rfc9068 => typ.is_some_and(is_access_token_type),
            // An untyped or generic JWT may be an access token. One explicitly
            // typed as another kind of JWT is not (RFC 8725 section 3.11).
            TokenProfile::ResourceServer => {
                typ.is_none_or(|typ| is_generic_jwt_type(typ) || is_access_token_type(typ))
            }
        };
        if !admitted_type {
            return Err(VerificationError::invalid(VerificationReason::Profile));
        }
        // Decode into bounded stack storage; admitted keys cap the signature size.
        // Keep `message` as the original encoded bytes that the issuer signed.
        let mut signature_bytes = [0_u8; MAX_SIGNATURE_BYTES];
        let signature = URL_SAFE_NO_PAD
            .decode_slice(signature, &mut signature_bytes)
            .map(|length| &signature_bytes[..length])
            .map_err(|_| VerificationError::invalid(VerificationReason::Signature))?;
        let kid = header.kid.as_deref();
        match self.keys.keys().verify(message, signature, kid, algorithm) {
            Ok(()) => {}
            Err(miss) if miss.may_need_new_keys(kid) => {
                self.verify_signature_after_refresh(message, signature, kid, algorithm, miss)
                    .await?;
            }
            Err(error) => return Err(error.into()),
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| VerificationError::invalid(VerificationReason::MalformedClaims))?;
        validate_jwt_claims(
            payload,
            &self.claim_policy,
            self.token_profile,
            jsonwebtoken::get_current_timestamp(),
            token.access_token(),
        )
    }

    /// Retries only the signature check; the caller validates the payload once.
    async fn verify_signature_after_refresh(
        &self,
        message: &[u8],
        signature: &[u8],
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
        miss: SignatureVerificationError,
    ) -> Result<(), VerificationError> {
        match self.keys.refresh_for_unknown_key().await {
            UnknownKeyRefresh::Refreshed(keys) => {
                Ok(keys.verify(message, signature, kid, algorithm)?)
            }
            UnknownKeyRefresh::StillUnknown => Err(miss.into()),
            UnknownKeyRefresh::Unavailable => Err(VerificationError::new(
                Failure::Unavailable,
                VerificationReason::Refresh,
            )),
        }
    }
}

fn split_last_dot(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let dot = bytes.iter().rposition(|byte| *byte == b'.')?;
    Some((&bytes[..dot], &bytes[dot + 1..]))
}

/// The largest signature an admitted key produces: RSA-8192.
const MAX_SIGNATURE_BYTES: usize = 1024;

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
    let claim_policy = ClaimPolicy::new(options.issuer.as_str().to_owned(), options.audiences)?;
    if options.algorithms.is_empty() {
        return Err(PreparationError::new(
            PreparationPhase::Options,
            PreparationReason::Parse,
        ));
    }
    let startup_deadline = Instant::now() + STARTUP_BUDGET;
    let jwks_uri = match options.jwks_uri {
        Some(jwks_uri) => jwks_uri,
        None => discover_jwks_uri(&provider, &options.issuer, startup_deadline).await?,
    };
    let jwks_error = |reason| PreparationError::new(PreparationPhase::Jwks, reason);
    let bytes = fetch_within(&provider, jwks_uri.url(), startup_deadline)
        .await
        .map_err(|failure| jwks_error(PreparationReason::Fetch(failure)))?;
    let keys = parse_key_set(&bytes, &options.algorithms).map_err(|error| {
        jwks_error(match error {
            KeySetError::Parse => PreparationReason::Parse,
            KeySetError::NoUsableKeys => PreparationReason::NoUsableKeys,
        })
    })?;
    let keys = KeyStore::new(Arc::new(keys));
    let verifier = JwtVerifier {
        claim_policy,
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

/// Reads the issuer's metadata and returns the key set location it names.
async fn discover_jwks_uri(
    provider: &ProviderClient,
    issuer: &IssuerUrl,
    deadline: Instant,
) -> Result<EndpointUrl, PreparationError> {
    let discovery = fetch_discovery(provider, issuer, deadline).await?;
    if discovery.issuer != issuer.as_str() {
        return Err(PreparationError::issuer_mismatch(issuer, &discovery.issuer));
    }
    EndpointUrl::parse(&discovery.jwks_uri).map_err(|_| {
        PreparationError::new(PreparationPhase::Discovery, PreparationReason::InvalidUrl)
    })
}

/// Requests OIDC discovery metadata. A provider that answers that location
/// with a status other than 200 is asked for RFC 8414 metadata, whose
/// well-known segment precedes the issuer path.
async fn fetch_discovery(
    provider: &ProviderClient,
    issuer: &IssuerUrl,
    deadline: Instant,
) -> Result<Discovery, PreparationError> {
    let error = |reason| PreparationError::new(PreparationPhase::Discovery, reason);
    let path = issuer.url().path();
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut openid = issuer.url().clone();
    openid.set_path(&format!("{path}/.well-known/openid-configuration"));
    let bytes = match fetch_within(provider, &openid, deadline).await {
        Err(ProviderFailure::Status(_)) => {
            let mut oauth = issuer.url().clone();
            oauth.set_path(&format!("/.well-known/oauth-authorization-server{path}"));
            fetch_within(provider, &oauth, deadline).await
        }
        result => result,
    }
    .map_err(|failure| error(PreparationReason::Fetch(failure)))?;
    serde_json::from_slice(&bytes).map_err(|_| error(PreparationReason::Parse))
}

/// One provider request inside the startup budget.
async fn fetch_within(
    provider: &ProviderClient,
    url: &url::Url,
    deadline: Instant,
) -> Result<Vec<u8>, ProviderFailure> {
    tokio::time::timeout_at(deadline, provider.get_json(url))
        .await
        .unwrap_or(Err(ProviderFailure::Timeout))
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

fn is_access_token_type(value: &str) -> bool {
    value.eq_ignore_ascii_case("at+jwt") || value.eq_ignore_ascii_case("application/at+jwt")
}

fn is_generic_jwt_type(value: &str) -> bool {
    value.eq_ignore_ascii_case("JWT") || value.eq_ignore_ascii_case("application/jwt")
}

struct JwtKey {
    kid: Option<String>,
    /// One aws-lc key per algorithm the key serves. A JWK without `alg` serves
    /// every configured algorithm of its family, as Nimbus and go-oidc do.
    /// Parsing once keeps RSA Montgomery setup off the request path.
    verifiers: Vec<(JwtAlgorithm, ParsedPublicKey)>,
}

impl JwtKey {
    fn verifier(&self, algorithm: JwtAlgorithm) -> Option<&ParsedPublicKey> {
        self.verifiers
            .iter()
            .find_map(|(bound, key)| (*bound == algorithm).then_some(key))
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
    ) -> impl Iterator<Item = &'a ParsedPublicKey> {
        self.keys
            .iter()
            .filter(move |key| kid.is_none_or(|kid| key.kid.as_deref() == Some(kid)))
            .filter_map(move |key| key.verifier(algorithm))
    }

    /// Tries every installed key that fits the token; the first valid signature wins.
    fn verify(
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

fn admit_key(jwk: Jwk, configured: &[JwtAlgorithm]) -> Result<JwtKey, KeyRejection> {
    if !signature_usage(&jwk) {
        return Err(KeyRejection::IncompatibleUsage);
    }
    let family = KeyFamily::from_jwk(&jwk).ok_or(KeyRejection::UnsupportedFamily)?;
    let algorithms = match jwk.common.key_algorithm {
        Some(explicit) => vec![
            JwtAlgorithm::from_key_algorithm(explicit)
                .filter(|algorithm| configured.contains(algorithm) && algorithm.matches(family))
                .ok_or(KeyRejection::AlgorithmBinding)?,
        ],
        None => configured
            .iter()
            .copied()
            .filter(|algorithm| algorithm.matches(family))
            .collect(),
    };
    if algorithms.is_empty() {
        return Err(KeyRejection::AlgorithmBinding);
    }
    let material = key_material(&jwk.algorithm).ok_or(KeyRejection::InvalidMaterial)?;
    let verifiers = algorithms
        .into_iter()
        .map(|algorithm| Some((algorithm, material.parse(algorithm)?)))
        .collect::<Option<_>>()
        .ok_or(KeyRejection::InvalidMaterial)?;
    Ok(JwtKey {
        kid: jwk.common.key_id,
        verifiers,
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

    fn rsa_parameters(self) -> Option<&'static RsaParameters> {
        match self {
            Self::Rs256 => Some(&signature::RSA_PKCS1_2048_8192_SHA256),
            Self::Ps256 => Some(&signature::RSA_PSS_2048_8192_SHA256),
            Self::Es256 | Self::EdDsa => None,
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

/// Decoded public key material, checked with aws-lc at admission so an
/// unusable entry is skipped instead of failing every token that names it.
enum KeyMaterial {
    Rsa { modulus: Vec<u8>, exponent: Vec<u8> },
    Raw(Vec<u8>),
}

impl KeyMaterial {
    fn parse(&self, algorithm: JwtAlgorithm) -> Option<ParsedPublicKey> {
        match self {
            Self::Rsa { modulus, exponent } => RsaPublicKeyComponents {
                n: modulus.as_slice(),
                e: exponent.as_slice(),
            }
            .to_parsed_public_key(algorithm.rsa_parameters()?)
            .ok(),
            Self::Raw(bytes) => {
                let verification: &'static dyn signature::VerificationAlgorithm = match algorithm {
                    JwtAlgorithm::Es256 => &signature::ECDSA_P256_SHA256_FIXED,
                    JwtAlgorithm::EdDsa => &signature::ED25519,
                    JwtAlgorithm::Rs256 | JwtAlgorithm::Ps256 => return None,
                };
                ParsedPublicKey::new(verification, bytes).ok()
            }
        }
    }
}

fn key_material(parameters: &AlgorithmParameters) -> Option<KeyMaterial> {
    match parameters {
        AlgorithmParameters::RSA(parameters) => {
            // Some IdPs emit a leading zero byte; install the minimal encoding.
            let modulus = without_leading_zeros(&URL_SAFE_NO_PAD.decode(&parameters.n).ok()?)?;
            let exponent = without_leading_zeros(&URL_SAFE_NO_PAD.decode(&parameters.e).ok()?)?;
            // aws-lc parsing does not bound the size; its verifiers accept 2048–8192 bits.
            let bits = modulus.len() * 8 - modulus[0].leading_zeros() as usize;
            (2048..=8192)
                .contains(&bits)
                .then_some(KeyMaterial::Rsa { modulus, exponent })
        }
        AlgorithmParameters::EllipticCurve(parameters) => {
            let x = URL_SAFE_NO_PAD.decode(&parameters.x).ok()?;
            let y = URL_SAFE_NO_PAD.decode(&parameters.y).ok()?;
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            Some(KeyMaterial::Raw([[4].as_slice(), &x, &y].concat()))
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};

    use super::{ClaimPolicy, JwtAlgorithm, JwtVerifier, KeyStore, TokenProfile, parse_key_set};
    use crate::{Failure, Transport, VerificationReason, parse_bearer, refresh::UnknownKeyRefresh};

    const JWT_SIGNING_DER: &[u8] = include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");

    /// Fixture signing uses jsonwebtoken, which needs its crypto provider;
    /// verification itself does not.
    fn ensure_crypto_provider() {
        let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
    }

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
        ensure_crypto_provider();
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
        let claim_policy =
            ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()]).unwrap();
        JwtVerifier {
            claim_policy,
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
    async fn registered_claim_nulls_keep_jsonwebtoken_rules_and_null_identity_is_absent() {
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
    async fn the_compact_form_and_header_are_checked_before_any_key() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let (message, signature) = token.rsplit_once('.').unwrap();
        let (_, payload) = message.split_once('.').unwrap();
        let header = |json: &str| URL_SAFE_NO_PAD.encode(json);
        for (token, expected) in [
            (format!("{token}.x"), VerificationReason::Header),
            (message.to_owned(), VerificationReason::Header),
            (
                format!(
                    "{}.{payload}.{signature}",
                    header(r#"{"alg":"RS256","crit":["exp"]}"#)
                ),
                VerificationReason::Header,
            ),
            (
                format!("{}.{payload}.{signature}", header(r#"{"alg":"none"}"#)),
                VerificationReason::Header,
            ),
            // jsonwebtoken's header rules also reject malformed unused members.
            (
                format!(
                    "{}.{payload}.{signature}",
                    header(r#"{"alg":"RS256","kid":"fixture","x5c":5}"#)
                ),
                VerificationReason::Header,
            ),
            (
                format!("{message}.{signature}~"),
                VerificationReason::Signature,
            ),
            (
                format!("{message}.{}", URL_SAFE_NO_PAD.encode([7_u8; 1025])),
                VerificationReason::Signature,
            ),
        ] {
            assert_eq!(
                check(&verifier, &token).await.unwrap_err(),
                expected,
                "{token}"
            );
        }
    }

    #[tokio::test]
    async fn a_token_explicitly_typed_as_another_kind_of_jwt_is_not_an_access_token() {
        let resource_server = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let rfc9068 = JwtVerifier {
            token_profile: TokenProfile::Rfc9068,
            ..verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256])
        };
        let typed = |typ: Option<&str>| {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some("fixture".to_owned());
            header.typ = typ.map(ToOwned::to_owned);
            let claims = serde_json::json!({
                "iss": "https://issuer.example", "aud": "api",
                "exp": jsonwebtoken::get_current_timestamp() + 60,
                "iat": jsonwebtoken::get_current_timestamp(),
                "sub": "subject", "client_id": "client", "jti": "id",
            });
            encode(&header, &claims, &rsa_signing()).unwrap()
        };
        for (typ, generic) in [
            (None, true),
            (Some("JWT"), true),
            (Some("jwt"), true),
            (Some("application/jwt"), true),
            (Some("at+jwt"), false),
            (Some("application/at+jwt"), false),
        ] {
            let token = typed(typ);
            assert!(check(&resource_server, &token).await.is_ok(), "{typ:?}");
            assert_eq!(
                check(&rfc9068, &token).await.err(),
                generic.then_some(VerificationReason::Profile),
                "{typ:?}"
            );
        }
        for typ in [
            "client-authentication+jwt",
            "logout+jwt",
            "dpop+jwt",
            "secevent+jwt",
            "",
        ] {
            let token = typed(Some(typ));
            for verifier in [&resource_server, &rfc9068] {
                assert_eq!(
                    check(verifier, &token).await.unwrap_err(),
                    VerificationReason::Profile,
                    "{typ}"
                );
            }
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

    #[tokio::test]
    async fn access_token_equals_the_presented_bearer_text() {
        use secrecy::ExposeSecret;
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let principal = check(&verifier, &token).await.unwrap();
        assert_eq!(principal.access_token().expose_secret(), token);
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
            PreparationError::new(
                PreparationPhase::Jwks,
                PreparationReason::Fetch(crate::ProviderFailure::Status(503))
            )
            .to_string(),
            "authentication preparation failed during Jwks: Fetch(Status(503))"
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

    const FIXTURE_HOST: &str = "authn.fixture.test";

    /// Serves `routes` (request target and JSON body, where `PORT` stands for
    /// the listening port) over fixture TLS and answers any other target with
    /// 404. Returns the address, the trusted root and the targets requested.
    async fn metadata_server(
        routes: Vec<(&'static str, String)>,
    ) -> (
        std::net::SocketAddr,
        Vec<u8>,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (acceptor, root) = crate::provider::fixture_acceptor(FIXTURE_HOST);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let port = address.port().to_string();
        let requested = Arc::new(std::sync::Mutex::new(Vec::new()));
        tokio::spawn({
            let requested = Arc::clone(&requested);
            async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut stream = acceptor.accept(stream).await.unwrap();
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 4096];
                    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                        let read = stream.read(&mut chunk).await.unwrap();
                        assert_ne!(read, 0);
                        request.extend_from_slice(&chunk[..read]);
                    }
                    let request = String::from_utf8(request).unwrap();
                    let target = request.split(' ').nth(1).unwrap().to_owned();
                    let response = match routes.iter().find(|(route, _)| *route == target) {
                        Some((_, body)) => {
                            let body = body.replace("PORT", &port);
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                        None => "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            .to_owned(),
                    };
                    requested.lock().unwrap().push(target);
                    stream.write_all(response.as_bytes()).await.unwrap();
                    let _ = stream.shutdown().await;
                }
            }
        });
        (address, root, requested)
    }

    async fn prepare(
        address: std::net::SocketAddr,
        root: &[u8],
        jwks_uri: Option<&str>,
    ) -> Result<(crate::Verifier, super::RefreshTask), crate::PreparationError> {
        let origin = format!("https://{FIXTURE_HOST}:{}", address.port());
        let options = super::JwtOptions {
            issuer: crate::IssuerUrl::parse(&format!("{origin}/tenant")).unwrap(),
            audiences: vec!["api".to_owned()],
            token_profile: TokenProfile::ResourceServer,
            algorithms: vec![JwtAlgorithm::Rs256],
            jwks_uri: jwks_uri
                .map(|path| crate::EndpointUrl::parse(&format!("{origin}{path}")).unwrap()),
        };
        let cancel = tokio_util::sync::CancellationToken::new();
        let provider =
            crate::provider::new_fixture_client(FIXTURE_HOST, address, root, cancel.clone())
                .unwrap();
        super::prepare_with_provider(options, provider, cancel).await
    }

    #[tokio::test]
    async fn discovery_falls_back_to_rfc8414_metadata_and_an_explicit_jwks_uri_skips_it() {
        ensure_crypto_provider();
        let key = jwk(&rsa_signing(), Algorithm::RS256, Some("fixture"), true);
        let metadata = format!(
            r#"{{"issuer":"https://{FIXTURE_HOST}:PORT/tenant","jwks_uri":"https://{FIXTURE_HOST}:PORT/keys"}}"#
        );
        let (address, root, requested) = metadata_server(vec![
            ("/.well-known/oauth-authorization-server/tenant", metadata),
            ("/keys", serde_json::json!({ "keys": [key] }).to_string()),
        ])
        .await;
        let _prepared = prepare(address, &root, None).await.unwrap();
        assert_eq!(
            std::mem::take(&mut *requested.lock().unwrap()),
            [
                "/tenant/.well-known/openid-configuration",
                "/.well-known/oauth-authorization-server/tenant",
                "/keys"
            ]
        );
        let _prepared = prepare(address, &root, Some("/keys")).await.unwrap();
        assert_eq!(*requested.lock().unwrap(), ["/keys"]);
    }

    #[tokio::test]
    async fn a_provider_without_metadata_fails_preparation_with_its_status() {
        use crate::{PreparationPhase, PreparationReason, ProviderFailure};
        let (address, root, requested) = metadata_server(Vec::new()).await;
        let error = prepare(address, &root, None).await.err().unwrap();
        assert_eq!(error.phase(), PreparationPhase::Discovery);
        assert_eq!(
            error.reason(),
            PreparationReason::Fetch(ProviderFailure::Status(404))
        );
        assert_eq!(requested.lock().unwrap().len(), 2);
    }

    /// Signs arbitrary header and payload bytes, which `jsonwebtoken::encode`
    /// cannot produce.
    fn sign_raw(header: &str, payload: &[u8]) -> String {
        use aws_lc_rs::{rand::SystemRandom, signature};
        let key = signature::RsaKeyPair::from_der(JWT_SIGNING_DER).unwrap();
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(payload)
        );
        let mut signed = vec![0; key.public_modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            message.as_bytes(),
            &mut signed,
        )
        .unwrap();
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(signed))
    }

    const CORPUS_ISSUER: &str = "https://issuer.example";
    const CORPUS_HEADER: &str = r#"{"alg":"RS256","kid":"fixture"}"#;

    /// Headers the library reads without a key set or a token type rule.
    const CORPUS_HEADERS: [&str; 27] = [
        r#"{"alg":"RS256"}"#,
        r#"{"alg":"RS256","kid":"unknown"}"#,
        r#"{"alg":"RS256","kid":5}"#,
        r#"{"alg":"RS256","kid":null}"#,
        r#"{"alg":"none","kid":"fixture"}"#,
        r#"{"alg":"HS256","kid":"fixture"}"#,
        r#"{"alg":"ES256","kid":"fixture"}"#,
        r#"{"alg":"PS256","kid":"fixture"}"#,
        r#"{"alg":"rs256","kid":"fixture"}"#,
        r#"{"kid":"fixture"}"#,
        r#"{"alg":"RS256","kid":"fixture","crit":[]}"#,
        r#"{"alg":"RS256","kid":"fixture","crit":["exp"]}"#,
        r#"{"alg":"RS256","kid":"fixture","crit":"exp"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":"JWT"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":"at+jwt"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":"dpop+jwt"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":1}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":null}"#,
        r#"{"alg":"RS256","kid":"fixture","x5c":5}"#,
        r#"{"alg":"RS256","kid":"fixture","jwk":"x"}"#,
        r#"{"alg":"RS256","kid":"fixture","cty":"JWT"}"#,
        r#"{"alg":"RS256","kid":"fixture","zzz":{"a":[1]}}"#,
        r#"{"alg":"RS256","alg":"RS256","kid":"fixture"}"#,
        r#"{"alg":"RS256","kid":"fixture","kid":"fixture"}"#,
        "[1]",
        "",
        "{",
    ];

    /// Every registered claim in every shape, and pairs that fail two checks.
    fn claim_cases(base: &serde_json::Value, now: u64) -> Vec<(String, String)> {
        use serde_json::{Value, json};
        const ISSUER: &str = CORPUS_ISSUER;
        const HEADER: &str = CORPUS_HEADER;
        #[allow(
            clippy::cast_precision_loss,
            reason = "current epoch seconds fit an f64"
        )]
        let shapes: Vec<Option<Value>> = vec![
            None,
            Some(Value::Null),
            Some("".into()),
            Some("api".into()),
            Some("other".into()),
            Some(ISSUER.into()),
            Some("a\"pi\\u0041".into()),
            Some(42.into()),
            Some(json!(1.5)),
            Some(json!(-5)),
            Some(json!(u64::MAX)),
            Some(json!(1e30)),
            Some((now - 100).into()),
            Some((now - 25).into()),
            Some((now + 25).into()),
            Some((now + 100).into()),
            Some(json!(now as f64 + 100.4)),
            Some(json!(now as f64 - 1000.6)),
            Some(json!(["api"])),
            Some(json!([ISSUER])),
            Some(json!(["x", "y"])),
            Some(json!(["x", "api"])),
            Some(json!(["x", ISSUER])),
            Some(json!([])),
            Some(json!([1])),
            Some(json!([null])),
            Some(json!({"a": 1})),
            Some(true.into()),
        ];
        let mut exact = Vec::new();
        for claim in ["iss", "aud", "exp", "nbf"] {
            for shape in &shapes {
                let mut claims = base.clone();
                match shape {
                    None => drop(claims.as_object_mut().unwrap().remove(claim)),
                    Some(value) => claims[claim] = value.clone(),
                }
                exact.push((
                    format!("{claim}={shape:?}"),
                    sign_raw(HEADER, claims.to_string().as_bytes()),
                ));
            }
        }
        for (first, first_value, second, second_value) in [
            ("iss", json!("x"), "exp", json!(now - 100)),
            ("aud", json!("x"), "nbf", json!(now + 100)),
            ("iss", json!("x"), "aud", json!("x")),
            ("exp", json!(now - 100), "nbf", json!(now + 100)),
        ] {
            let mut claims = base.clone();
            claims[first] = first_value;
            claims[second] = second_value;
            exact.push((
                format!("{first} and {second}"),
                sign_raw(HEADER, claims.to_string().as_bytes()),
            ));
        }
        exact
    }

    /// Payloads that are not one plain claims object, and broken compact forms.
    fn payload_and_compact_cases(body: &str) -> Vec<(String, String)> {
        const HEADER: &str = CORPUS_HEADER;
        let mut exact = Vec::new();
        let members = &body[1..body.len() - 1];
        for (name, payload) in [
            ("duplicate iss", format!(r#"{{{members},"iss":"x"}}"#)),
            ("duplicate aud", format!(r#"{{{members},"aud":"api"}}"#)),
            ("duplicate exp", format!(r#"{{{members},"exp":1}}"#)),
            ("duplicate nbf", format!(r#"{{{members},"nbf":1,"nbf":2}}"#)),
            ("duplicate custom", format!(r#"{{{members},"t":1,"t":2}}"#)),
            ("trailing space", format!("{body} ")),
            ("leading space", format!(" {body}")),
            ("array", "[1]".to_owned()),
            ("string", "\"x\"".to_owned()),
            ("two documents", format!("{body}{body}")),
            (
                "deep nesting",
                format!(
                    r#"{{{members},"d":{}0{}}}"#,
                    "[".repeat(200),
                    "]".repeat(200)
                ),
            ),
        ] {
            exact.push((name.to_owned(), sign_raw(HEADER, payload.as_bytes())));
        }
        exact.push((
            "payload is not UTF-8".to_owned(),
            sign_raw(HEADER, &[b'{', b'"', 0xff, b'"', b':', b'1', b'}']),
        ));
        let good = sign_raw(HEADER, body.as_bytes());
        let (message, signature) = good.rsplit_once('.').unwrap();
        let (header, payload) = message.split_once('.').unwrap();
        for (name, token) in [
            ("valid", good.clone()),
            ("four parts", format!("{good}.x")),
            ("two parts", message.to_owned()),
            ("empty signature", format!("{message}.")),
            ("padded signature", format!("{message}.{signature}=")),
            (
                "truncated signature",
                format!("{message}.{}", &signature[..signature.len() - 4]),
            ),
            (
                "long signature",
                format!("{message}.{}", URL_SAFE_NO_PAD.encode([7_u8; 1500])),
            ),
            (
                "signature outside the alphabet",
                format!("{message}.{}~", &signature[..signature.len() - 1]),
            ),
            (
                "another token's signature",
                format!(
                    "{message}.{}",
                    sign_raw(HEADER, b"{}").rsplit_once('.').unwrap().1
                ),
            ),
            (
                "payload outside the alphabet",
                format!("{header}.a~b.{signature}"),
            ),
            ("empty header", format!(".{payload}.{signature}")),
            ("padded header", format!("{header}=.{payload}.{signature}")),
        ] {
            exact.push((name.to_owned(), token));
        }
        exact
    }

    /// This crate verifies the signature and reads the registered claims
    /// itself. Its decisions on them must stay those of `jsonwebtoken::decode`,
    /// and no header that the library refuses may pass here.
    #[tokio::test]
    async fn registered_claim_decisions_match_jsonwebtoken_decode() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let key = serde_json::from_value::<Jwk>(jwk(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            true,
        ))
        .unwrap();
        let key = jsonwebtoken::DecodingKey::from_jwk(&key).unwrap();
        let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
        validation.leeway = 30;
        validation.validate_nbf = true;
        validation.set_issuer(&[CORPUS_ISSUER]);
        validation.set_audience(&["api"]);
        validation.set_required_spec_claims(&["exp", "iss", "aud"]);
        // `IgnoredAny` keeps the library's own claim rules and adds none.
        let library = |token: &str| {
            jsonwebtoken::decode::<serde::de::IgnoredAny>(token, &key, &validation).is_ok()
        };
        // A token outside the bearer grammar never reaches the verifier.
        let accepts = async |token: &str| {
            let header = format!("Bearer {token}");
            match parse_bearer([header.as_bytes()]) {
                Ok(token) => verifier.verify(&token).await.is_ok(),
                Err(_) => false,
            }
        };

        let now = jsonwebtoken::get_current_timestamp();
        let base = serde_json::json!({
            "iss": CORPUS_ISSUER, "aud": "api", "exp": now + 600, "sub": "subject",
        });
        let body = base.to_string();
        let mut exact = claim_cases(&base, now);
        exact.extend(payload_and_compact_cases(&body));
        let mut differences = Vec::new();
        for (name, token) in &exact {
            let (here, there) = (accepts(token).await, library(token));
            if here != there {
                differences.push(format!("{name}: {here} here, {there} in jsonwebtoken"));
            }
        }
        // This crate adds key selection and a token type rule, so it may
        // refuse more headers than the library, never fewer.
        for header in CORPUS_HEADERS {
            let token = sign_raw(header, body.as_bytes());
            if accepts(&token).await && !library(&token) {
                differences.push(format!("{header}: accepted here, refused by jsonwebtoken"));
            }
        }
        assert!(differences.is_empty(), "{differences:#?}");
        // The corpus exercises both decisions, not one constant answer.
        let accepted = exact.iter().filter(|(_, token)| library(token)).count();
        assert!(accepted > 0 && accepted < exact.len());
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

    #[test]
    fn authentication_metrics_preserve_success_failure_and_cancellation_labels() {
        use std::{future::Future, task::Poll};

        let diagnostics = Diagnostics::default();
        let engine = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        engine.keys.permit_unknown_refresh_for_test();
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let header = format!("Bearer {token}");
        let unknown = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("rotated"),
            &serde_json::json!({}),
        );
        let unknown_header = format!("Bearer {unknown}");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        metrics::with_local_recorder(&diagnostics, || {
            // Success counters bind to the recorder installed at preparation.
            let verifier = crate::Verifier::jwt(engine);
            runtime.block_on(async {
                for transport in [Transport::Http, Transport::Grpc] {
                    verifier
                        .authenticate([header.as_bytes()], transport)
                        .await
                        .unwrap();
                }
                assert_eq!(
                    verifier
                        .authenticate(std::iter::empty(), Transport::Http)
                        .await,
                    Err(Failure::Missing)
                );
                assert_eq!(
                    verifier
                        .authenticate([b"Bearer =".as_slice()], Transport::Grpc)
                        .await,
                    Err(Failure::Malformed)
                );
                let mut pending =
                    Box::pin(verifier.authenticate([unknown_header.as_bytes()], Transport::Http));
                std::future::poll_fn(|cx| {
                    assert!(pending.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(pending);
            });
        });
        let counters = diagnostics.counters.lock().unwrap();
        let verifications = counters
            .iter()
            .filter(|(key, _)| key.name() == "authn_token_verifications_total")
            .collect::<Vec<_>>();
        assert_eq!(verifications.len(), 2);
        assert!(verifications.iter().all(|(key, value)| {
            *value == 1
                && key
                    .labels()
                    .any(|label| label.key() == "mode" && label.value() == "jwt")
                && key
                    .labels()
                    .any(|label| label.key() == "outcome" && label.value() == "success")
                && key
                    .labels()
                    .any(|label| label.key() == "reason" && label.value() == "verified")
        }));
        let outcomes = counters
            .iter()
            .filter(|(key, _)| key.name() == crate::AUTHN_VERIFICATIONS_METRIC)
            .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 5);
        for (transport, result, failure) in [
            ("http", "success", "none"),
            ("grpc", "success", "none"),
            ("http", "failure", "missing"),
            ("grpc", "failure", "malformed"),
            ("http", "cancelled", "cancelled"),
        ] {
            assert!(outcomes.iter().any(|(key, value)| {
                *value == 1
                    && key
                        .labels()
                        .any(|label| label.key() == "transport" && label.value() == transport)
                    && key
                        .labels()
                        .any(|label| label.key() == "result" && label.value() == result)
                    && key
                        .labels()
                        .any(|label| label.key() == "failure" && label.value() == failure)
            }));
        }
    }
}
