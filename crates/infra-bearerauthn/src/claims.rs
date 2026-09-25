//! Typed claim decoding and application identity normalization.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer};

use crate::{Failure, Principal, TokenProfile};

const LEEWAY_SECONDS: u64 = 30;

#[derive(Clone)]
pub(crate) struct ClaimPolicy {
    issuer: String,
    audiences: Vec<String>,
}

impl ClaimPolicy {
    pub(crate) fn new(issuer: String, audiences: Vec<String>) -> Self {
        Self { issuer, audiences }
    }

    pub(crate) fn issuer(&self) -> &str {
        &self.issuer
    }

    pub(crate) fn audiences(&self) -> &[String] {
        &self.audiences
    }
}

/// A typed JWT payload. Serde's struct decoder rejects duplicate consumed fields
/// while leaving unconsumed extension claims alone.
#[derive(Deserialize)]
pub(crate) struct JwtClaims {
    pub(crate) iss: String,
    pub(crate) aud: Audience,
    pub(crate) exp: u64,
    #[serde(default)]
    pub(crate) nbf: Nullable<u64>,
    #[serde(default)]
    pub(crate) sub: Option<String>,
    #[serde(default)]
    pub(crate) client_id: Option<String>,
    #[serde(default)]
    pub(crate) azp: Option<String>,
    #[serde(default)]
    pub(crate) appid: Option<String>,
    #[serde(default)]
    pub(crate) cid: Option<String>,
    #[serde(default)]
    pub(crate) scope: Nullable<String>,
    #[serde(default)]
    pub(crate) scp: Nullable<Vec<String>>,
    #[serde(default)]
    pub(crate) jti: Option<String>,
    #[serde(default)]
    pub(crate) iat: Option<u64>,
}

#[derive(Deserialize)]
struct ActiveEnvelope {
    active: bool,
}

#[derive(Deserialize)]
struct IntrospectionClaims {
    active: bool,
    iss: String,
    aud: Audience,
    exp: u64,
    #[serde(default)]
    nbf: Nullable<u64>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    scope: Nullable<String>,
    #[serde(default)]
    scp: Nullable<Vec<String>>,
}

/// Preserves the distinction between an absent claim and a supplied JSON null.
pub(crate) enum Nullable<T> {
    Missing,
    Null,
    Value(T),
}

impl<T> Default for Nullable<T> {
    fn default() -> Self {
        Self::Missing
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Nullable<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn values(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

/// Validates post-signature JWT identity/profile evidence.
pub(crate) fn validate_jwt_claims(
    claims: JwtClaims,
    policy: &ClaimPolicy,
    token_profile: TokenProfile,
    now_epoch_seconds: u64,
) -> Result<Principal, Failure> {
    let common = validate_common(
        &claims.iss,
        &claims.aud,
        claims.exp,
        &claims.nbf,
        claims.sub.as_deref(),
        claims.client_id.as_deref(),
        &claims.scope,
        &claims.scp,
        policy,
        now_epoch_seconds,
        Failure::Invalid,
        true,
    )?;
    let aliases = [
        claims.client_id.as_deref(),
        claims.azp.as_deref(),
        claims.appid.as_deref(),
        claims.cid.as_deref(),
    ];
    let client_id = coherent_identity(aliases, Failure::Invalid)?;
    if common.subject.is_none() && client_id.is_none() {
        return Err(Failure::Invalid);
    }
    if token_profile == TokenProfile::Rfc9068 {
        if common.subject.is_none()
            || claims
                .client_id
                .as_deref()
                .and_then(nonempty_identity)
                .is_none()
            || claims
                .jti
                .as_deref()
                .filter(|value| !value.is_empty())
                .is_none()
            || claims
                .iat
                .is_none_or(|iat| iat > now_epoch_seconds.saturating_add(LEEWAY_SECONDS))
        {
            return Err(Failure::Invalid);
        }
    }
    Ok(Principal::new(
        policy.issuer.clone(),
        common.subject,
        client_id,
        common.scopes,
        claims.exp,
    ))
}

/// Classifies a typed RFC 7662 response without exposing provider details.
pub(crate) fn validate_introspection_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    now_epoch_seconds: u64,
) -> Result<Principal, Failure> {
    let envelope: ActiveEnvelope =
        serde_json::from_slice(bytes).map_err(|_| Failure::Unavailable)?;
    if !envelope.active {
        return Err(Failure::Invalid);
    }
    let claims: IntrospectionClaims =
        serde_json::from_slice(bytes).map_err(|_| Failure::Unavailable)?;
    if !claims.active {
        return Err(Failure::Invalid);
    }
    let common = validate_common(
        &claims.iss,
        &claims.aud,
        claims.exp,
        &claims.nbf,
        claims.sub.as_deref(),
        claims.client_id.as_deref(),
        &claims.scope,
        &claims.scp,
        policy,
        now_epoch_seconds,
        Failure::Unavailable,
        true,
    )?;
    Ok(Principal::new(
        policy.issuer.clone(),
        common.subject,
        common.client_id,
        common.scopes,
        claims.exp,
    ))
}

struct CommonClaims {
    subject: Option<String>,
    client_id: Option<String>,
    scopes: Vec<String>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "the typed claim boundary keeps evidence classification explicit"
)]
fn validate_common(
    issuer: &str,
    audience: &Audience,
    expiry: u64,
    not_before: &Nullable<u64>,
    subject: Option<&str>,
    client_id: Option<&str>,
    scope: &Nullable<String>,
    scp: &Nullable<Vec<String>>,
    policy: &ClaimPolicy,
    now: u64,
    malformed: Failure,
    validate_time: bool,
) -> Result<CommonClaims, Failure> {
    if issuer.is_empty()
        || issuer != policy.issuer
        || audience.values().is_empty()
        || audience.values().iter().any(String::is_empty)
        || !audience
            .values()
            .iter()
            .any(|value| policy.audiences.iter().any(|audience| audience == value))
    {
        return Err(Failure::Invalid);
    }
    let subject = subject
        .map(|value| identity(value, malformed))
        .transpose()?
        .map(ToOwned::to_owned);
    let client_id = client_id
        .map(|value| identity(value, malformed))
        .transpose()?
        .map(ToOwned::to_owned);
    if subject.is_none() && client_id.is_none() {
        return Err(malformed);
    }
    let nbf = match not_before {
        Nullable::Missing => None,
        Nullable::Null => return Err(malformed),
        Nullable::Value(value) => Some(*value),
    };
    if validate_time
        && (now > expiry.saturating_add(LEEWAY_SECONDS)
            || nbf.is_some_and(|value| value > now.saturating_add(LEEWAY_SECONDS)))
    {
        return Err(Failure::Invalid);
    }
    Ok(CommonClaims {
        subject,
        client_id,
        scopes: normalize_scopes(scope, scp, malformed)?,
    })
}

fn coherent_identity(
    values: [Option<&str>; 4],
    malformed: Failure,
) -> Result<Option<String>, Failure> {
    let mut identities = values
        .into_iter()
        .flatten()
        .map(|value| identity(value, malformed));
    let Some(first) = identities.next().transpose()? else {
        return Ok(None);
    };
    for value in identities {
        if value? != first {
            return Err(malformed);
        }
    }
    Ok(Some(first.to_owned()))
}

fn identity(value: &str, malformed: Failure) -> Result<&str, Failure> {
    if nonempty_identity(value).is_none() {
        Err(malformed)
    } else {
        Ok(value)
    }
}

fn nonempty_identity(value: &str) -> Option<&str> {
    (!value.is_empty() && value.trim() == value).then_some(value)
}

fn normalize_scopes(
    scope: &Nullable<String>,
    scp: &Nullable<Vec<String>>,
    malformed: Failure,
) -> Result<Vec<String>, Failure> {
    let from_scope = match scope {
        Nullable::Missing | Nullable::Null => BTreeSet::new(),
        Nullable::Value(value) => parse_scope_string(value, malformed)?,
    };
    let from_scp = match scp {
        Nullable::Missing | Nullable::Null => BTreeSet::new(),
        Nullable::Value(values) => values
            .iter()
            .map(|value| scope_token(value, malformed).map(ToOwned::to_owned))
            .collect::<Result<BTreeSet<_>, _>>()?,
    };
    if !from_scope.is_empty() && !from_scp.is_empty() && from_scope != from_scp {
        return Err(malformed);
    }
    Ok(if from_scope.is_empty() {
        from_scp
    } else {
        from_scope
    }
    .into_iter()
    .collect())
}

fn parse_scope_string(value: &str, malformed: Failure) -> Result<BTreeSet<String>, Failure> {
    if value.is_empty() {
        return Ok(BTreeSet::new());
    }
    value
        .split(' ')
        .map(|part| scope_token(part, malformed).map(ToOwned::to_owned))
        .collect()
}

fn scope_token(value: &str, malformed: Failure) -> Result<&str, Failure> {
    if value.is_empty()
        || !value.bytes().all(|byte| {
            byte == b'!' || (b'#'..=b'[').contains(&byte) || (b']'..=b'~').contains(&byte)
        })
    {
        return Err(malformed);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{ClaimPolicy, JwtClaims, validate_introspection_claims, validate_jwt_claims};
    use crate::{Failure, TokenProfile};

    fn policy() -> ClaimPolicy {
        ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()])
    }

    #[test]
    fn typed_jwt_claims_normalize_matching_scope_forms() {
        let claims: JwtClaims = serde_json::from_str(r#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject","scope":"read write read","scp":["write","read"]}"#).unwrap();
        let principal =
            validate_jwt_claims(claims, &policy(), TokenProfile::ResourceServer, 100).unwrap();
        assert_eq!(principal.scopes(), ["read", "write"]);
        assert_eq!(principal.expires_at(), 130);
    }

    #[test]
    fn provider_claim_shape_and_scope_disagreement_remain_unavailable() {
        let response = br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject","scope":"read","scp":["write"]}"#;
        assert_eq!(
            validate_introspection_claims(response, &policy(), 100),
            Err(Failure::Unavailable)
        );
        assert_eq!(
            validate_introspection_claims(br#"{"active":false,"iss":false}"#, &policy(), 100),
            Err(Failure::Invalid)
        );
        assert_eq!(validate_introspection_claims(br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"nbf":null,"sub":"subject"}"#, &policy(), 100), Err(Failure::Unavailable));
    }
}
