use std::{collections::BTreeMap, sync::Arc};

use serde::Deserialize;

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
        let invalid_shape = |_| ClaimAccessError::InvalidShape;
        let mut input = ClaimJson(&self.identity.payload);
        let claims = input.value(MAX_CLAIM_NESTING)?;
        if !input.remaining().is_empty() {
            return Err(ClaimAccessError::InvalidShape);
        }
        let mut json = Vec::with_capacity(self.identity.payload.len());
        claims.write_json(&mut json).map_err(invalid_shape)?;
        serde_json::from_slice(&json).map_err(invalid_shape)
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

// Keep serde_json's ordinary limit of 127 nested containers.
const MAX_CLAIM_NESTING: u8 = 127;

enum ClaimValue<'a> {
    Object(Vec<(String, Self)>),
    Array(Vec<Self>),
    Scalar(&'a str),
}

impl ClaimValue<'_> {
    fn write_json(&self, json: &mut Vec<u8>) -> serde_json::Result<()> {
        match self {
            Self::Object(members) => {
                json.push(b'{');
                for (index, (key, value)) in members.iter().enumerate() {
                    if index != 0 {
                        json.push(b',');
                    }
                    serde_json::to_writer(&mut *json, key)?;
                    json.push(b':');
                    value.write_json(json)?;
                }
                json.push(b'}');
            }
            Self::Array(items) => {
                json.push(b'[');
                for (index, value) in items.iter().enumerate() {
                    if index != 0 {
                        json.push(b',');
                    }
                    value.write_json(json)?;
                }
                json.push(b']');
            }
            Self::Scalar(raw) => json.extend_from_slice(raw.as_bytes()),
        }
        Ok(())
    }
}

// Walk containers to collapse duplicate members. Serde owns the scalar and
// key grammar, and the final typed decode sees the original numeric text.
struct ClaimJson<'a>(&'a str);

impl<'a> ClaimJson<'a> {
    fn remaining(&self) -> &'a str {
        self.0.trim_start_matches([' ', '\t', '\r', '\n'])
    }

    fn take(&mut self, delimiter: char) -> bool {
        if let Some(rest) = self.remaining().strip_prefix(delimiter) {
            self.0 = rest;
            true
        } else {
            false
        }
    }

    fn scalar<T: Deserialize<'a>>(&mut self) -> Result<(T, &'a str), ClaimAccessError> {
        let input = self.remaining();
        let mut stream = serde_json::Deserializer::from_str(input).into_iter::<T>();
        let value = stream
            .next()
            .and_then(Result::ok)
            .ok_or(ClaimAccessError::InvalidShape)?;
        let (raw, rest) = input.split_at(stream.byte_offset());
        self.0 = rest;
        Ok((value, raw))
    }

    fn value(&mut self, depth: u8) -> Result<ClaimValue<'a>, ClaimAccessError> {
        match self.remaining().as_bytes().first() {
            Some(b'{' | b'[') if depth == 0 => Err(ClaimAccessError::InvalidShape),
            Some(b'{') => {
                self.take('{');
                let mut members = BTreeMap::new();
                let mut first = None;
                if !self.take('}') {
                    loop {
                        let (key, _) = self.scalar::<String>()?;
                        if !self.take(':') {
                            return Err(ClaimAccessError::InvalidShape);
                        }
                        first.get_or_insert_with(|| key.clone());
                        members.insert(key, self.value(depth - 1)?);
                        if self.take('}') {
                            break;
                        }
                        if !self.take(',') {
                            return Err(ClaimAccessError::InvalidShape);
                        }
                    }
                }
                // Serde JSON features can give the first object key special
                // meaning. Keep it first when decoding the normalized JSON.
                let first = first.and_then(|key| members.remove_entry(&key));
                Ok(ClaimValue::Object(
                    first.into_iter().chain(members).collect(),
                ))
            }
            Some(b'[') => {
                self.take('[');
                let mut items = Vec::new();
                if !self.take(']') {
                    loop {
                        items.push(self.value(depth - 1)?);
                        if self.take(']') {
                            break;
                        }
                        if !self.take(',') {
                            return Err(ClaimAccessError::InvalidShape);
                        }
                    }
                }
                Ok(ClaimValue::Array(items))
            }
            _ => self
                .scalar::<serde_json::Value>()
                .map(|(_, raw)| ClaimValue::Scalar(raw)),
        }
    }
}

// template:begin oidc-introspection:authn-retained-payload
impl Principal {
    pub(crate) fn payload_len(&self) -> usize {
        self.identity.payload.len()
    }
}
// template:end oidc-introspection:authn-retained-payload
