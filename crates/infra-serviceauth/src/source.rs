//! The outbound token source: this service's signed tokens, one handle per
//! receiver.

use std::{
    fmt,
    sync::{Arc, Mutex, PoisonError},
};

use http::HeaderValue;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use secrecy::{ExposeSecret as _, SecretString};

use crate::{CallerName, JwtType, MAX_TOKEN_LIFETIME_SECONDS, admissible_key_id};

/// A handle reuses its token while it is younger than this, half its
/// lifetime, so a receiver never sees one within 30 seconds of expiry.
const REUSE_SECONDS: u64 = MAX_TOKEN_LIFETIME_SECONDS / 2;

/// This service's signing identity, from its configuration snapshot.
#[derive(Clone)]
pub struct SigningOptions {
    /// This service's name: its tokens' `iss` and `sub`.
    pub name: CallerName,
    pub jwt_type: JwtType,
    /// The `kid` under which receivers' trust registries hold the public key.
    pub key_id: String,
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
    #[error("the key id is not 1 to 256 visible ASCII characters")]
    InvalidKeyId,
    #[error("the receiver is not a valid caller name")]
    InvalidReceiver,
    #[error("service authentication is enabled but this service has no signing key")]
    NoSigningKey,
    #[error("the system clock precedes the Unix epoch")]
    Clock,
    #[error("a service token could not be signed")]
    Signing,
}

/// This process's outbound token source. Cloning shares it.
#[derive(Clone)]
pub struct TokenSource(Signing);

#[derive(Clone)]
enum Signing {
    /// Service authentication is off: calls carry no token.
    Disabled,
    /// Callers are verified, but this service signs nothing.
    Unconfigured,
    Key(Arc<Signer>),
}

impl fmt::Debug for TokenSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match &self.0 {
            Signing::Disabled => "TokenSource(disabled)",
            Signing::Unconfigured => "TokenSource(no signing key)",
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
        Self(Signing::Unconfigured)
    }

    /// Prepares signing without I/O; signing one token proves the key.
    ///
    /// # Errors
    /// Returns [`SigningError::InvalidKey`] for a key that does not sign
    /// ES256 and [`SigningError::InvalidKeyId`] for an unusable key id.
    pub fn new(options: SigningOptions) -> Result<Self, SigningError> {
        if !admissible_key_id(&options.key_id) {
            return Err(SigningError::InvalidKeyId);
        }
        let key = EncodingKey::from_ec_pem(options.private_key.expose_secret().as_bytes())
            .map_err(|_| SigningError::InvalidKey)?;
        let mut header = Header::new(Algorithm::ES256);
        header.typ = Some(options.jwt_type.as_str().to_owned());
        header.kid = Some(options.key_id);
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

    /// A handle that signs tokens addressed to `receiver`. Composition asks
    /// once per outbound client; each handle reuses its own token.
    ///
    /// # Errors
    /// Returns [`SigningError::InvalidReceiver`] for a name outside the
    /// caller grammar and [`SigningError::NoSigningKey`] when this service
    /// verifies callers but was given no signing key.
    pub fn receiver(&self, receiver: &str) -> Result<ReceiverTokens, SigningError> {
        let audience = CallerName::parse(receiver).map_err(|_| SigningError::InvalidReceiver)?;
        let signer = match &self.0 {
            Signing::Disabled => None,
            Signing::Unconfigured => return Err(SigningError::NoSigningKey),
            Signing::Key(signer) => Some(Arc::clone(signer)),
        };
        Ok(ReceiverTokens {
            signer,
            audience,
            cached: Arc::new(Mutex::new(None)),
        })
    }
}

/// Tokens for one receiver. Cloning shares the cached token.
#[derive(Clone)]
pub struct ReceiverTokens {
    signer: Option<Arc<Signer>>,
    audience: CallerName,
    cached: Arc<Mutex<Option<Cached>>>,
}

struct Cached {
    issued_at: u64,
    header: HeaderValue,
}

impl fmt::Debug for ReceiverTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceiverTokens")
            .field("receiver", &self.audience)
            .field("signing", &self.signer.is_some())
            .finish_non_exhaustive()
    }
}

impl ReceiverTokens {
    /// The receiver these tokens are addressed to.
    #[must_use]
    pub fn receiver(&self) -> &str {
        self.audience.as_str()
    }

    /// The `Authorization` value for the next call, marked sensitive, or
    /// `None` while service authentication is off.
    ///
    /// # Errors
    /// Returns [`SigningError::Clock`] when the system clock precedes the
    /// Unix epoch.
    pub fn authorization(&self) -> Result<Option<HeaderValue>, SigningError> {
        if self.signer.is_none() {
            return Ok(None);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| SigningError::Clock)?
            .as_secs();
        self.authorization_at(now).map(Some)
    }

    fn authorization_at(&self, now: u64) -> Result<HeaderValue, SigningError> {
        let Some(signer) = &self.signer else {
            return Err(SigningError::NoSigningKey);
        };
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(token) = cached
            .as_ref()
            .filter(|token| (token.issued_at..token.issued_at + REUSE_SECONDS).contains(&now))
        {
            return Ok(token.header.clone());
        }
        let mut header = HeaderValue::try_from(format!(
            "Bearer {}",
            signer.sign(self.audience.as_str(), now)?
        ))
        .map_err(|_| SigningError::Signing)?;
        header.set_sensitive(true);
        *cached = Some(Cached {
            issued_at: now,
            header: header.clone(),
        });
        Ok(header)
    }
}

struct Signer {
    key: EncodingKey,
    header: Header,
    name: CallerName,
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
    fn sign(&self, audience: &str, now: u64) -> Result<String, SigningError> {
        let claims = Claims {
            iss: self.name.as_str(),
            sub: self.name.as_str(),
            aud: audience,
            iat: now,
            exp: now + MAX_TOKEN_LIFETIME_SECONDS,
        };
        jsonwebtoken::encode(&self.header, &claims, &self.key).map_err(|_| SigningError::Signing)
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
            name: CallerName::parse("consultant").unwrap(),
            jwt_type: JwtType::parse("example-svc+jwt").unwrap(),
            key_id: key.kid().to_owned(),
            private_key: key.pem().into(),
        }
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
        let key = TestKey::new("consultant/2026-10");
        let source = TokenSource::new(options(&key)).unwrap();
        let catalog = source.receiver("catalog").unwrap();
        let header = catalog.authorization_at(NOW).unwrap();
        assert!(header.is_sensitive());
        let token = token(&header);
        let parts: Vec<&str> = token.split('.').collect();
        let decoded = |part: &str| -> serde_json::Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
        };
        assert_eq!(
            decoded(parts[0]),
            serde_json::json!({"alg": "ES256", "typ": "example-svc+jwt", "kid": "consultant/2026-10"})
        );
        let claims = decoded(parts[1]);
        assert_eq!(claims["iss"], "consultant");
        assert_eq!(claims["sub"], "consultant");
        assert_eq!(claims["aud"], "catalog");
        assert_eq!(claims["iat"], NOW);
        assert_eq!(claims["exp"], NOW + 60);
        assert_eq!(claims.as_object().unwrap().len(), 5, "{claims}");
        let verifier = Verifier::new(
            CallerName::parse("catalog").unwrap(),
            JwtType::parse("example-svc+jwt").unwrap(),
            TrustRegistry::parse(&registry(&[("consultant", &[&key])])).unwrap(),
        );
        assert_eq!(
            verifier
                .verify(token.as_bytes(), || Ok(NOW + 59))
                .unwrap()
                .name(),
            "consultant"
        );
        // A receiver verifies only tokens addressed to it.
        let other = Verifier::new(
            CallerName::parse("console").unwrap(),
            JwtType::parse("example-svc+jwt").unwrap(),
            TrustRegistry::parse(&registry(&[("consultant", &[&key])])).unwrap(),
        );
        assert!(other.verify(token.as_bytes(), || Ok(NOW)).is_err());
    }

    #[test]
    fn a_handle_reuses_its_token_for_half_its_lifetime() {
        let key = TestKey::new("consultant/2026-10");
        let catalog = TokenSource::new(options(&key))
            .unwrap()
            .receiver("catalog")
            .unwrap();
        let first = catalog.authorization_at(NOW).unwrap();
        let shared = catalog.clone();
        assert_eq!(shared.authorization_at(NOW + 29).unwrap(), first);
        let renewed = catalog.authorization_at(NOW + 30).unwrap();
        assert_ne!(renewed, first);
        // A clock that steps back signs anew rather than reuse a future token.
        assert_ne!(catalog.authorization_at(NOW + 10).unwrap(), renewed);
        // Handles for another receiver keep their own token.
        let llm = TokenSource::new(options(&key))
            .unwrap()
            .receiver("llm-gateway")
            .unwrap();
        assert_ne!(llm.authorization_at(NOW).unwrap(), first);
    }

    #[test]
    fn composition_errors_are_named_and_keys_never_shown() {
        let key = TestKey::new("consultant/2026-10");
        let mut invalid = options(&key);
        invalid.private_key = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"
            .to_owned()
            .into();
        assert_eq!(
            TokenSource::new(invalid).unwrap_err(),
            SigningError::InvalidKey
        );
        let mut invalid = options(&key);
        invalid.key_id = "two words".to_owned();
        assert_eq!(
            TokenSource::new(invalid).unwrap_err(),
            SigningError::InvalidKeyId
        );
        let source = TokenSource::new(options(&key)).unwrap();
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
        assert_eq!(
            format!("{catalog:?}"),
            r#"ReceiverTokens { receiver: "catalog", signing: true, .. }"#
        );
    }
}
