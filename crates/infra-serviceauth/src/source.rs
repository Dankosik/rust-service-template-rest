use std::{fmt, sync::Arc};

use arc_swap::ArcSwapOption;
use http::HeaderValue;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretString};
use zeroize::Zeroizing;

use crate::{JwtType, KeyId, MAX_TOKEN_LIFETIME_SECONDS, ServiceName, unix_seconds};

/// A handle reuses its token while it is younger than this, so every token it
/// sends has more than half its lifetime left by this service's clock.
const REUSE_SECONDS: u64 = MAX_TOKEN_LIFETIME_SECONDS / 2;

#[derive(Clone)]
pub struct SigningOptions {
    pub name: ServiceName,
    pub jwt_type: JwtType,
    /// The `kid` under which receivers' trust registries hold the public key.
    pub key_id: KeyId,
    /// PKCS#8 PEM of an EC P-256 private key.
    pub private_key: SecretString,
}

impl fmt::Debug for SigningOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SigningOptions")
            .field("name", &self.name)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// Why a token source or a token could not be produced. Never key material.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SigningError {
    #[error("the signing key is not a PKCS#8 PEM EC P-256 private key")]
    InvalidKey,
    #[error("the receiver is not a valid service name")]
    InvalidReceiver,
    #[error("service authentication is enabled but this service has no signing key")]
    NoSigningKey,
    #[error("the system clock precedes the Unix epoch")]
    Clock,
    #[error("a service token could not be signed")]
    Signing,
}

#[derive(Clone)]
pub struct TokenSource(Signing);

#[derive(Clone)]
enum Signing {
    Disabled,
    WithoutKey,
    Key(Arc<Signer>),
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match &self.0 {
            Signing::Disabled => "TokenSource(disabled)",
            Signing::WithoutKey => "TokenSource(no signing key)",
            Signing::Key(_) => "TokenSource([REDACTED])",
        })
    }
}

impl TokenSource {
    /// The source while service authentication is off: every handle sends
    /// no token.
    #[must_use]
    pub const fn disabled() -> Self {
        Self(Signing::Disabled)
    }

    /// The source of a service that verifies callers but holds no signing
    /// key: asking it for a receiver handle is a composition error.
    #[must_use]
    pub const fn without_key() -> Self {
        Self(Signing::WithoutKey)
    }

    pub fn try_new(options: SigningOptions) -> Result<Self, SigningError> {
        let key = EncodingKey::from_ec_pem(options.private_key.expose_secret().as_bytes())
            .map_err(|_| SigningError::InvalidKey)?;
        let mut header = Header::new(Algorithm::ES256);
        header.typ = Some(options.jwt_type.as_str().to_owned());
        header.kid = Some(options.key_id.as_str().to_owned());
        let signer = Signer {
            key,
            header,
            name: options.name,
        };
        signer
            .sign(signer.name.as_str(), 0)
            .map_err(|_| SigningError::InvalidKey)?;
        Ok(Self(Signing::Key(Arc::new(signer))))
    }

    /// A handle that signs tokens addressed to `receiver`, one per outbound client.
    pub fn receiver(&self, receiver: &str) -> Result<ReceiverTokens, SigningError> {
        let audience = ServiceName::parse(receiver).map_err(|_| SigningError::InvalidReceiver)?;
        let state = match &self.0 {
            Signing::Disabled => ReceiverState::Disabled(audience),
            Signing::WithoutKey => return Err(SigningError::NoSigningKey),
            Signing::Key(signer) => ReceiverState::Active(Arc::new(ActiveReceiver {
                signer: Arc::clone(signer),
                audience,
                cached: ArcSwapOption::empty(),
                refresh: Mutex::new(()),
            })),
        };
        Ok(ReceiverTokens(state))
    }
}

#[derive(Clone)]
pub struct ReceiverTokens(ReceiverState);

#[derive(Clone)]
enum ReceiverState {
    Disabled(ServiceName),
    Active(Arc<ActiveReceiver>),
}

struct ActiveReceiver {
    signer: Arc<Signer>,
    audience: ServiceName,
    cached: ArcSwapOption<Cached>,
    refresh: Mutex<()>,
}

struct Cached {
    issued_at: u64,
    authorization: HeaderValue,
}

impl fmt::Debug for ReceiverTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (audience, signing) = match &self.0 {
            ReceiverState::Disabled(audience) => (audience, false),
            ReceiverState::Active(active) => (&active.audience, true),
        };
        formatter
            .debug_struct("ReceiverTokens")
            .field("receiver", audience)
            .field("signing", &signing)
            .finish_non_exhaustive()
    }
}

impl ReceiverTokens {
    /// `None` while service authentication is off.
    pub fn authorization(&self) -> Result<Option<HeaderValue>, SigningError> {
        let ReceiverState::Active(active) = &self.0 else {
            return Ok(None);
        };
        let now = unix_seconds().ok_or(SigningError::Clock)?;
        active.authorization_at(now).map(Some)
    }
}

impl ActiveReceiver {
    fn reusable(cached: Option<&Arc<Cached>>, now: u64) -> Option<HeaderValue> {
        cached
            .filter(|token| (token.issued_at..token.issued_at + REUSE_SECONDS).contains(&now))
            .map(|token| token.authorization.clone())
    }

    fn authorization_at(&self, now: u64) -> Result<HeaderValue, SigningError> {
        if let Some(authorization) = Self::reusable(self.cached.load().as_ref(), now) {
            return Ok(authorization);
        }
        let _refresh = self.refresh.lock();
        if let Some(authorization) = Self::reusable(self.cached.load().as_ref(), now) {
            return Ok(authorization);
        }
        let token = self.signer.sign(self.audience.as_str(), now)?;
        let bearer = Zeroizing::new(format!("Bearer {}", token.as_str()));
        let mut authorization =
            HeaderValue::from_str(bearer.as_str()).map_err(|_| SigningError::Signing)?;
        authorization.set_sensitive(true);
        self.cached.store(Some(Arc::new(Cached {
            issued_at: now,
            authorization: authorization.clone(),
        })));
        Ok(authorization)
    }
}

struct Signer {
    key: EncodingKey,
    header: Header,
    name: ServiceName,
}

#[derive(serde::Serialize)]
struct Claims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    iat: u64,
    exp: u64,
}

impl Signer {
    fn sign(&self, audience: &str, now: u64) -> Result<Zeroizing<String>, SigningError> {
        let claims = Claims {
            iss: self.name.as_str(),
            sub: self.name.as_str(),
            aud: audience,
            iat: now,
            exp: now + MAX_TOKEN_LIFETIME_SECONDS,
        };
        jsonwebtoken::encode(&self.header, &claims, &self.key)
            .map(Zeroizing::new)
            .map_err(|_| SigningError::Signing)
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    use super::*;
    use crate::{
        TrustRegistry, Verifier,
        test_support::{TestKey, registry},
    };

    const NOW: u64 = 1_800_000_000;

    fn options(key: &TestKey) -> SigningOptions {
        SigningOptions {
            name: ServiceName::parse("orders").unwrap(),
            jwt_type: JwtType::parse("example-svc+jwt").unwrap(),
            key_id: KeyId::parse(key.kid()).unwrap(),
            private_key: key.pem().into(),
        }
    }

    fn at(tokens: &ReceiverTokens, now: u64) -> HeaderValue {
        let ReceiverState::Active(active) = &tokens.0 else {
            panic!("expected an active receiver");
        };
        active.authorization_at(now).unwrap()
    }

    fn token(header: &HeaderValue) -> String {
        header
            .to_str()
            .unwrap()
            .strip_prefix("Bearer ")
            .unwrap()
            .to_owned()
    }

    #[test]
    fn signed_tokens_meet_the_contract_the_receiver_verifies() {
        let key = TestKey::new("orders/2026-10");
        let source = TokenSource::try_new(options(&key)).unwrap();
        let catalog = source.receiver("catalog").unwrap();
        let header = at(&catalog, NOW);
        assert!(header.is_sensitive());
        let token = token(&header);
        let parts: Vec<&str> = token.split('.').collect();
        let decoded = |part: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
        };
        assert_eq!(
            decoded(parts[0]),
            serde_json::json!({"alg": "ES256", "typ": "example-svc+jwt", "kid": "orders/2026-10"})
        );
        let claims = decoded(parts[1]);
        assert_eq!(claims["iss"], "orders");
        assert_eq!(claims["sub"], "orders");
        assert_eq!(claims["aud"], "catalog");
        assert_eq!(claims["iat"], NOW);
        assert_eq!(claims["exp"], NOW + 60);
        assert_eq!(claims.as_object().unwrap().len(), 5, "{claims}");
        let verifier = Verifier::new(
            ServiceName::parse("catalog").unwrap(),
            JwtType::parse("example-svc+jwt").unwrap(),
            TrustRegistry::parse(&registry(&[("orders", &[&key])])).unwrap(),
        );
        assert_eq!(
            verifier
                .verify(token.as_bytes(), || Ok(NOW + 59))
                .unwrap()
                .name()
                .as_str(),
            "orders"
        );
        let other = Verifier::new(
            ServiceName::parse("billing").unwrap(),
            JwtType::parse("example-svc+jwt").unwrap(),
            TrustRegistry::parse(&registry(&[("orders", &[&key])])).unwrap(),
        );
        assert!(other.verify(token.as_bytes(), || Ok(NOW)).is_err());
    }

    #[test]
    fn a_handle_reuses_its_token_for_half_its_lifetime() {
        let key = TestKey::new("orders/2026-10");
        let source = TokenSource::try_new(options(&key)).unwrap();
        let catalog = source.receiver("catalog").unwrap();
        let first = at(&catalog, NOW);
        let shared = catalog.clone();
        assert_eq!(at(&shared, NOW + 29), first);
        let inventory = source.receiver("inventory").unwrap();
        assert_ne!(at(&inventory, NOW + 29), first);
        let another_catalog = source.receiver("catalog").unwrap();
        assert_ne!(at(&another_catalog, NOW + 29), first);
        assert_eq!(at(&catalog, NOW + 29), first);
        let renewed = at(&catalog, NOW + 30);
        assert_ne!(renewed, first);
        // A clock that steps back signs anew rather than reuse a future token.
        assert_ne!(at(&catalog, NOW + 10), renewed);
    }

    #[test]
    fn composition_errors_are_named_and_keys_never_shown() {
        let key = TestKey::new("orders/2026-10");
        let mut invalid = options(&key);
        invalid.private_key = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"
            .to_owned()
            .into();
        assert_eq!(
            TokenSource::try_new(invalid).unwrap_err(),
            SigningError::InvalidKey
        );
        let source = TokenSource::try_new(options(&key)).unwrap();
        assert_eq!(
            source.receiver("Catalog").unwrap_err(),
            SigningError::InvalidReceiver
        );
        assert_eq!(
            TokenSource::without_key().receiver("catalog").unwrap_err(),
            SigningError::NoSigningKey
        );
        let disabled = TokenSource::disabled().receiver("catalog").unwrap();
        assert_eq!(disabled.authorization(), Ok(None));
        assert!(format!("{disabled:?}").contains("catalog"));
        assert_eq!(
            TokenSource::disabled().receiver("Catalog").unwrap_err(),
            SigningError::InvalidReceiver
        );
        let catalog = source.receiver("catalog").unwrap();
        assert!(catalog.authorization().unwrap().is_some());
        for debug in [
            format!("{source:?}"),
            format!("{catalog:?}"),
            format!("{:?}", options(&key)),
        ] {
            assert!(
                !debug.contains("PRIVATE KEY") && !debug.contains("Bearer"),
                "{debug}"
            );
        }
    }
}
