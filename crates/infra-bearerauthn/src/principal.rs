use std::sync::Arc;

use indexmap::IndexMap;
use serde::Deserialize;

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
    pub(crate) access_token: secrecy::SecretString,
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

    #[must_use]
    pub fn access_token(&self) -> &secrecy::SecretString {
        &self.identity.access_token
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
        use secrecy::ExposeSecret as _;
        let identity = &*self.identity;
        (
            (&identity.issuer, &identity.subject, &identity.client_id),
            (
                self.scopes(),
                &identity.payload,
                identity.access_token.expose_secret(),
            ),
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

const MAX_CLAIM_NESTING: u8 = 127;

enum ClaimValue<'a> {
    Object(IndexMap<String, Self, foldhash::fast::RandomState>),
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
                let mut members = IndexMap::with_hasher(foldhash::fast::RandomState::default());
                if !self.take('}') {
                    loop {
                        let (key, _) = self.scalar::<String>()?;
                        if !self.take(':') {
                            return Err(ClaimAccessError::InvalidShape);
                        }
                        members.insert(key, self.value(depth - 1)?);
                        if self.take('}') {
                            break;
                        }
                        if !self.take(',') {
                            return Err(ClaimAccessError::InvalidShape);
                        }
                    }
                }
                Ok(ClaimValue::Object(members))
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
