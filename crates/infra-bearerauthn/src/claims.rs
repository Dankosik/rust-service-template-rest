//! Typed claim decoding and application identity normalization.
//!
//! JSON `null` means absent for every consumed claim.

use crate::{
    Actor, Failure, PreparationError, PreparationPhase, PreparationReason, Principal,
    VerificationError, VerificationReason,
};
use serde::Deserialize;
use std::{borrow::Cow, fmt};
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
    pub(crate) fn new(issuer: String, audiences: Vec<String>) -> Result<Self, PreparationError> {
        if audiences.is_empty() || audiences.iter().any(String::is_empty) {
            return Err(PreparationError::new(
                PreparationPhase::Options,
                PreparationReason::Parse,
            ));
        }
        Ok(Self { issuer, audiences })
    }
}

/// A claim holding one string or an array of strings: `aud`, and `scope` or
/// `scp`, whose single string is space-delimited. Strings borrow from the
/// payload unless they contain escapes.
enum Values<'a> {
    One(Cow<'a, str>),
    Many(Vec<Cow<'a, str>>),
}

impl<'de: 'a, 'a> Deserialize<'de> for Values<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<'a>(std::marker::PhantomData<Values<'a>>);
        impl<'de: 'a, 'a> serde::de::Visitor<'de> for Visitor<'a> {
            type Value = Values<'a>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or an array of strings")
            }
            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                value: &'de str,
            ) -> Result<Self::Value, E> {
                Ok(Values::One(Cow::Borrowed(value)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Values::One(Cow::Owned(value.to_owned())))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(64));
                while let Some(Borrowed(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(Values::Many(values))
            }
        }
        deserializer.deserialize_any(Visitor(std::marker::PhantomData))
    }
}

/// A string that borrows from the payload unless it contains escapes.
#[derive(Deserialize)]
struct Borrowed<'a>(#[serde(borrow)] Cow<'a, str>);

/// The outermost RFC 8693 `act` object, shared by JWT and introspection
/// evidence. A non-object `act` fails to deserialize, which the caller reads
/// as malformed claims; `sub` and `client_id` accept only string values, and
/// any other member, including a nested `act`, is ignored.
#[derive(Deserialize)]
struct RawAct<'a> {
    #[serde(borrow)]
    sub: Cow<'a, str>,
    #[serde(borrow)]
    client_id: Option<Cow<'a, str>>,
}

impl RawAct<'_> {
    /// `sub` must be non-empty; an empty value is malformed evidence, not an
    /// absent actor, so the caller maps `Err` to its own malformed reason.
    fn into_actor(self) -> Result<Actor, ()> {
        if self.sub.is_empty() {
            return Err(());
        }
        Ok(Actor::new(
            self.sub.into_owned(),
            self.client_id.map(Cow::into_owned),
        ))
    }
}

impl Values<'_> {
    fn contains_any(&self, accepted: &[String]) -> bool {
        match self {
            Self::One(value) => accepted.iter().any(|accepted| accepted == value),
            Self::Many(values) => values
                .iter()
                .any(|value| accepted.iter().any(|accepted| accepted == value)),
        }
    }
}

// template:begin oidc-jwt:authn-claims-jwt-type
/// The JWT claims this crate reads, after the signature has been verified.
#[derive(Deserialize)]
pub(crate) struct JwtClaims<'a> {
    /// A string or, as jsonwebtoken reads it, an array naming the issuer.
    #[serde(borrow)]
    iss: Option<Values<'a>>,
    #[serde(borrow)]
    aud: Option<Values<'a>>,
    exp: Option<u64>,
    /// A `null` or non-numeric `nbf` is malformed, not absent.
    #[serde(default, deserialize_with = "numeric_date")]
    nbf: Option<u64>,
    #[serde(borrow)]
    sub: Option<Cow<'a, str>>,
    #[serde(borrow)]
    client_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    azp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    appid: Option<Cow<'a, str>>,
    #[serde(borrow)]
    cid: Option<Cow<'a, str>>,
    #[serde(borrow)]
    scope: Option<Values<'a>>,
    #[serde(borrow)]
    scp: Option<Values<'a>>,
    #[serde(borrow)]
    jti: Option<Cow<'a, str>>,
    iat: Option<u64>,
    #[serde(borrow)]
    act: Option<RawAct<'a>>,
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
struct ActiveClaims<'a> {
    #[serde(borrow)]
    iss: Option<Cow<'a, str>>,
    #[serde(borrow)]
    aud: Option<Values<'a>>,
    exp: Option<u64>,
    nbf: Option<u64>,
    #[serde(borrow)]
    sub: Option<Cow<'a, str>>,
    #[serde(borrow)]
    client_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    scope: Option<Values<'a>>,
    #[serde(borrow)]
    scp: Option<Values<'a>>,
    #[serde(borrow)]
    act: Option<RawAct<'a>>,
}
// template:end oidc-introspection:authn-claims-introspection-envelope

// template:begin oidc-jwt:authn-claims-jwt-validation
/// Validates a verified JWT payload: registered claims in RFC 7519 order,
/// then identity and the selected token profile.
pub(crate) fn validate_jwt_claims(
    payload: Vec<u8>,
    policy: &ClaimPolicy,
    token_profile: TokenProfile,
    now: u64,
    access_token: secrecy::SecretString,
) -> Result<Principal, VerificationError> {
    let invalid = VerificationError::invalid;
    let payload =
        String::from_utf8(payload).map_err(|_| invalid(VerificationReason::MalformedClaims))?;
    let claims: JwtClaims<'_> =
        serde_json::from_str(&payload).map_err(|_| invalid(VerificationReason::MalformedClaims))?;
    let (Some(issuer), Some(audience), Some(expiry)) = (claims.iss, claims.aud, claims.exp) else {
        return Err(invalid(VerificationReason::MissingClaim));
    };
    check_registered_claims(&issuer, &audience, expiry, claims.nbf, policy, now)?;
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
    let actor = claims
        .act
        .map(RawAct::into_actor)
        .transpose()
        .map_err(|()| invalid(VerificationReason::MalformedClaims))?;
    Ok(Principal::new(
        policy.issuer.clone(),
        subject,
        client_id,
        scopes,
        expiry,
        payload,
        access_token,
        actor,
    ))
}

/// A `NumericDate`: an unsigned integer, or a finite non-negative number rounded
/// to one, as jsonwebtoken reads `nbf`.
fn numeric_date<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    struct Visitor;
    impl serde::de::Visitor<'_> for Visitor {
        type Value = Option<u64>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a NumericDate")
        }
        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(Some(value))
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
            if value.is_finite() && value >= 0.0 && value < u64::MAX as f64 {
                Ok(Some(value.round() as u64))
            } else {
                Err(E::custom("NumericDate out of range"))
            }
        }
    }
    deserializer.deserialize_any(Visitor)
}

/// JWT registered-claim order: lifetime first, then issuer and audience.
fn check_registered_claims(
    issuer: &Values<'_>,
    audience: &Values<'_>,
    expiry: u64,
    not_before: Option<u64>,
    policy: &ClaimPolicy,
    now: u64,
) -> Result<(), VerificationError> {
    check_lifetime(expiry, not_before, now)?;
    if !issuer.contains_any(std::slice::from_ref(&policy.issuer)) {
        return Err(VerificationError::invalid(VerificationReason::Issuer));
    }
    if !audience.contains_any(&policy.audiences) {
        return Err(VerificationError::invalid(VerificationReason::Audience));
    }
    Ok(())
}
// template:end oidc-jwt:authn-claims-jwt-validation

// template:begin oidc-introspection:authn-claims-introspection-validation
/// Validates an RFC 7662 response. A response this crate cannot read is
/// unavailable trust; an active response lacking required claims is invalid.
pub(crate) fn validate_introspection_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    now: u64,
    access_token: secrecy::SecretString,
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
    let claims: ActiveClaims<'_> = serde_json::from_str(payload).map_err(|_| malformed())?;
    let (Some(issuer), Some(audience), Some(expiry)) = (claims.iss, claims.aud, claims.exp) else {
        return Err(invalid(VerificationReason::MissingClaim));
    };
    if issuer != policy.issuer {
        return Err(invalid(VerificationReason::Issuer));
    }
    if !audience.contains_any(&policy.audiences) {
        return Err(invalid(VerificationReason::Audience));
    }
    check_lifetime(expiry, claims.nbf, now)?;
    let subject = non_empty(claims.sub);
    let client_id = non_empty(claims.client_id);
    if subject.is_none() && client_id.is_none() {
        return Err(invalid(VerificationReason::MissingClaim));
    }
    let scopes = normalize_scopes(claims.scope.or(claims.scp), Failure::Unavailable)?;
    let actor = claims
        .act
        .map(RawAct::into_actor)
        .transpose()
        .map_err(|()| malformed())?;
    Ok(Principal::new(
        policy.issuer.clone(),
        subject,
        client_id,
        scopes,
        expiry,
        payload.to_owned(),
        access_token,
        actor,
    ))
}
// template:end oidc-introspection:authn-claims-introspection-validation

/// Expiry and not-before, each with the same clock-skew leeway.
fn check_lifetime(expiry: u64, not_before: Option<u64>, now: u64) -> Result<(), VerificationError> {
    if now > expiry.saturating_add(LEEWAY_SECONDS) {
        return Err(VerificationError::invalid(VerificationReason::Expired));
    }
    if not_before.is_some_and(|not_before| not_before > now.saturating_add(LEEWAY_SECONDS)) {
        return Err(VerificationError::invalid(VerificationReason::NotYetValid));
    }
    Ok(())
}

fn non_empty(value: Option<Cow<'_, str>>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(Cow::into_owned)
}

/// Returns sorted, unique RFC 6749 scope tokens with their exact case.
fn normalize_scopes(
    scope: Option<Values<'_>>,
    malformed: Failure,
) -> Result<Vec<String>, VerificationError> {
    let mut values: Vec<String> = match scope {
        None => Vec::new(),
        Some(Values::One(value)) if value.is_empty() => Vec::new(),
        Some(Values::One(value)) => value.split(' ').map(ToOwned::to_owned).collect(),
        Some(Values::Many(values)) => values.into_iter().map(Cow::into_owned).collect(),
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
        ClaimPolicy::new("https://issuer.example".to_owned(), vec!["api".to_owned()]).unwrap()
    }

    /// A fixed access token for tests that do not exercise its exact value.
    fn access_token() -> secrecy::SecretString {
        secrecy::SecretString::from("presented-token")
    }

    // template:begin oidc-jwt:authn-claims-jwt-normalization-test
    #[test]
    fn jwt_identity_uses_the_first_nonempty_client_alias_and_treats_null_as_absent() {
        let verify = |mut claims: serde_json::Value| {
            claims["iss"] = "https://issuer.example".into();
            claims["aud"] = "api".into();
            validate_jwt_claims(
                claims.to_string().into_bytes(),
                &policy(),
                TokenProfile::ResourceServer,
                100,
                access_token(),
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
    fn registered_claims_follow_the_jsonwebtoken_rules_in_its_order() {
        let verify = |extra: serde_json::Value| {
            let mut claims = serde_json::json!({
                "iss": "https://issuer.example", "aud": "api", "exp": 1000, "sub": "subject",
            });
            for (key, value) in extra.as_object().unwrap() {
                claims[key] = value.clone();
            }
            validate_jwt_claims(
                claims.to_string().into_bytes(),
                &policy(),
                TokenProfile::ResourceServer,
                1000,
                access_token(),
            )
            .map(|principal| principal.expires_at())
            .map_err(|error| error.reason)
        };
        for (extra, expected) in [
            (serde_json::json!({}), Ok(1000)),
            (serde_json::json!({"aud": ["other", "api"]}), Ok(1000)),
            (serde_json::json!({"exp": 970, "nbf": 1030}), Ok(970)),
            (serde_json::json!({"nbf": 1030.4}), Ok(1000)),
            (
                serde_json::json!({"iss": null}),
                Err(VerificationReason::MissingClaim),
            ),
            (
                serde_json::json!({"aud": null}),
                Err(VerificationReason::MissingClaim),
            ),
            (
                serde_json::json!({"exp": 969}),
                Err(VerificationReason::Expired),
            ),
            (
                serde_json::json!({"nbf": 1031}),
                Err(VerificationReason::NotYetValid),
            ),
            (
                serde_json::json!({"iss": "https://issuer.example/"}),
                Err(VerificationReason::Issuer),
            ),
            (
                serde_json::json!({"aud": ["other"]}),
                Err(VerificationReason::Audience),
            ),
            (
                serde_json::json!({"aud": []}),
                Err(VerificationReason::Audience),
            ),
            // Lifetime is checked before issuer and audience.
            (
                serde_json::json!({"exp": 1, "iss": "other"}),
                Err(VerificationReason::Expired),
            ),
            (
                serde_json::json!({"nbf": "1"}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"exp": 1000.5}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"iss": ["other", "https://issuer.example"]}),
                Ok(1000),
            ),
            (
                serde_json::json!({"iss": []}),
                Err(VerificationReason::Issuer),
            ),
            (
                serde_json::json!({"aud": [1]}),
                Err(VerificationReason::MalformedClaims),
            ),
        ] {
            assert_eq!(verify(extra.clone()), expected, "{extra}");
        }
    }

    #[test]
    fn rfc9068_requires_its_claims_and_a_past_issue_time() {
        let verify = |extra: serde_json::Value| {
            let mut claims = serde_json::json!({
                "iss": "https://issuer.example", "aud": "api",
                "exp": 200, "sub": "subject", "client_id": "client", "jti": "id", "iat": 100,
            });
            claims
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            validate_jwt_claims(
                claims.to_string().into_bytes(),
                &policy(),
                TokenProfile::Rfc9068,
                100,
                access_token(),
            )
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

    #[test]
    fn jwt_act_identifies_the_outermost_actor_and_rejects_malformed_evidence() {
        let verify = |extra: serde_json::Value| {
            let mut claims = serde_json::json!({
                "iss": "https://issuer.example", "aud": "api", "exp": 1000, "sub": "subject",
            });
            claims
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            validate_jwt_claims(
                claims.to_string().into_bytes(),
                &policy(),
                TokenProfile::ResourceServer,
                100,
                access_token(),
            )
        };
        let principal = verify(serde_json::json!({
            "act": {"sub": "service-a", "client_id": "gateway", "act": {"sub": "nested"}},
        }))
        .unwrap();
        let actor = principal.actor().unwrap();
        assert_eq!(actor.subject(), "service-a");
        assert_eq!(actor.client_id(), Some("gateway"));
        assert_eq!(format!("{actor:?}"), "Actor([REDACTED])");
        for absent in [serde_json::json!({}), serde_json::json!({"act": null})] {
            assert!(verify(absent).unwrap().actor().is_none());
        }
        assert_eq!(
            verify(serde_json::json!({"act": {"sub": "solo"}}))
                .unwrap()
                .actor()
                .unwrap()
                .client_id(),
            None
        );
        for malformed in [
            serde_json::json!({"act": "not-an-object"}),
            serde_json::json!({"act": []}),
            serde_json::json!({"act": {}}),
            serde_json::json!({"act": {"sub": ""}}),
            serde_json::json!({"act": {"sub": 5}}),
            serde_json::json!({"act": {"sub": "x", "client_id": 5}}),
        ] {
            assert_eq!(
                verify(malformed.clone()).unwrap_err().reason,
                VerificationReason::MalformedClaims,
                "{malformed}"
            );
        }
    }

    #[test]
    fn jwt_access_token_returns_the_exact_presented_token() {
        use secrecy::ExposeSecret;
        let claims = serde_json::json!({
            "iss": "https://issuer.example", "aud": "api", "exp": 1000, "sub": "subject",
        });
        let principal = validate_jwt_claims(
            claims.to_string().into_bytes(),
            &policy(),
            TokenProfile::ResourceServer,
            100,
            secrecy::SecretString::from("verified-bearer-text"),
        )
        .unwrap();
        assert_eq!(
            principal.access_token().expose_secret(),
            "verified-bearer-text"
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
                validate_introspection_claims(response, &policy(), 100, access_token())
                    .unwrap_err(),
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
            let error =
                validate_introspection_claims(response, &policy(), 100, access_token())
                    .unwrap_err();
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
            &policy(), 100, access_token(),
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

    #[test]
    fn introspection_act_identifies_the_outermost_actor_and_rejects_malformed_evidence() {
        let verify = |extra: serde_json::Value| {
            let mut response = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
            response
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            validate_introspection_claims(
                response.to_string().as_bytes(),
                &policy(),
                100,
                access_token(),
            )
        };
        let principal = verify(serde_json::json!({
            "act": {"sub": "service-a", "client_id": "gateway", "act": {"sub": "nested"}},
        }))
        .unwrap();
        let actor = principal.actor().unwrap();
        assert_eq!(actor.subject(), "service-a");
        assert_eq!(actor.client_id(), Some("gateway"));
        for absent in [serde_json::json!({}), serde_json::json!({"act": null})] {
            assert!(verify(absent).unwrap().actor().is_none());
        }
        assert_eq!(
            verify(serde_json::json!({"act": {"sub": "solo"}}))
                .unwrap()
                .actor()
                .unwrap()
                .client_id(),
            None
        );
        for malformed in [
            serde_json::json!({"act": "not-an-object"}),
            serde_json::json!({"act": []}),
            serde_json::json!({"act": {}}),
            serde_json::json!({"act": {"sub": ""}}),
            serde_json::json!({"act": {"sub": 5}}),
            serde_json::json!({"act": {"sub": "x", "client_id": 5}}),
        ] {
            let error = verify(malformed.clone()).unwrap_err();
            assert_eq!(error.failure, Failure::Unavailable, "{malformed}");
            assert_eq!(
                error.reason,
                VerificationReason::MalformedClaims,
                "{malformed}"
            );
        }
    }

    #[test]
    fn introspection_access_token_returns_the_exact_presented_token() {
        use secrecy::ExposeSecret;
        let response = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
        let principal = validate_introspection_claims(
            response.to_string().as_bytes(),
            &policy(),
            100,
            secrecy::SecretString::from("verified-bearer-text"),
        )
        .unwrap();
        assert_eq!(
            principal.access_token().expose_secret(),
            "verified-bearer-text"
        );
    }
    // template:end oidc-introspection:authn-claims-introspection-shape-test

    // template:begin oidc-introspection:authn-claims-introspection-missing-test
    #[test]
    fn active_claims_are_required_and_null_means_absent() {
        let complete = serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"});
        let reason = |response: &serde_json::Value| {
            validate_introspection_claims(
                response.to_string().as_bytes(),
                &policy(),
                100,
                access_token(),
            )
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
                    access_token(),
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
                    response.to_string().into_bytes(),
                    &policy(),
                    TokenProfile::ResourceServer,
                    100,
                    access_token(),
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
