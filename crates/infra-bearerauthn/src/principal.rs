use std::sync::Arc;

use crate::claims::Scopes;

/// A verified identity. Construction remains crate-private so callers cannot
/// attach unverified request data as a principal.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("Principal([VERIFIED_IDENTITY_REDACTED])")]
pub struct Principal {
    identity: Arc<Identity>,
    // Keep expiry outside the shared allocation for the const accessor.
    expiry_epoch_seconds: u64,
}

/// The verified evidence every engine supplies for one principal.
pub(crate) struct Identity {
    pub(crate) issuer: String,
    pub(crate) subject: Option<String>,
    pub(crate) client_id: Option<String>,
    pub(crate) scopes: Scopes,
    pub(crate) payload: String,
    pub(crate) access_token: secrecy::SecretString,
    pub(crate) actor: Option<Actor>,
}

// `secrecy::SecretString` intentionally has no `PartialEq`, so this compares
// its exposed text alongside the other verified fields.
impl PartialEq for Identity {
    fn eq(&self, other: &Self) -> bool {
        use secrecy::ExposeSecret;
        let Self {
            issuer,
            subject,
            client_id,
            scopes,
            payload,
            access_token,
            actor,
        } = self;
        issuer == &other.issuer
            && subject == &other.subject
            && client_id == &other.client_id
            && scopes == &other.scopes
            && payload == &other.payload
            && access_token.expose_secret() == other.access_token.expose_secret()
            && actor == &other.actor
    }
}

impl Eq for Identity {}

impl Principal {
    pub(crate) fn new(identity: Identity, expiry_epoch_seconds: u64) -> Self {
        Self {
            identity: Arc::new(identity),
            expiry_epoch_seconds,
        }
    }

    /// Deserializes immutable claims from the accepted provider evidence.
    /// Duplicate custom members use the last value, before decoding the requested type.
    ///
    /// # Errors
    /// Returns a sanitized error when the application type cannot read the claims.
    pub fn claims<T: serde::de::DeserializeOwned>(&self) -> Result<T, ClaimAccessError> {
        // The Value pass preserves last-member-wins semantics. Decoding T directly
        // from JSON would reject duplicate fields in application structs.
        let value: serde_json::Value = serde_json::from_str(&self.identity.payload)
            .map_err(|_| ClaimAccessError::InvalidShape)?;
        serde_json::from_value(value).map_err(|_| ClaimAccessError::InvalidShape)
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.identity.issuer
    }

    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.identity.subject.as_deref()
    }

    /// The first non-empty `client_id`, `azp`, `appid`, or `cid` claim of a
    /// JWT, or the `client_id` member of an introspection response. An empty
    /// value is absent. A principal always has a subject, a client id, or both.
    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.identity.client_id.as_deref()
    }

    /// The normalized, exact-case scopes from verified token evidence.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.identity.scopes.as_slice()
    }

    /// True when every scope in `required` was granted; an empty `required`
    /// is any authenticated caller.
    #[must_use]
    pub fn has_scopes(&self, required: &[String]) -> bool {
        self.identity.scopes.contains_all(required)
    }

    /// The verified `exp` claim as Unix time in seconds, without leeway.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expiry_epoch_seconds
    }

    /// The exact bearer token text this principal was verified from. It is
    /// the subject token of an RFC 8693 exchange; never forward it as an
    /// outbound `Authorization` header.
    #[must_use]
    pub fn access_token(&self) -> &secrecy::SecretString {
        &self.identity.access_token
    }

    /// The outermost RFC 8693 `act` claim's identity, when the verified
    /// evidence named one. Nested actors are not exposed.
    #[must_use]
    pub fn actor(&self) -> Option<&Actor> {
        self.identity.actor.as_ref()
    }
}

/// The current actor delegated to act on a subject's behalf (RFC 8693 §4.1).
/// `may_act` is an authorization-server input, not receiver evidence, and is
/// never exposed here.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("Actor([REDACTED])")]
pub struct Actor {
    subject: String,
    client_id: Option<String>,
}

impl Actor {
    pub(crate) fn new(subject: String, client_id: Option<String>) -> Self {
        Self { subject, client_id }
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
}

/// Typed claim access failed without exposing the application type or payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClaimAccessError {
    #[error("verified claims do not match the requested type")]
    InvalidShape,
}

// template:begin oidc-introspection:authn-retained-payload
impl Principal {
    pub(crate) fn payload_len(&self) -> usize {
        self.identity.payload.len()
    }
}
// template:end oidc-introspection:authn-retained-payload
