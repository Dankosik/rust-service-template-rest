use std::sync::Arc;

use crate::claims::Scopes;

#[derive(Clone, derive_more::Debug)]
#[debug("Principal([VERIFIED_IDENTITY_REDACTED])")]
pub struct Principal {
    identity: Arc<Identity>,
    expiry_epoch_seconds: u64,
}

pub(crate) struct Identity {
    pub(crate) issuer: String,
    pub(crate) subject: Option<String>,
    pub(crate) client_id: Option<String>,
    pub(crate) scopes: Scopes,
    pub(crate) payload: String,
    pub(crate) actor: Option<Actor>,
}

impl Principal {
    pub(crate) fn new(identity: Identity, expiry_epoch_seconds: u64) -> Self {
        Self {
            identity: Arc::new(identity),
            expiry_epoch_seconds,
        }
    }

    pub fn claims<T: serde::de::DeserializeOwned>(&self) -> Result<T, ClaimAccessError> {
        serde_json::from_str(&self.identity.payload).map_err(|_| ClaimAccessError::InvalidShape)
    }

    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.identity.issuer
    }

    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.identity.subject.as_deref()
    }

    #[must_use]
    pub fn client_id(&self) -> Option<&str> {
        self.identity.client_id.as_deref()
    }

    #[must_use]
    pub fn scopes(&self) -> &[String] {
        self.identity.scopes.as_slice()
    }

    #[must_use]
    pub fn has_scopes(&self, required: &[String]) -> bool {
        self.identity.scopes.contains_all(required)
    }

    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expiry_epoch_seconds
    }

    /// RFC 8693 §4.1
    #[must_use]
    pub fn actor(&self) -> Option<&Actor> {
        self.identity.actor.as_ref()
    }
}

#[cfg(test)]
impl Principal {
    pub(crate) fn fields(&self) -> impl PartialEq + std::fmt::Debug + '_ {
        let identity = &*self.identity;
        (
            (&identity.issuer, &identity.subject, &identity.client_id),
            (self.scopes(), &identity.payload),
            (&identity.actor, self.expiry_epoch_seconds),
        )
    }
}

/// RFC 8693 §4.1
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
