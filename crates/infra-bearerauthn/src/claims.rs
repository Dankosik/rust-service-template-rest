//! Typed claim decoding and application identity normalization.

use crate::{Failure, Principal, VerificationError, VerificationReason};
use serde::Deserialize;
use std::{collections::BTreeSet, sync::Arc};
// template:begin oidc-jwt:authn-claims-jwt-import
use crate::TokenProfile;
// template:end oidc-jwt:authn-claims-jwt-import

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
    // template:begin oidc-jwt:authn-claims-jwt-policy-accessors
    pub(crate) fn issuer(&self) -> &str {
        &self.issuer
    }
    pub(crate) fn audiences(&self) -> &[String] {
        &self.audiences
    }
    // template:end oidc-jwt:authn-claims-jwt-policy-accessors
}

// template:begin oidc-jwt:authn-claims-jwt-type
/// Serde checks the consumed shapes and duplicate fields; verified decode's
/// Validation owns issuer, audience, expiry and not-before checks.
#[derive(Deserialize)]
#[allow(
    clippy::option_option,
    reason = "serde double_option preserves missing and explicit null while derived fields reject duplicates"
)]
pub(crate) struct JwtClaims {
    #[serde(rename = "iss")]
    _issuer: String,
    #[serde(rename = "aud")]
    _audience: Audience,
    pub(crate) exp: u64,
    #[serde(default, rename = "nbf", with = "serde_with::rust::double_option")]
    _not_before: Option<Option<u64>>,
    #[serde(default)]
    sub: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    appid: Option<String>,
    #[serde(default)]
    cid: Option<String>,
    #[serde(default, with = "serde_with::rust::double_option")]
    scope: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    scp: Option<Option<serde_json::Value>>,
    #[serde(default)]
    jti: Option<String>,
    #[serde(default)]
    iat: Option<u64>,
}
// template:end oidc-jwt:authn-claims-jwt-type

// template:begin oidc-introspection:authn-claims-introspection-envelope
/// Keep the fixed consumed fields until active is known. This struct rejects
/// duplicates even for inactive responses without interpreting their claim types.
#[derive(Deserialize)]
#[allow(
    clippy::option_option,
    reason = "serde double_option preserves missing and explicit null while derived fields reject duplicates"
)]
struct ActiveEnvelope {
    active: bool,
    #[serde(default, with = "serde_with::rust::double_option")]
    iss: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    aud: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    exp: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    nbf: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    sub: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    client_id: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    scope: Option<Option<serde_json::Value>>,
    #[serde(default, with = "serde_with::rust::double_option")]
    scp: Option<Option<serde_json::Value>>,
}

#[allow(
    clippy::option_option,
    reason = "typed decoding must preserve the admitted field distinction between missing, null and value"
)]
fn typed_claim<T: serde::de::DeserializeOwned>(
    raw: Option<Option<serde_json::Value>>,
) -> Result<Option<Option<T>>, VerificationError> {
    raw.map(|value| value.map(serde_json::from_value).transpose())
        .transpose()
        .map_err(|_| {
            VerificationError::new(Failure::Unavailable, VerificationReason::MalformedClaims)
        })
}
// template:end oidc-introspection:authn-claims-introspection-envelope

#[derive(Deserialize)]
#[serde(untagged)]
#[allow(
    dead_code,
    reason = "JWT typed decoding checks this shape; library Validation reads its value"
)]
pub(crate) enum Audience {
    One(String),
    Many(Vec<String>),
}
// template:begin oidc-introspection:authn-claims-audience-values
impl Audience {
    fn values(&self) -> &[String] {
        match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}
// template:end oidc-introspection:authn-claims-audience-values

// template:begin oidc-jwt:authn-claims-jwt-validation
pub(crate) fn validate_jwt_claims(
    claims: &JwtClaims,
    policy: &ClaimPolicy,
    token_profile: TokenProfile,
    now: u64,
    payload: Arc<str>,
) -> Result<Principal, VerificationError> {
    let client_id = coherent_identity([
        claims.client_id.as_deref(),
        claims.azp.as_deref(),
        claims.appid.as_deref(),
        claims.cid.as_deref(),
    ])?;
    let subject = claims
        .sub
        .as_deref()
        .map(identity)
        .transpose()?
        .map(ToOwned::to_owned);
    if subject.is_none() && client_id.is_none() {
        return Err(VerificationError::invalid(VerificationReason::MissingClaim));
    }
    if token_profile == TokenProfile::Rfc9068 {
        if subject.is_none()
            || claims.client_id.as_deref().is_none_or(str::is_empty)
            || claims.jti.as_deref().is_none_or(str::is_empty)
            || claims.iat.is_none()
        {
            return Err(VerificationError::invalid(VerificationReason::MissingClaim));
        }
        if claims
            .iat
            .is_some_and(|iat| iat > now.saturating_add(LEEWAY_SECONDS))
        {
            return Err(VerificationError::invalid(VerificationReason::Profile));
        }
    }
    let scopes = normalize_scopes(
        claims.scope.as_ref().and_then(Option::as_ref),
        claims.scp.as_ref().and_then(Option::as_ref),
        Failure::Invalid,
    )?;
    Ok(Principal::new(
        policy.issuer.clone(),
        subject,
        client_id,
        scopes,
        claims.exp,
        payload,
    ))
}

fn coherent_identity(values: [Option<&str>; 4]) -> Result<Option<String>, VerificationError> {
    let mut identities = values.into_iter().flatten().map(identity);
    let Some(first) = identities.next().transpose()? else {
        return Ok(None);
    };
    for value in identities {
        if value? != first {
            return Err(VerificationError::invalid(VerificationReason::Identity));
        }
    }
    Ok(Some(first.to_owned()))
}
// template:end oidc-jwt:authn-claims-jwt-validation

// template:begin oidc-introspection:authn-claims-introspection-validation
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct VerifiedIntrospection {
    principal: Principal,
    not_before: Option<u64>,
}

impl VerifiedIntrospection {
    pub(crate) fn principal(&self) -> &Principal {
        &self.principal
    }

    pub(crate) fn into_principal(self) -> Principal {
        self.principal
    }

    pub(crate) fn validate_time(&self, now: u64) -> Result<(), VerificationError> {
        validate_introspection_time(self.principal.expires_at(), self.not_before, now)
    }
}

impl std::fmt::Debug for VerifiedIntrospection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("VerifiedIntrospection([REDACTED])")
    }
}

fn validate_introspection_time(
    expiry: u64,
    not_before: Option<u64>,
    now: u64,
) -> Result<(), VerificationError> {
    if now > expiry.saturating_add(LEEWAY_SECONDS) {
        return Err(VerificationError::invalid(VerificationReason::Expired));
    }
    if not_before.is_some_and(|value| value > now.saturating_add(LEEWAY_SECONDS)) {
        return Err(VerificationError::invalid(VerificationReason::NotYetValid));
    }
    Ok(())
}

pub(crate) fn validate_introspection_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    now: u64,
) -> Result<VerifiedIntrospection, VerificationError> {
    let malformed =
        || VerificationError::new(Failure::Unavailable, VerificationReason::MalformedClaims);
    let missing = || VerificationError::invalid(VerificationReason::MissingClaim);
    let payload: Box<serde_json::value::RawValue> =
        serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let claims: ActiveEnvelope = serde_json::from_str(payload.get()).map_err(|_| malformed())?;
    if !claims.active {
        return Err(VerificationError::invalid(VerificationReason::Inactive));
    }
    // Decode every supplied field before classifying missing evidence: a wrong
    // supplied type remains a provider failure even alongside an omitted claim.
    let issuer: Option<String> = typed_claim(claims.iss)?.flatten();
    let audience: Option<Audience> = typed_claim(claims.aud)?.flatten();
    let expiry: Option<Option<u64>> = typed_claim(claims.exp)?;
    let not_before: Option<Option<u64>> = typed_claim(claims.nbf)?;
    let subject: Option<String> = typed_claim(claims.sub)?.flatten();
    let client_id: Option<String> = typed_claim(claims.client_id)?.flatten();
    let expiry = match expiry {
        Some(None) => return Err(malformed()),
        None => None,
        Some(Some(value)) => Some(value),
    };
    let not_before = match not_before {
        Some(None) => return Err(malformed()),
        None => None,
        Some(Some(value)) => Some(value),
    };
    let scopes = normalize_scopes(
        claims.scope.as_ref().and_then(Option::as_ref),
        claims.scp.as_ref().and_then(Option::as_ref),
        Failure::Unavailable,
    )?;
    let (issuer, audience, expiry) = (
        issuer.ok_or_else(missing)?,
        audience.ok_or_else(missing)?,
        expiry.ok_or_else(missing)?,
    );
    if issuer.is_empty()
        || audience.values().is_empty()
        || audience.values().iter().any(String::is_empty)
    {
        return Err(missing());
    }
    if issuer != policy.issuer {
        return Err(VerificationError::invalid(VerificationReason::Issuer));
    }
    if !audience
        .values()
        .iter()
        .any(|value| policy.audiences.contains(value))
    {
        return Err(VerificationError::invalid(VerificationReason::Audience));
    }
    validate_introspection_time(expiry, not_before, now)?;
    let subject = subject
        .as_deref()
        .map(identity)
        .transpose()?
        .map(ToOwned::to_owned);
    let client_id = client_id
        .as_deref()
        .map(identity)
        .transpose()?
        .map(ToOwned::to_owned);
    if subject.is_none() && client_id.is_none() {
        return Err(missing());
    }
    Ok(VerifiedIntrospection {
        principal: Principal::new(
            policy.issuer.clone(),
            subject,
            client_id,
            scopes,
            expiry,
            Arc::from(payload.get()),
        ),
        not_before,
    })
}
// template:end oidc-introspection:authn-claims-introspection-validation

fn identity(value: &str) -> Result<&str, VerificationError> {
    if value.is_empty() {
        Err(VerificationError::invalid(VerificationReason::Identity))
    } else {
        Ok(value)
    }
}

fn normalize_scopes(
    scope: Option<&serde_json::Value>,
    scp: Option<&serde_json::Value>,
    malformed: Failure,
) -> Result<Vec<String>, VerificationError> {
    let error = || VerificationError::new(malformed, VerificationReason::Scope);
    let selected = scope.or(scp);
    let values = match selected {
        None => return Ok(Vec::new()),
        Some(serde_json::Value::String(value)) if value.is_empty() => return Ok(Vec::new()),
        Some(serde_json::Value::String(value)) => value.split(' ').collect::<Vec<_>>(),
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| value.as_str().ok_or_else(error))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(error()),
    };
    values
        .into_iter()
        .map(|value| scope_token(value).ok_or_else(error).map(ToOwned::to_owned))
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|values| values.into_iter().collect())
}

fn scope_token(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.bytes().all(|byte| {
            byte == b'!' || (b'#'..=b'[').contains(&byte) || (b']'..=b'~').contains(&byte)
        }))
    .then_some(value)
}

#[cfg(test)]
mod tests {
    use super::ClaimPolicy;
    use crate::Failure;
    // template:begin oidc-jwt:authn-claims-jwt-test-imports
    use super::{JwtClaims, validate_jwt_claims};
    use crate::TokenProfile;
    // template:end oidc-jwt:authn-claims-jwt-test-imports
    // template:begin oidc-introspection:authn-claims-introspection-test-import
    use super::validate_introspection_claims;
    // template:end oidc-introspection:authn-claims-introspection-test-import

    fn policy() -> ClaimPolicy {
        ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()])
    }

    // template:begin oidc-jwt:authn-claims-jwt-normalization-test
    #[test]
    fn typed_jwt_claims_normalize_matching_scope_forms() {
        let claims: JwtClaims = serde_json::from_str(r#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject","scope":"read write read","scp":["write","read"]}"#).unwrap();
        let principal = validate_jwt_claims(
            &claims,
            &policy(),
            TokenProfile::ResourceServer,
            100,
            std::sync::Arc::from("{}"),
        )
        .unwrap();
        assert_eq!(principal.scopes(), ["read", "write"]);
        assert_eq!(principal.expires_at(), 130);
    }

    // template:end oidc-jwt:authn-claims-jwt-normalization-test

    // template:begin oidc-introspection:authn-claims-introspection-shape-test
    #[test]
    fn selected_scope_ignores_conflicting_fallback_and_null_nbf_is_unavailable() {
        let response = br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject","scope":"read","scp":["write"]}"#;
        assert_eq!(
            validate_introspection_claims(response, &policy(), 100)
                .unwrap()
                .into_principal()
                .scopes(),
            ["read"]
        );
        assert_eq!(
            validate_introspection_claims(br#"{"active":false,"iss":false}"#, &policy(), 100)
                .map_err(|error| error.failure),
            Err(Failure::Invalid)
        );
        assert_eq!(validate_introspection_claims(br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"nbf":null,"sub":"subject"}"#, &policy(), 100).map_err(|error| error.failure), Err(Failure::Unavailable));
    }
    #[test]
    fn inactive_claim_meaning_is_ignored_but_duplicate_consumed_fields_reject() {
        assert_eq!(
            validate_introspection_claims(
                br#"{"active":false,"exp":null,"sub":42}"#,
                &policy(),
                100
            )
            .unwrap_err()
            .failure,
            Failure::Invalid
        );
        for response in [
            br#"{"active":false,"sub":"one","sub":"two"}"#.as_slice(),
            br#"{"active":true,"sub":null,"sub":"two"}"#,
            br#"{"active":false,"scope":null,"scope":"read"}"#,
            br#"{"active":false,"scp":null,"scp":[]}"#,
        ] {
            assert_eq!(
                validate_introspection_claims(response, &policy(), 100)
                    .unwrap_err()
                    .failure,
                Failure::Unavailable
            );
        }
        for claim in ["iss", "aud"] {
            let mut response = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
            response[claim] = serde_json::Value::Null;
            assert_eq!(
                validate_introspection_claims(
                    &serde_json::to_vec(&response).unwrap(),
                    &policy(),
                    100
                )
                .unwrap_err()
                .reason,
                crate::VerificationReason::MissingClaim
            );
        }
    }
    #[test]
    fn typed_claim_access_preserves_evidence_and_custom_last_member_semantics() {
        #[derive(serde::Deserialize)]
        struct ApplicationClaims {
            tenant: String,
            scope: Vec<String>,
            sub: String,
        }
        #[derive(Debug, serde::Deserialize)]
        struct WrongShape {
            #[serde(rename = "tenant")]
            _tenant: u64,
        }
        let principal = validate_introspection_claims(
            br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject","scope":["write","read","read"],"tenant":"old","tenant":"private-value"}"#,
            &policy(), 100,
        ).unwrap().into_principal();
        let claims = principal.claims::<ApplicationClaims>().unwrap();
        assert_eq!(claims.tenant, "private-value");
        assert_eq!(claims.scope, ["write", "read", "read"]);
        assert_eq!(claims.sub, "subject");
        assert_eq!(principal.scopes(), ["read", "write"]);
        let error = principal.claims::<WrongShape>().unwrap_err();
        assert_eq!(error, crate::ClaimAccessError::InvalidShape);
        assert!(!format!("{error:?} {error} {principal:?}").contains("private-value"));
    }

    // template:end oidc-introspection:authn-claims-introspection-shape-test

    // template:begin oidc-introspection:authn-claims-introspection-missing-test
    #[test]
    fn missing_active_claims_are_invalid_not_provider_failures() {
        let complete = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
        for claim in ["iss", "aud", "exp", "sub"] {
            let mut response = complete.clone();
            response.as_object_mut().unwrap().remove(claim);
            let error = validate_introspection_claims(
                &serde_json::to_vec(&response).unwrap(),
                &policy(),
                100,
            )
            .unwrap_err();
            assert_eq!(error.failure, Failure::Invalid, "missing {claim}");
            assert_eq!(error.reason, crate::VerificationReason::MissingClaim);
        }
        for (claim, value) in [
            ("exp", serde_json::Value::Null),
            ("exp", serde_json::json!(1.5)),
            ("iss", serde_json::json!(1)),
            ("aud", serde_json::json!(false)),
            ("sub", serde_json::json!(1)),
        ] {
            let mut response = complete.clone();
            response[claim] = value;
            assert_eq!(
                validate_introspection_claims(
                    &serde_json::to_vec(&response).unwrap(),
                    &policy(),
                    100
                )
                .map_err(|error| error.failure),
                Err(Failure::Unavailable),
                "wrong type {claim}"
            );
        }
    }

    // template:end oidc-introspection:authn-claims-introspection-missing-test

    #[test]
    fn selected_scope_has_precedence_and_accepts_both_shapes() {
        for (extra, expected) in [
            (serde_json::json!({"scope":"","scp":["read"]}), Some(vec![])),
            (
                serde_json::json!({"scope":"read","scp":[]}),
                Some(vec!["read"]),
            ),
            (
                serde_json::json!({"scope":["read"],"scp":42}),
                Some(vec!["read"]),
            ),
            (
                serde_json::json!({"scope":null,"scp":"write read"}),
                Some(vec!["read", "write"]),
            ),
            (serde_json::json!({"scope":42,"scp":"read"}), None),
            (
                serde_json::json!({"scope":null,"scp":["read"]}),
                Some(vec!["read"]),
            ),
            (
                serde_json::json!({"scope":"read","scp":null}),
                Some(vec!["read"]),
            ),
            (serde_json::json!({"scope":"","scp":[]}), Some(vec![])),
            (serde_json::json!({}), Some(vec![])),
        ] {
            let mut response = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
            response
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            // template:begin oidc-introspection:authn-claims-scope-introspection-assertion
            {
                let result = validate_introspection_claims(
                    &serde_json::to_vec(&response).unwrap(),
                    &policy(),
                    100,
                );
                if let Some(expected) = &expected {
                    assert_eq!(
                        result.unwrap().into_principal().scopes(),
                        expected.as_slice()
                    );
                } else {
                    assert_eq!(result.unwrap_err().failure, Failure::Unavailable);
                }
            }
            // template:end oidc-introspection:authn-claims-scope-introspection-assertion
            // template:begin oidc-jwt:authn-claims-scope-jwt-assertion
            let claims: JwtClaims = serde_json::from_value(response).unwrap();
            {
                let result = validate_jwt_claims(
                    &claims,
                    &policy(),
                    TokenProfile::ResourceServer,
                    100,
                    std::sync::Arc::from("{}"),
                );
                if let Some(expected) = &expected {
                    assert_eq!(result.unwrap().scopes(), expected.as_slice());
                } else {
                    assert_eq!(result.unwrap_err().failure, Failure::Invalid);
                }
            }
            // template:end oidc-jwt:authn-claims-scope-jwt-assertion
        }
    }
}
