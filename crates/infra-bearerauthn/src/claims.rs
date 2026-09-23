//! Strict, duplicate-aware parsing for the claims consumed by authentication.

// template:begin oidc-introspection:authn-claims-btree-set-import
use std::collections::BTreeSet;
// template:end oidc-introspection:authn-claims-btree-set-import

use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, Visitor},
};

use crate::{Failure, Principal};
// template:begin oidc-jwt:authn-claims-token-profile-import
use crate::TokenProfile;
// template:end oidc-jwt:authn-claims-token-profile-import

const LEEWAY_SECONDS: u128 = 30;

#[derive(Clone)]
pub(crate) struct ClaimPolicy {
    issuer: String,
    audience: String,
}

impl ClaimPolicy {
    pub(crate) fn new(issuer: String, audience: String) -> Self {
        Self { issuer, audience }
    }
}

// template:begin oidc-jwt:authn-validate-jwt-claims
/// Validates a verified JWT payload. Any malformed or failing evidence is a
/// token failure because the token itself supplied it.
pub(crate) fn validate_jwt_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    token_profile: TokenProfile,
    now_epoch_seconds: u64,
) -> Result<Principal, Failure> {
    let shape = ClaimShape::Jwt(token_profile);
    let raw = RawClaims::parse(bytes, shape, Failure::Invalid)?;
    let common = validate_common(&raw, policy, now_epoch_seconds, Failure::Invalid, shape)?;

    if token_profile == TokenProfile::Rfc9068 {
        let subject = required_identity(&raw.sub, Failure::Invalid)?;
        let client_id = required_identity(&raw.client_id, Failure::Invalid)?;
        let jti = raw.jti.value().filter(|value| !value.is_empty());
        let iat = required_number(&raw.iat, Failure::Invalid)?;
        if jti.is_none() {
            return Err(Failure::Invalid);
        }
        if u128::from(iat) > u128::from(now_epoch_seconds) + LEEWAY_SECONDS {
            return Err(Failure::Invalid);
        }
        return Ok(Principal::new(
            policy.issuer.clone(),
            Some(subject.to_owned()),
            Some(client_id.to_owned()),
            common.expiry_epoch_seconds,
        ));
    }

    Ok(Principal::new(
        policy.issuer.clone(),
        common.subject,
        common.client_id,
        common.expiry_epoch_seconds,
    ))
}
// template:end oidc-jwt:authn-validate-jwt-claims

// template:begin oidc-introspection:authn-validate-introspection-claims
/// Validates an RFC 7662 response. Malformed required provider evidence is an
/// unavailable provider, while a well-formed but nonmatching token is invalid.
pub(crate) fn validate_introspection_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    now_epoch_seconds: u64,
) -> Result<Principal, Failure> {
    if !parse_active(bytes)? {
        return Err(Failure::Invalid);
    }
    let shape = ClaimShape::Introspection;
    let raw = RawClaims::parse(bytes, shape, Failure::Unavailable)?;
    if !matches!(&raw.active, ClaimMember::Value(true)) {
        return Err(Failure::Unavailable);
    }
    let common = validate_common(&raw, policy, now_epoch_seconds, Failure::Unavailable, shape)?;
    Ok(Principal::new(
        policy.issuer.clone(),
        common.subject,
        common.client_id,
        common.expiry_epoch_seconds,
    ))
}
// template:end oidc-introspection:authn-validate-introspection-claims

struct CommonClaims {
    subject: Option<String>,
    client_id: Option<String>,
    expiry_epoch_seconds: u64,
}

fn validate_common(
    raw: &RawClaims,
    policy: &ClaimPolicy,
    now_epoch_seconds: u64,
    malformed: Failure,
    #[allow(
        unused_variables,
        reason = "JWT profile markers remove alias handling from introspection-only output"
    )]
    shape: ClaimShape,
) -> Result<CommonClaims, Failure> {
    let issuer = required_nonempty_text(&raw.iss, malformed)?;
    let audience = required_audience(&raw.aud, malformed)?;
    let expiry = required_number(&raw.exp, malformed)?;
    let not_before = optional_number(&raw.nbf, malformed)?;

    let subject = optional_identity(&raw.sub, malformed)?;
    let mut client_values = Vec::new();
    if raw.client_id.is_present() {
        client_values.push(required_identity(&raw.client_id, malformed)?);
    }
    // template:begin oidc-jwt:authn-claims-client-aliases
    if shape.aliases() {
        for value in [&raw.azp, &raw.appid, &raw.cid] {
            if value.is_present() {
                client_values.push(required_identity(value, malformed)?);
            }
        }
    }
    // template:end oidc-jwt:authn-claims-client-aliases
    let client_id = client_values.first().map(|value| (*value).to_owned());
    if let Some(first) = client_values.first()
        && client_values.iter().any(|value| *value != *first)
    {
        return Err(malformed);
    }
    if subject.is_none() && client_id.is_none() {
        return Err(malformed);
    }
    if issuer != policy.issuer || !audience.iter().any(|value| value == &policy.audience) {
        return Err(Failure::Invalid);
    }
    if u128::from(now_epoch_seconds) > u128::from(expiry) + LEEWAY_SECONDS
        || not_before
            .is_some_and(|value| u128::from(value) > u128::from(now_epoch_seconds) + LEEWAY_SECONDS)
    {
        return Err(Failure::Invalid);
    }

    Ok(CommonClaims {
        subject: subject.map(ToOwned::to_owned),
        client_id,
        expiry_epoch_seconds: expiry,
    })
}

fn required_text(value: &ClaimMember<String>, malformed: Failure) -> Result<&str, Failure> {
    value.value().map(String::as_str).ok_or(malformed)
}

fn required_nonempty_text(
    value: &ClaimMember<String>,
    malformed: Failure,
) -> Result<&str, Failure> {
    let value = required_text(value, malformed)?;
    if value.is_empty() {
        return Err(malformed);
    }
    Ok(value)
}

fn required_number(value: &ClaimMember<u64>, malformed: Failure) -> Result<u64, Failure> {
    value.value().copied().ok_or(malformed)
}

fn optional_number(value: &ClaimMember<u64>, malformed: Failure) -> Result<Option<u64>, Failure> {
    match value {
        ClaimMember::Missing => Ok(None),
        ClaimMember::Null => Err(malformed),
        ClaimMember::Value(value) => Ok(Some(*value)),
    }
}

fn required_audience(
    value: &ClaimMember<Audience>,
    malformed: Failure,
) -> Result<&[String], Failure> {
    value.value().map(Audience::values).ok_or(malformed)
}

fn optional_identity(
    value: &ClaimMember<String>,
    malformed: Failure,
) -> Result<Option<&str>, Failure> {
    if !value.is_present() {
        return Ok(None);
    }
    Ok(Some(required_identity(value, malformed)?))
}

fn required_identity(value: &ClaimMember<String>, malformed: Failure) -> Result<&str, Failure> {
    let value = required_text(value, malformed)?;
    if value.is_empty() || value.trim() != value {
        return Err(malformed);
    }
    Ok(value)
}

struct Audience(Vec<String>);

impl Audience {
    fn values(&self) -> &[String] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Audience {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AudienceVisitor;

        impl<'de> Visitor<'de> for AudienceVisitor {
            type Value = Audience;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string or a nonempty array of strings")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.is_empty() {
                    return Err(serde::de::Error::custom("audience must not be empty"));
                }
                Ok(Audience(vec![value.to_owned()]))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<String>()? {
                    values.push(value);
                }
                if values.is_empty() || values.iter().any(String::is_empty) {
                    return Err(serde::de::Error::custom("audience array must not be empty"));
                }
                Ok(Audience(values))
            }
        }

        deserializer.deserialize_any(AudienceVisitor)
    }
}

#[derive(Clone, Copy)]
enum ClaimShape {
    // template:begin oidc-jwt:authn-claim-shape-jwt
    Jwt(TokenProfile),
    // template:end oidc-jwt:authn-claim-shape-jwt
    // template:begin oidc-introspection:authn-claim-shape-introspection
    Introspection,
    // template:end oidc-introspection:authn-claim-shape-introspection
}

impl ClaimShape {
    // template:begin oidc-jwt:authn-claim-shape-aliases-jwt
    fn aliases(self) -> bool {
        matches!(self, Self::Jwt(_))
    }
    // template:end oidc-jwt:authn-claim-shape-aliases-jwt

    // template:begin oidc-jwt:authn-claim-shape-rfc9068
    fn rfc9068(self) -> bool {
        matches!(self, Self::Jwt(TokenProfile::Rfc9068))
    }
    // template:end oidc-jwt:authn-claim-shape-rfc9068

    // template:begin oidc-introspection:authn-claim-shape-active
    fn is_introspection(self) -> bool {
        matches!(self, Self::Introspection)
    }
    // template:end oidc-introspection:authn-claim-shape-active
}

/// A consumed JSON member keeps absence distinct from an explicit null.
#[derive(Default)]
enum ClaimMember<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<T> ClaimMember<T> {
    fn is_present(&self) -> bool {
        !matches!(self, Self::Missing)
    }

    fn value(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Null => None,
        }
    }
}

#[derive(Default)]
struct RawClaims {
    iss: ClaimMember<String>,
    aud: ClaimMember<Audience>,
    exp: ClaimMember<u64>,
    nbf: ClaimMember<u64>,
    sub: ClaimMember<String>,
    client_id: ClaimMember<String>,
    // template:begin oidc-jwt:authn-claims-alias-fields
    azp: ClaimMember<String>,
    appid: ClaimMember<String>,
    cid: ClaimMember<String>,
    // template:end oidc-jwt:authn-claims-alias-fields
    // template:begin oidc-jwt:authn-claims-rfc9068-fields
    iat: ClaimMember<u64>,
    jti: ClaimMember<String>,
    // template:end oidc-jwt:authn-claims-rfc9068-fields
    // template:begin oidc-introspection:authn-claims-active-fields
    active: ClaimMember<bool>,
    // template:end oidc-introspection:authn-claims-active-fields
}

impl RawClaims {
    fn parse(bytes: &[u8], shape: ClaimShape, failure: Failure) -> Result<Self, Failure> {
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        let result = deserializer.deserialize_map(RawClaimsVisitor { shape });
        result
            .and_then(|claims| deserializer.end().map(|()| claims))
            .map_err(|_| failure)
    }
}

struct RawClaimsVisitor {
    shape: ClaimShape,
}

impl<'de> Visitor<'de> for RawClaimsVisitor {
    type Value = RawClaims;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut claims = RawClaims::default();
        // template:begin oidc-introspection:authn-claims-all-member-duplicates
        let mut all_names = BTreeSet::new();
        // template:end oidc-introspection:authn-claims-all-member-duplicates
        while let Some(name) = map.next_key::<String>()? {
            // template:begin oidc-introspection:authn-claims-all-member-duplicate-check
            if self.shape.is_introspection() && !all_names.insert(name.clone()) {
                return Err(serde::de::Error::custom("duplicate introspection member"));
            }
            // template:end oidc-introspection:authn-claims-all-member-duplicate-check
            match name.as_str() {
                "iss" => set_once(&mut claims.iss, &mut map)?,
                "aud" => set_once(&mut claims.aud, &mut map)?,
                "exp" => set_once(&mut claims.exp, &mut map)?,
                "nbf" => set_once(&mut claims.nbf, &mut map)?,
                "sub" => set_once(&mut claims.sub, &mut map)?,
                "client_id" => set_once(&mut claims.client_id, &mut map)?,
                // template:begin oidc-jwt:authn-claims-alias-visitor
                "azp" if self.shape.aliases() => {
                    set_once(&mut claims.azp, &mut map)?;
                }
                "appid" if self.shape.aliases() => {
                    set_once(&mut claims.appid, &mut map)?;
                }
                "cid" if self.shape.aliases() => {
                    set_once(&mut claims.cid, &mut map)?;
                }
                // template:end oidc-jwt:authn-claims-alias-visitor
                // template:begin oidc-jwt:authn-claims-rfc9068-visitor
                "iat" if self.shape.rfc9068() => {
                    set_once(&mut claims.iat, &mut map)?;
                }
                "jti" if self.shape.rfc9068() => {
                    set_once(&mut claims.jti, &mut map)?;
                }
                // template:end oidc-jwt:authn-claims-rfc9068-visitor
                // template:begin oidc-introspection:authn-claims-active-visitor
                "active" if self.shape.is_introspection() => {
                    set_once(&mut claims.active, &mut map)?;
                }
                // template:end oidc-introspection:authn-claims-active-visitor
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(claims)
    }
}

// template:begin oidc-introspection:authn-parse-introspection-active
/// Reads only the discriminator first so an inactive response can ignore the
/// remaining claim meaning while still rejecting duplicate top-level names and
/// trailing JSON. A second strict pass is performed only for an active token.
fn parse_active(bytes: &[u8]) -> Result<bool, Failure> {
    struct ActiveVisitor;

    impl<'de> Visitor<'de> for ActiveVisitor {
        type Value = Option<bool>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON object with one boolean active member")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut names = BTreeSet::new();
            let mut active = None;
            while let Some(name) = map.next_key::<String>()? {
                if !names.insert(name.clone()) {
                    return Err(serde::de::Error::custom("duplicate introspection member"));
                }
                if name == "active" {
                    active = map.next_value::<Option<bool>>()?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(active)
        }
    }

    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let active = deserializer
        .deserialize_map(ActiveVisitor)
        .and_then(|active| deserializer.end().map(|()| active))
        .map_err(|_| Failure::Unavailable)?;
    active.ok_or(Failure::Unavailable)
}
// template:end oidc-introspection:authn-parse-introspection-active

fn set_once<'de, A, T>(destination: &mut ClaimMember<T>, map: &mut A) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
    T: Deserialize<'de>,
{
    if destination.is_present() {
        return Err(serde::de::Error::custom("duplicate consumed claim"));
    }
    *destination = match map.next_value::<Option<T>>()? {
        Some(value) => ClaimMember::Value(value),
        None => ClaimMember::Null,
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ClaimPolicy;
    // template:begin oidc-introspection:authn-claims-introspection-test-import
    use super::validate_introspection_claims;
    // template:end oidc-introspection:authn-claims-introspection-test-import
    // template:begin oidc-jwt:authn-claims-jwt-test-imports
    use super::validate_jwt_claims;
    use crate::TokenProfile;
    // template:end oidc-jwt:authn-claims-jwt-test-imports
    use crate::Failure;

    fn policy() -> ClaimPolicy {
        ClaimPolicy::new("https://issuer.example".to_owned(), "api".to_owned())
    }

    // template:begin oidc-jwt:authn-claims-resource-server-test
    #[test]
    fn resource_server_claims_require_identity_and_exact_audience() {
        let payload = br#"{"iss":"https://issuer.example","aud":["other","api"],"exp":130,"sub":"subject","azp":"client"}"#;
        let principal =
            validate_jwt_claims(payload, &policy(), TokenProfile::ResourceServer, 100).unwrap();

        assert_eq!(principal.issuer(), "https://issuer.example");
        assert_eq!(principal.subject(), Some("subject"));
        assert_eq!(principal.client_id(), Some("client"));
    }
    // template:end oidc-jwt:authn-claims-resource-server-test

    // template:begin oidc-jwt:authn-claims-jwt-rejection-test
    #[test]
    fn jwt_rejects_duplicate_consumed_members_and_whitespace_identity() {
        for payload in [
            br#"{"iss":"https://issuer.example","iss":"https://issuer.example","aud":"api","exp":130,"sub":"s"}"#.as_slice(),
            br#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":" subject"}"#.as_slice(),
        ] {
            assert_eq!(
                validate_jwt_claims(payload, &policy(), TokenProfile::ResourceServer, 100),
                Err(Failure::Invalid)
            );
        }
    }

    #[test]
    fn jwt_distinguishes_missing_null_and_duplicate_claims() {
        let absent_nbf = br#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":"s"}"#;
        assert!(
            validate_jwt_claims(absent_nbf, &policy(), TokenProfile::ResourceServer, 100).is_ok()
        );

        for payload in [
            br#"{"iss":"https://issuer.example","aud":"api","exp":130,"nbf":null,"sub":"s"}"#
                .as_slice(),
            br#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":null,"sub":"s"}"#
                .as_slice(),
        ] {
            assert_eq!(
                validate_jwt_claims(payload, &policy(), TokenProfile::ResourceServer, 100),
                Err(Failure::Invalid)
            );
        }
    }
    // template:end oidc-jwt:authn-claims-jwt-rejection-test

    // template:begin oidc-jwt:authn-claims-rfc9068-test
    #[test]
    fn rfc9068_requires_subject_client_jti_and_iat() {
        let valid = br#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":"s","client_id":"c","jti":"j","iat":120}"#;
        assert!(validate_jwt_claims(valid, &policy(), TokenProfile::Rfc9068, 100).is_ok());

        let missing_client = br#"{"iss":"https://issuer.example","aud":"api","exp":130,"sub":"s","azp":"c","jti":"j","iat":120}"#;
        assert_eq!(
            validate_jwt_claims(missing_client, &policy(), TokenProfile::Rfc9068, 100),
            Err(Failure::Invalid)
        );
    }
    // template:end oidc-jwt:authn-claims-rfc9068-test

    // template:begin oidc-introspection:authn-claims-introspection-classification-test
    #[test]
    fn introspection_separates_provider_contract_failure_from_invalid_token() {
        let inactive = br#"{"active":false,"iss":false}"#;
        assert_eq!(
            validate_introspection_claims(inactive, &policy(), 100),
            Err(Failure::Invalid)
        );

        let malformed =
            br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":null}"#;
        assert_eq!(
            validate_introspection_claims(malformed, &policy(), 100),
            Err(Failure::Unavailable)
        );

        let wrong_audience =
            br#"{"active":true,"iss":"https://issuer.example","aud":"other","exp":130,"sub":"s"}"#;
        assert_eq!(
            validate_introspection_claims(wrong_audience, &policy(), 100),
            Err(Failure::Invalid)
        );

        for malformed_audience in [
            br#"{"active":true,"iss":"https://issuer.example","aud":"","exp":130,"sub":"s"}"#.as_slice(),
            br#"{"active":true,"iss":"https://issuer.example","aud":["api",""],"exp":130,"sub":"s"}"#.as_slice(),
        ] {
            assert_eq!(
                validate_introspection_claims(malformed_audience, &policy(), 100),
                Err(Failure::Unavailable)
            );
        }
    }
    // template:end oidc-introspection:authn-claims-introspection-classification-test
}
