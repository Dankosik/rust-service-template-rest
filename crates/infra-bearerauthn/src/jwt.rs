//! OIDC discovery, strict JWT parsing, and signature verification.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::claims::{ClaimPolicy, validate_jwt_claims};
use crate::provider::{ProviderClient, parse_provider_url};
use crate::refresh::{SharedRefresh, UnknownKeyResult, run_refresh_worker};
use crate::{BearerToken, Failure, JwtOptions, Principal, RefreshTask, TokenProfile, Verifier};

const STARTUP_BUDGET: Duration = Duration::from_secs(6);
const ATTEMPT_BUDGET: Duration = Duration::from_secs(3);

#[cfg(test)]
#[path = "../../infra-egress-dns/tests/fixtures/tls.rs"]
mod tls;

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
        let compact = CompactToken::parse(token.as_bytes(), self.token_profile)?;
        let key = self.select_key(compact.kid.as_deref(), deadline).await?;

        verify_signature(token.as_bytes(), &key)?;
        let now_epoch_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Failure::Unavailable)?
            .as_secs();
        validate_jwt_claims(
            &compact.payload,
            &self.claim_policy,
            self.token_profile,
            now_epoch_seconds,
        )
    }

    async fn select_key(
        &self,
        kid: Option<&str>,
        deadline: Instant,
    ) -> Result<DecodingKey, Failure> {
        let initial = self.refresh.keys().await;
        let key = match initial.select(kid) {
            KeySelection::One(key) => key.clone(),
            KeySelection::UnknownKid => match self.refresh.refresh_unknown(deadline).await {
                UnknownKeyResult::Refreshed => {
                    let refreshed = self.refresh.keys().await;
                    match refreshed.select(kid) {
                        KeySelection::One(key) => key.clone(),
                        KeySelection::UnknownKid | KeySelection::Ambiguous => {
                            return Err(Failure::Invalid);
                        }
                    }
                }
                UnknownKeyResult::CooldownSuccess => return Err(Failure::Invalid),
                UnknownKeyResult::RefreshFailed
                | UnknownKeyResult::CooldownFailure
                | UnknownKeyResult::DeadlineElapsed => return Err(Failure::Unavailable),
            },
            KeySelection::Ambiguous => return Err(Failure::Invalid),
        };
        Ok(key)
    }
}

/// Prepares initial trust before traffic admission and returns the sole refresh
/// future for bootstrap to spawn through its existing process tracker.
///
/// # Errors
///
/// Returns [`Failure::Unavailable`] when discovery, initial trust, or private
/// provider transport cannot establish usable JWT verification.
pub async fn prepare_jwt(
    options: JwtOptions,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), Failure> {
    let provider = ProviderClient::new()?;
    prepare_with_provider(options, provider, cancel).await
}

/// Prepares the real JWT verifier through the narrowly admitted local TLS
/// transport used by this crate's fixture tests.
#[cfg(test)]
pub(crate) async fn prepare_jwt_with_fixture(
    options: JwtOptions,
    fixture: crate::test_support::FixtureTransport,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), Failure> {
    prepare_with_provider(options, fixture.into_provider(), cancel).await
}

async fn prepare_with_provider(
    options: JwtOptions,
    provider: ProviderClient,
    cancel: CancellationToken,
) -> Result<(Verifier, RefreshTask), Failure> {
    let startup_deadline = Instant::now() + STARTUP_BUDGET;
    let (jwks_uri, keys) = tokio::time::timeout_at(startup_deadline, async {
        let discovery_uri = discovery_url(&options.issuer)?;
        let discovery = fetch_discovery(&provider, &discovery_uri, startup_deadline).await?;
        if discovery.issuer != options.issuer {
            return Err(Failure::Unavailable);
        }
        let jwks_uri = admitted_jwks_uri(&discovery.jwks_uri)?;
        let bytes = provider
            .get_json(&jwks_uri, attempt_deadline(startup_deadline)?)
            .await?;
        Ok::<_, Failure>((jwks_uri, Arc::new(parse_key_set(&bytes)?)))
    })
    .await
    .map_err(|_| Failure::Unavailable)??;

    let refresh = SharedRefresh::new(keys);
    let verifier = JwtVerifier {
        claim_policy: ClaimPolicy::new(options.issuer, options.audience),
        token_profile: options.token_profile,
        refresh: refresh.clone(),
    };
    let refresh_cancel = cancel.child_token();
    let refresh_task: RefreshTask = Box::pin(async move {
        run_refresh_worker(refresh, provider, jwks_uri, refresh_cancel).await;
    });
    Ok((Verifier::Jwt(verifier), refresh_task))
}

async fn fetch_discovery(
    provider: &ProviderClient,
    uri: &Url,
    startup_deadline: Instant,
) -> Result<Discovery, Failure> {
    let bytes = provider
        .get_json(uri, attempt_deadline(startup_deadline)?)
        .await?;
    Discovery::parse(&bytes)
}

fn attempt_deadline(startup_deadline: Instant) -> Result<Instant, Failure> {
    let now = Instant::now();
    let deadline = (now + ATTEMPT_BUDGET).min(startup_deadline);
    (deadline > now)
        .then_some(deadline)
        .ok_or(Failure::Unavailable)
}

fn discovery_url(issuer: &str) -> Result<Url, Failure> {
    let mut url = parse_provider_url(issuer)?;
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    url.set_path(&format!("{path}/.well-known/openid-configuration"));
    Ok(url)
}

fn admitted_jwks_uri(value: &str) -> Result<Url, Failure> {
    parse_provider_url(value)
}

struct CompactToken {
    payload: Vec<u8>,
    kid: Option<String>,
}

impl CompactToken {
    fn parse(token: &[u8], profile: TokenProfile) -> Result<Self, Failure> {
        let mut segments = token.split(|byte| *byte == b'.');
        let (Some(header), Some(payload), Some(_signature), None) = (
            segments.next(),
            segments.next(),
            segments.next(),
            segments.next(),
        ) else {
            return Err(Failure::Invalid);
        };
        if header.is_empty() || payload.is_empty() {
            return Err(Failure::Invalid);
        }
        let header = URL_SAFE_NO_PAD
            .decode(header)
            .map_err(|_| Failure::Invalid)?;
        let header = JoseHeader::parse(&header)?;
        if header.alg != "RS256" {
            return Err(Failure::Invalid);
        }
        if profile == TokenProfile::Rfc9068
            && !header.typ.as_deref().is_some_and(is_access_token_type)
        {
            return Err(Failure::Invalid);
        }
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Failure::Invalid)?;
        Ok(Self {
            payload,
            kid: header.kid,
        })
    }
}

fn is_access_token_type(value: &str) -> bool {
    value.eq_ignore_ascii_case("at+jwt") || value.eq_ignore_ascii_case("application/at+jwt")
}

#[derive(Default)]
struct JoseHeader {
    alg: String,
    typ: Option<String>,
    kid: Option<String>,
    seen_alg: bool,
    seen_typ: bool,
    seen_kid: bool,
}

impl JoseHeader {
    fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let header = deserializer
            .deserialize_map(JoseHeaderVisitor)
            .and_then(|header| deserializer.end().map(|()| header))
            .map_err(|_| Failure::Invalid)?;
        if !header.seen_alg || header.alg.is_empty() {
            return Err(Failure::Invalid);
        }
        Ok(header)
    }
}

struct JoseHeaderVisitor;

impl<'de> Visitor<'de> for JoseHeaderVisitor {
    type Value = JoseHeader;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JWT JOSE header object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut header = JoseHeader::default();
        while let Some(name) = map.next_key::<String>()? {
            match name.as_str() {
                "alg" => set_once(&mut header.seen_alg, &mut header.alg, &mut map)?,
                "typ" => set_once_optional(&mut header.seen_typ, &mut header.typ, &mut map)?,
                "kid" => set_once_optional(&mut header.seen_kid, &mut header.kid, &mut map)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(header)
    }
}

struct Discovery {
    issuer: String,
    jwks_uri: String,
}

impl Discovery {
    fn parse(bytes: &[u8]) -> Result<Self, Failure> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        deserializer
            .deserialize_map(DiscoveryVisitor)
            .and_then(|discovery| deserializer.end().map(|()| discovery))
            .map_err(|_| Failure::Unavailable)
    }
}

struct DiscoveryVisitor;

impl<'de> Visitor<'de> for DiscoveryVisitor {
    type Value = Discovery;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an OIDC discovery object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut issuer = None;
        let mut jwks_uri = None;
        while let Some(name) = map.next_key::<String>()? {
            match name.as_str() {
                "issuer" => set_required_once(&mut issuer, &mut map)?,
                "jwks_uri" => set_required_once(&mut jwks_uri, &mut map)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Discovery {
            issuer: issuer.ok_or_else(|| serde::de::Error::missing_field("issuer"))?,
            jwks_uri: jwks_uri.ok_or_else(|| serde::de::Error::missing_field("jwks_uri"))?,
        })
    }
}

#[derive(Clone)]
struct JwtKey {
    kid: Option<String>,
    decoding_key: DecodingKey,
}

pub(crate) struct KeySet {
    keys: Vec<JwtKey>,
}

impl KeySet {
    fn select(&self, kid: Option<&str>) -> KeySelection<'_> {
        match kid {
            Some(kid) => match self
                .keys
                .iter()
                .filter(|key| key.kid.as_deref() == Some(kid))
                .collect::<Vec<_>>()
                .as_slice()
            {
                [key] => KeySelection::One(&key.decoding_key),
                [] => KeySelection::UnknownKid,
                _ => KeySelection::Ambiguous,
            },
            None => match self.keys.as_slice() {
                [key] => KeySelection::One(&key.decoding_key),
                _ => KeySelection::Ambiguous,
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn has_kid(&self, kid: &str) -> bool {
        self.keys.iter().any(|key| key.kid.as_deref() == Some(kid))
    }
}

enum KeySelection<'a> {
    One(&'a DecodingKey),
    UnknownKid,
    Ambiguous,
}

pub(crate) fn parse_key_set(bytes: &[u8]) -> Result<KeySet, Failure> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let raw = deserializer
        .deserialize_map(JwksVisitor)
        .and_then(|jwks| deserializer.end().map(|()| jwks))
        .map_err(|_| Failure::Unavailable)?;

    let mut seen_kids = HashSet::new();
    let mut keys = Vec::new();
    for raw_key in raw.keys {
        if raw_key.kty.as_deref() != Some("RSA") {
            continue;
        }
        if let Some(key) = raw_key.into_eligible(&mut seen_kids)? {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return Err(Failure::Unavailable);
    }
    Ok(KeySet { keys })
}

struct Jwks {
    keys: Vec<RawJwk>,
}

struct JwksVisitor;

impl<'de> Visitor<'de> for JwksVisitor {
    type Value = Jwks;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JWKS object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = None;
        while let Some(name) = map.next_key::<String>()? {
            if name == "keys" {
                set_required_once(&mut keys, &mut map)?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(Jwks {
            keys: keys.ok_or_else(|| serde::de::Error::missing_field("keys"))?,
        })
    }
}

impl<'de> Deserialize<'de> for RawJwk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawJwkVisitor)
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag preserves strict duplicate, missing, and null JWK evidence"
)]
#[derive(Default)]
struct RawJwk {
    kty: Option<String>,
    kid: Option<String>,
    alg: Option<String>,
    use_: Option<String>,
    key_ops: Option<Vec<String>>,
    n: Option<String>,
    e: Option<String>,
    seen_kty: bool,
    seen_kid: bool,
    seen_alg: bool,
    seen_use: bool,
    seen_key_ops: bool,
    seen_n: bool,
    seen_e: bool,
}

struct RawJwkVisitor;

impl<'de> Visitor<'de> for RawJwkVisitor {
    type Value = RawJwk;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JWK object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut key = RawJwk::default();
        while let Some(name) = map.next_key::<String>()? {
            match name.as_str() {
                "kty" => set_once_optional(&mut key.seen_kty, &mut key.kty, &mut map)?,
                "kid" => set_once_optional(&mut key.seen_kid, &mut key.kid, &mut map)?,
                "alg" => set_once_optional(&mut key.seen_alg, &mut key.alg, &mut map)?,
                "use" => set_once_optional(&mut key.seen_use, &mut key.use_, &mut map)?,
                "key_ops" => set_once_optional(&mut key.seen_key_ops, &mut key.key_ops, &mut map)?,
                "n" => set_once_optional(&mut key.seen_n, &mut key.n, &mut map)?,
                "e" => set_once_optional(&mut key.seen_e, &mut key.e, &mut map)?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(key)
    }
}

impl RawJwk {
    fn into_eligible(self, seen_kids: &mut HashSet<String>) -> Result<Option<JwtKey>, Failure> {
        if self
            .alg
            .as_deref()
            .is_some_and(|algorithm| algorithm != "RS256")
            || self.use_.as_deref().is_some_and(|usage| usage != "sig")
        {
            return Ok(None);
        }
        if let Some(operations) = self.key_ops
            && (!operations.iter().any(|operation| operation == "verify")
                || operations.iter().any(|operation| {
                    matches!(
                        operation.as_str(),
                        "sign"
                            | "encrypt"
                            | "decrypt"
                            | "wrapKey"
                            | "unwrapKey"
                            | "deriveKey"
                            | "deriveBits"
                    )
                }))
        {
            return Ok(None);
        }
        let modulus = self.n.ok_or(Failure::Unavailable)?;
        let exponent = self.e.ok_or(Failure::Unavailable)?;
        let modulus_bytes = URL_SAFE_NO_PAD
            .decode(modulus)
            .map_err(|_| Failure::Unavailable)?;
        let exponent_bytes = URL_SAFE_NO_PAD
            .decode(exponent)
            .map_err(|_| Failure::Unavailable)?;
        let bits = modulus_bits(&modulus_bytes).ok_or(Failure::Unavailable)?;
        if !(2048..=8192).contains(&bits) || exponent_bytes.is_empty() {
            return Err(Failure::Unavailable);
        }
        if let Some(kid) = &self.kid
            && !seen_kids.insert(kid.clone())
        {
            return Err(Failure::Unavailable);
        }
        Ok(Some(JwtKey {
            kid: self.kid,
            decoding_key: DecodingKey::from_rsa_raw_components(&modulus_bytes, &exponent_bytes),
        }))
    }
}

fn modulus_bits(bytes: &[u8]) -> Option<usize> {
    let first = *bytes.first()?;
    if first == 0 {
        return None;
    }
    Some((bytes.len() - 1) * 8 + (8 - first.leading_zeros() as usize))
}

fn set_once<'de, A, T>(seen: &mut bool, destination: &mut T, map: &mut A) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
    T: Deserialize<'de>,
{
    if *seen {
        return Err(serde::de::Error::duplicate_field("JWT member"));
    }
    *seen = true;
    *destination = map.next_value()?;
    Ok(())
}

fn set_once_optional<'de, A, T>(
    seen: &mut bool,
    destination: &mut Option<T>,
    map: &mut A,
) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
    T: Deserialize<'de>,
{
    if *seen {
        return Err(serde::de::Error::duplicate_field("JWT member"));
    }
    *seen = true;
    *destination = Some(map.next_value()?);
    Ok(())
}

fn set_required_once<'de, A, T>(destination: &mut Option<T>, map: &mut A) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
    T: Deserialize<'de>,
{
    if destination.is_some() {
        return Err(serde::de::Error::duplicate_field("JWT member"));
    }
    *destination = Some(map.next_value()?);
    Ok(())
}

fn verify_signature(token: &[u8], key: &DecodingKey) -> Result<(), Failure> {
    let mut validation = Validation::new(Algorithm::RS256);
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;
    validation.required_spec_claims.clear();
    jsonwebtoken::decode::<IgnoredAny>(token, key, &validation)
        .map(|_| ())
        .map_err(|_| Failure::Invalid)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::Instant,
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
        },
    };
    use tokio_util::sync::CancellationToken;

    use super::{
        ClaimPolicy, CompactToken, JwtVerifier, KeySelection, SharedRefresh, TokenProfile,
        parse_key_set, prepare_jwt_with_fixture, tls::TlsMaterial,
    };
    use crate::{Failure, JwtOptions, Verifier, claims::validate_jwt_claims, parse_bearer};

    const FIXTURE_HOST: &str = "authn.fixture.test";
    const JWT_SIGNING_DER: &[u8] = include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");
    const FIXTURE_MODULUS: &str = "oqVsNW7NLfad_LLglPJsIEFPiNDxHC8VBEUrd0qat1etcgBHlY5rPq_Fxo6DUo6fe1_haaLbqf4HwPK7TAf_N9FuQ4TnLuG2fHkNFUlf_P2IM_c3MpiOo4-1DcTk1_-aN4P-7XKq_c4W6ssAdtE5T91TX8gQMtRmsy-G6G45mdroQBlM3V7ZQcn3T3d6j3BQRYdBijVaPMFYxBfOEi4-3OWpJIiqYiW-TZYFcM9RUkE8egaFh7Ck8Ah_B1f30lrrVaidmdFpvZrxIPg0FvmEOQJrRZuACZNKgK83sVUgRmI3jtSquFlbvoo-ayOIOMssBQuAt380L22tfKSjn3cE0Q";

    fn fixture_jwks(kid: &str) -> String {
        format!(r#"{{"keys":[{{"kty":"RSA","kid":"{kid}","n":"{FIXTURE_MODULUS}","e":"AQAB"}}]}}"#)
    }

    fn signed_token(issuer: &str, kid: &str, audience: &str) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_owned());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        jsonwebtoken::encode(
            &header,
            &serde_json::json!({
                "iss": issuer,
                "aud": audience,
                "exp": now + 300,
                "sub": "subject",
            }),
            &EncodingKey::from_rsa_der(JWT_SIGNING_DER),
        )
        .unwrap()
    }

    fn verifier_with_fixture_key(kid: &str) -> JwtVerifier {
        let keys = Arc::new(parse_key_set(fixture_jwks(kid).as_bytes()).unwrap());
        JwtVerifier {
            claim_policy: ClaimPolicy::new(
                "https://issuer.example".to_owned(),
                "service".to_owned(),
            ),
            token_profile: TokenProfile::ResourceServer,
            refresh: SharedRefresh::new(keys),
        }
    }

    async fn tls_sequence_server(
        material: &TlsMaterial,
        build_responses: impl FnOnce(std::net::SocketAddr) -> Vec<Vec<u8>>,
    ) -> (std::net::SocketAddr, JoinHandle<Vec<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses = build_responses(address);
        let config = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(material.cert.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key.clone())),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for response in responses {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = acceptor.accept(stream).await.unwrap();
                let mut request = vec![0; 4096];
                let read = stream.read(&mut request).await.unwrap();
                request.truncate(read);
                requests.push(request);
                stream.write_all(&response).await.unwrap();
                stream.flush().await.unwrap();
            }
            requests
        });
        (address, server)
    }

    fn json_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn modulus() -> String {
        let mut bytes = vec![0_u8; 256];
        bytes[0] = 0x80;
        URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn key_selection_requires_one_exact_eligible_key() {
        let n = modulus();
        let keys = parse_key_set(
            format!(r#"{{"keys":[{{"kty":"RSA","kid":"one","n":"{n}","e":"AQAB"}}]}}"#).as_bytes(),
        )
        .unwrap();
        assert!(matches!(keys.select(Some("one")), KeySelection::One(_)));
        assert!(matches!(
            keys.select(Some("other")),
            KeySelection::UnknownKid
        ));
        assert!(matches!(keys.select(None), KeySelection::One(_)));

        let duplicate = format!(
            r#"{{"keys":[{{"kty":"RSA","kid":"one","n":"{n}","e":"AQAB"}},{{"kty":"RSA","kid":"one","n":"{n}","e":"AQAB"}}]}}"#
        );
        assert!(matches!(
            parse_key_set(duplicate.as_bytes()),
            Err(Failure::Unavailable)
        ));
    }

    #[test]
    fn strict_header_and_claim_policy_reject_duplicates_and_rfc9068_typing_gaps() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"at+jwt","typ":"at+jwt"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br"{}");
        let token = format!("{header}.{payload}.signature");
        assert!(matches!(
            CompactToken::parse(token.as_bytes(), TokenProfile::Rfc9068),
            Err(Failure::Invalid)
        ));

        let policy = ClaimPolicy::new("https://issuer.example".to_owned(), "service".to_owned());
        let claims = br#"{"iss":"https://issuer.example","aud":"service","exp":200,"sub":"person","client_id":"client","jti":"id","iat":100}"#;
        assert!(validate_jwt_claims(claims, &policy, TokenProfile::Rfc9068, 100).is_ok());
        let duplicate = br#"{"iss":"https://issuer.example","iss":"https://issuer.example","aud":"service","exp":200,"sub":"person","client_id":"client","jti":"id","iat":100}"#;
        assert_eq!(
            validate_jwt_claims(duplicate, &policy, TokenProfile::Rfc9068, 100),
            Err(Failure::Invalid)
        );
    }

    #[test]
    fn strict_consumed_header_and_jwk_metadata_reject_nulls_and_key_operations() {
        let payload = URL_SAFE_NO_PAD.encode(br"{}");
        for header_json in [
            br#"{"alg":null}"#.as_slice(),
            br#"{"alg":"RS256","typ":null}"#.as_slice(),
            br#"{"alg":"RS256","kid":null}"#.as_slice(),
        ] {
            let header = URL_SAFE_NO_PAD.encode(header_json);
            let token = format!("{header}.{payload}.signature");
            assert!(matches!(
                CompactToken::parse(token.as_bytes(), TokenProfile::ResourceServer),
                Err(Failure::Invalid)
            ));
        }

        let n = modulus();
        for member in ["kty", "kid", "alg", "use", "key_ops", "n", "e"] {
            let members = [
                ("kty", r#""kty":"RSA""#.to_owned()),
                ("kid", r#""kid":"fixture""#.to_owned()),
                ("alg", r#""alg":"RS256""#.to_owned()),
                ("use", r#""use":"sig""#.to_owned()),
                ("key_ops", r#""key_ops":["verify"]"#.to_owned()),
                ("n", format!(r#""n":"{n}""#)),
                ("e", r#""e":"AQAB""#.to_owned()),
            ];
            let null_field = format!(r#""{member}":null"#);
            let fields = members
                .iter()
                .filter(|(name, _)| *name != member)
                .map(|(_, value)| value.as_str())
                .chain(std::iter::once(null_field.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            let jwks = format!(r#"{{"keys":[{{{fields}}}]}}"#);
            assert!(matches!(
                parse_key_set(jwks.as_bytes()),
                Err(Failure::Unavailable)
            ));
        }

        for operation in [
            "sign",
            "encrypt",
            "decrypt",
            "wrapKey",
            "unwrapKey",
            "deriveKey",
            "deriveBits",
        ] {
            let jwks = format!(
                r#"{{"keys":[{{"kty":"RSA","n":"{n}","e":"AQAB","key_ops":["verify","{operation}"]}}]}}"#
            );
            assert!(matches!(
                parse_key_set(jwks.as_bytes()),
                Err(Failure::Unavailable)
            ));
        }
    }

    #[tokio::test]
    async fn jwt_verifier_accepts_signed_rs256_and_rejects_bad_signature_or_strict_claims() {
        let verifier = verifier_with_fixture_key("fixture");
        let deadline = Instant::now() + Duration::from_secs(1);
        let valid = signed_token("https://issuer.example", "fixture", "service");
        let valid_header = format!("Bearer {valid}");
        let valid = parse_bearer([valid_header.as_bytes()]).unwrap();
        assert_eq!(
            verifier.verify(&valid, deadline).await.unwrap().subject(),
            Some("subject")
        );

        let mut wrong_signature =
            signed_token("https://issuer.example", "fixture", "service").into_bytes();
        let signature_start = wrong_signature
            .iter()
            .rposition(|byte| *byte == b'.')
            .unwrap()
            + 1;
        wrong_signature[signature_start] = if wrong_signature[signature_start] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let wrong_signature_header =
            format!("Bearer {}", String::from_utf8(wrong_signature).unwrap());
        let wrong_signature = parse_bearer([wrong_signature_header.as_bytes()]).unwrap();
        assert_eq!(
            verifier.verify(&wrong_signature, deadline).await,
            Err(Failure::Invalid)
        );

        let wrong_audience = signed_token("https://issuer.example", "fixture", "other");
        let wrong_audience_header = format!("Bearer {wrong_audience}");
        let wrong_audience = parse_bearer([wrong_audience_header.as_bytes()]).unwrap();
        assert_eq!(
            verifier.verify(&wrong_audience, deadline).await,
            Err(Failure::Invalid)
        );
    }

    #[tokio::test]
    async fn fixture_prepare_discovers_initial_keys_rotates_and_retains_last_good_after_failure() {
        let material = TlsMaterial::new(FIXTURE_HOST);
        let initial = fixture_jwks("old");
        let rotated = fixture_jwks("new");
        let (address, server) = tls_sequence_server(&material, |address| {
            vec![
                json_response(&format!(
                    r#"{{"issuer":"https://{FIXTURE_HOST}:{}","jwks_uri":"https://{FIXTURE_HOST}:{}/jwks"}}"#,
                    address.port(),
                    address.port(),
                )),
                json_response(&initial),
                json_response(&rotated),
                json_response(r#"{"keys":[]}"#),
            ]
        })
        .await;
        let issuer = format!("https://{FIXTURE_HOST}:{}", address.port());
        let cancel = CancellationToken::new();
        let fixture =
            crate::test_support::FixtureTransport::new(FIXTURE_HOST, address, &material.root)
                .unwrap();
        let (verifier, refresh) = prepare_jwt_with_fixture(
            JwtOptions {
                issuer: issuer.clone(),
                audience: "service".to_owned(),
                token_profile: TokenProfile::ResourceServer,
            },
            fixture,
            cancel.child_token(),
        )
        .await
        .unwrap();
        let Verifier::Jwt(jwt) = &verifier else {
            unreachable!();
        };
        let worker = tokio::spawn(refresh);

        jwt.refresh.permit_unknown_refresh_for_test().await;
        let rotated = signed_token(&issuer, "new", "service");
        let rotated_header = format!("Bearer {rotated}");
        let rotated = parse_bearer([rotated_header.as_bytes()]).unwrap();
        assert!(
            verifier
                .verify(&rotated, Instant::now() + Duration::from_secs(1))
                .await
                .is_ok()
        );

        jwt.refresh.permit_unknown_refresh_for_test().await;
        let missing = signed_token(&issuer, "missing", "service");
        let missing_header = format!("Bearer {missing}");
        let missing = parse_bearer([missing_header.as_bytes()]).unwrap();
        assert_eq!(
            verifier
                .verify(&missing, Instant::now() + Duration::from_secs(1))
                .await,
            Err(Failure::Unavailable)
        );
        let retained = signed_token(&issuer, "new", "service");
        let retained_header = format!("Bearer {retained}");
        let retained = parse_bearer([retained_header.as_bytes()]).unwrap();
        assert!(
            verifier
                .verify(&retained, Instant::now() + Duration::from_secs(1))
                .await
                .is_ok()
        );

        cancel.cancel();
        worker.await.unwrap();
        let requests = server.await.unwrap();
        assert!(
            String::from_utf8_lossy(&requests[0])
                .starts_with("GET /.well-known/openid-configuration HTTP/1.1")
        );
        assert!(String::from_utf8_lossy(&requests[1]).starts_with("GET /jwks HTTP/1.1"));
        assert!(String::from_utf8_lossy(&requests[2]).starts_with("GET /jwks HTTP/1.1"));
        assert!(String::from_utf8_lossy(&requests[3]).starts_with("GET /jwks HTTP/1.1"));
    }
}
