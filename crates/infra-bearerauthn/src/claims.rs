//! Typed claim decoding and application identity normalization.
//!
//! JSON `null` means absent for every consumed claim.

use crate::{Failure, Principal, VerificationError, VerificationReason};
use serde::Deserialize;
use std::sync::Arc;
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

/// A `scope` or `scp` value: one space-delimited string or an array of strings.
#[derive(Deserialize)]
#[serde(untagged)]
enum Scope {
    Delimited(String),
    List(Vec<String>),
}

// template:begin oidc-jwt:authn-claims-jwt-type
/// The JWT claims this crate reads. jsonwebtoken's `Validation` has already
/// checked issuer, audience, expiry and not-before.
#[derive(Deserialize)]
pub(crate) struct JwtClaims {
    exp: u64,
    sub: Option<String>,
    client_id: Option<String>,
    azp: Option<String>,
    appid: Option<String>,
    cid: Option<String>,
    scope: Option<Scope>,
    scp: Option<Scope>,
    jti: Option<String>,
    iat: Option<u64>,
}
// template:end oidc-jwt:authn-claims-jwt-type

// template:begin oidc-introspection:authn-claims-introspection-envelope
/// Only `active` is read before the provider says the token is active.
#[derive(Deserialize)]
struct Envelope {
    active: bool,
}

/// The claims an active RFC 7662 response must or may supply.
#[derive(Deserialize)]
struct ActiveClaims {
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<u64>,
    nbf: Option<u64>,
    sub: Option<String>,
    client_id: Option<String>,
    scope: Option<Scope>,
    scp: Option<Scope>,
}
// template:end oidc-introspection:authn-claims-introspection-envelope

// template:begin oidc-introspection:authn-claims-audience-values
#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
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
// template:end oidc-introspection:authn-claims-audience-values

// template:begin oidc-jwt:authn-claims-jwt-validation
pub(crate) fn validate_jwt_claims(
    payload: &str,
    policy: &ClaimPolicy,
    token_profile: TokenProfile,
    now: u64,
) -> Result<Principal, VerificationError> {
    let claims: JwtClaims = serde_json::from_str(payload)
        .map_err(|_| VerificationError::invalid(VerificationReason::MalformedClaims))?;
    let subject = non_empty(claims.sub);
    let has_client_id = claims
        .client_id
        .as_ref()
        .is_some_and(|value| !value.is_empty());
    let client_id = [claims.client_id, claims.azp, claims.appid, claims.cid]
        .into_iter()
        .find_map(non_empty);
    if subject.is_none() && client_id.is_none() {
        return Err(VerificationError::invalid(VerificationReason::MissingClaim));
    }
    if token_profile == TokenProfile::Rfc9068 {
        let (Some(iat), Some(_), Some(()), Some(_)) = (
            claims.iat,
            &subject,
            has_client_id.then_some(()),
            non_empty(claims.jti),
        ) else {
            return Err(VerificationError::invalid(VerificationReason::MissingClaim));
        };
        if iat > now.saturating_add(LEEWAY_SECONDS) {
            return Err(VerificationError::invalid(VerificationReason::Profile));
        }
    }
    let scopes = normalize_scopes(claims.scope.or(claims.scp), Failure::Invalid)?;
    Ok(Principal::new(
        policy.issuer.clone(),
        subject,
        client_id,
        scopes,
        claims.exp,
        Arc::from(payload),
    ))
}
// template:end oidc-jwt:authn-claims-jwt-validation

// template:begin oidc-introspection:authn-claims-introspection-validation
/// Validates an RFC 7662 response. A response this crate cannot read is
/// unavailable trust; an active response lacking required claims is invalid.
pub(crate) fn validate_introspection_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    now: u64,
) -> Result<Principal, VerificationError> {
    let malformed =
        || VerificationError::new(Failure::Unavailable, VerificationReason::MalformedClaims);
    let invalid = VerificationError::invalid;
    // Keep the provider evidence intact, excluding only JSON boundary whitespace.
    // Envelope deserialization still validates the entire response before active.
    let payload = std::str::from_utf8(bytes)
        .map_err(|_| malformed())?
        .trim_matches([' ', '\t', '\r', '\n']);
    let envelope: Envelope = serde_json::from_str(payload).map_err(|_| malformed())?;
    if !envelope.active {
        return Err(invalid(VerificationReason::Inactive));
    }
    let claims: ActiveClaims = serde_json::from_str(payload).map_err(|_| malformed())?;
    let (Some(issuer), Some(audience), Some(expiry)) = (claims.iss, claims.aud, claims.exp) else {
        return Err(invalid(VerificationReason::MissingClaim));
    };
    if issuer != policy.issuer {
        return Err(invalid(VerificationReason::Issuer));
    }
    if !audience
        .values()
        .iter()
        .any(|value| policy.audiences.contains(value))
    {
        return Err(invalid(VerificationReason::Audience));
    }
    if now > expiry.saturating_add(LEEWAY_SECONDS) {
        return Err(invalid(VerificationReason::Expired));
    }
    if claims
        .nbf
        .is_some_and(|not_before| not_before > now.saturating_add(LEEWAY_SECONDS))
    {
        return Err(invalid(VerificationReason::NotYetValid));
    }
    let subject = non_empty(claims.sub);
    let client_id = non_empty(claims.client_id);
    if subject.is_none() && client_id.is_none() {
        return Err(invalid(VerificationReason::MissingClaim));
    }
    let scopes = normalize_scopes(claims.scope.or(claims.scp), Failure::Unavailable)?;
    Ok(Principal::new(
        policy.issuer.clone(),
        subject,
        client_id,
        scopes,
        expiry,
        Arc::from(payload),
    ))
}
// template:end oidc-introspection:authn-claims-introspection-validation

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.is_empty())
}

/// Returns sorted, unique RFC 6749 scope tokens with their exact case.
fn normalize_scopes(
    scope: Option<Scope>,
    malformed: Failure,
) -> Result<Vec<String>, VerificationError> {
    let mut values = match scope {
        None => Vec::new(),
        Some(Scope::Delimited(value)) if value.is_empty() => Vec::new(),
        Some(Scope::Delimited(value)) => value.split(' ').map(ToOwned::to_owned).collect(),
        Some(Scope::List(values)) => values,
    };
    if !values.iter().all(|value| is_scope_token(value)) {
        return Err(VerificationError::new(malformed, VerificationReason::Scope));
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

/// RFC 6749 section 3.3: `scope-token = 1*( %x21 / %x23-5B / %x5D-7E )`.
fn is_scope_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte == b'!' || (b'#'..=b'[').contains(&byte) || (b']'..=b'~').contains(&byte)
        })
}

#[cfg(test)]
mod tests {
    use super::ClaimPolicy;
    use crate::{Failure, VerificationReason};
    // template:begin oidc-jwt:authn-claims-jwt-test-imports
    use super::validate_jwt_claims;
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
    fn jwt_identity_uses_the_first_nonempty_client_alias_and_treats_null_as_absent() {
        let verify = |claims: serde_json::Value| {
            validate_jwt_claims(
                &claims.to_string(),
                &policy(),
                TokenProfile::ResourceServer,
                100,
            )
        };
        let principal = verify(serde_json::json!({
            "exp": 130, "sub": null, "client_id": "", "azp": "client", "cid": "other",
            "scope": "read write read",
        }))
        .unwrap();
        assert_eq!(principal.subject(), None);
        assert_eq!(principal.client_id(), Some("client"));
        assert_eq!(principal.scopes(), ["read", "write"]);
        assert_eq!(principal.expires_at(), 130);
        assert_eq!(
            verify(serde_json::json!({"exp": 130, "sub": ""})).unwrap_err(),
            crate::VerificationError::invalid(VerificationReason::MissingClaim)
        );
    }

    #[test]
    fn rfc9068_requires_its_claims_and_a_past_issue_time() {
        let verify = |extra: serde_json::Value| {
            let mut claims = serde_json::json!({
                "exp": 200, "sub": "subject", "client_id": "client", "jti": "id", "iat": 100,
            });
            claims
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            validate_jwt_claims(&claims.to_string(), &policy(), TokenProfile::Rfc9068, 100)
                .map_err(|error| error.reason)
        };
        assert!(verify(serde_json::json!({})).is_ok());
        for missing in ["sub", "client_id", "jti", "iat"] {
            assert_eq!(
                verify(serde_json::json!({ missing: null })),
                Err(VerificationReason::MissingClaim),
                "{missing}"
            );
        }
        assert_eq!(
            verify(serde_json::json!({"iat": 131})),
            Err(VerificationReason::Profile)
        );
    }
    // template:end oidc-jwt:authn-claims-jwt-normalization-test

    // template:begin oidc-introspection:authn-claims-introspection-shape-test
    #[test]
    fn inactive_responses_ignore_the_rest_of_the_body() {
        for response in [
            br#"{"active":false}"#.as_slice(),
            br#"{"active":false,"exp":null,"sub":42}"#,
            br#"{"active":false,"sub":"one","sub":"two"}"#,
        ] {
            assert_eq!(
                validate_introspection_claims(response, &policy(), 100).unwrap_err(),
                crate::VerificationError::invalid(VerificationReason::Inactive)
            );
        }
    }

    #[test]
    fn unreadable_responses_are_unavailable_trust() {
        for response in [
            b"{".as_slice(),
            br#"{"active":"yes"}"#,
            br#"{"active":false}{}"#,
            br#"{"active":false,"active":true}"#,
            b"{\"active\":false,\"extra\":\"\xff\"}",
            b"\x0b{\"active\":false}",
            br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"one","sub":"two"}"#,
            br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":1.5,"sub":"subject"}"#,
            br#"{"active":true,"iss":1,"aud":"api","exp":130,"sub":"subject"}"#,
            br#"{"active":true,"iss":"https://issuer.example","aud":false,"exp":130,"sub":"subject"}"#,
        ] {
            let error = validate_introspection_claims(response, &policy(), 100).unwrap_err();
            assert_eq!(error.failure, Failure::Unavailable, "{response:?}");
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
        ).unwrap();
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
    fn active_claims_are_required_and_null_means_absent() {
        let complete = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
        let reason = |response: &serde_json::Value| {
            validate_introspection_claims(response.to_string().as_bytes(), &policy(), 100)
                .map(|_| ())
                .map_err(|error| (error.failure, error.reason))
        };
        for claim in ["iss", "aud", "exp", "sub"] {
            for absent in [None, Some(serde_json::Value::Null)] {
                let mut response = complete.clone();
                match absent {
                    None => drop(response.as_object_mut().unwrap().remove(claim)),
                    Some(null) => response[claim] = null,
                }
                assert_eq!(
                    reason(&response),
                    Err((Failure::Invalid, VerificationReason::MissingClaim)),
                    "{response}"
                );
            }
        }
        let mut response = complete.clone();
        response["nbf"] = serde_json::Value::Null;
        assert_eq!(reason(&response), Ok(()));
        for (claim, value, expected) in [
            ("iss", "https://other.example", VerificationReason::Issuer),
            ("iss", "", VerificationReason::Issuer),
            ("aud", "other", VerificationReason::Audience),
            ("aud", "", VerificationReason::Audience),
        ] {
            let mut response = complete.clone();
            response[claim] = value.into();
            assert_eq!(reason(&response), Err((Failure::Invalid, expected)));
        }
        for (claim, value, expected) in [
            ("exp", 69, VerificationReason::Expired),
            ("nbf", 131, VerificationReason::NotYetValid),
        ] {
            let mut response = complete.clone();
            response[claim] = value.into();
            assert_eq!(reason(&response), Err((Failure::Invalid, expected)));
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
                serde_json::json!({"scope":null,"scp":"write read"}),
                Some(vec!["read", "write"]),
            ),
            (
                serde_json::json!({"scope":null,"scp":["read"]}),
                Some(vec!["read"]),
            ),
            (serde_json::json!({"scope":"","scp":[]}), Some(vec![])),
            (serde_json::json!({}), Some(vec![])),
            (serde_json::json!({"scope":42,"scp":"read"}), None),
            (serde_json::json!({"scope":["read"],"scp":42}), None),
            (serde_json::json!({"scope":"read  write"}), None),
            (serde_json::json!({"scope":["read write"]}), None),
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
                    assert_eq!(result.unwrap().scopes(), expected.as_slice());
                } else {
                    assert_eq!(result.unwrap_err().failure, Failure::Unavailable, "{extra}");
                }
            }
            // template:end oidc-introspection:authn-claims-scope-introspection-assertion
            // template:begin oidc-jwt:authn-claims-scope-jwt-assertion
            {
                let result = validate_jwt_claims(
                    &response.to_string(),
                    &policy(),
                    TokenProfile::ResourceServer,
                    100,
                );
                if let Some(expected) = &expected {
                    assert_eq!(result.unwrap().scopes(), expected.as_slice());
                } else {
                    assert_eq!(result.unwrap_err().failure, Failure::Invalid, "{extra}");
                }
            }
            // template:end oidc-jwt:authn-claims-scope-jwt-assertion
        }
    }
}
