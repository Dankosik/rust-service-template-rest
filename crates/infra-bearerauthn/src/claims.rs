use crate::{
    Actor, PreparationError, Principal, VerificationError, VerificationReason,
    principal::{Identifiers, Identity},
};
use compact_str::CompactString;
use serde::Deserialize;
use smallvec::SmallVec;
use std::{borrow::Cow, fmt, sync::Arc, time::Duration};
// template:begin oidc-jwt:authn-claims-jwt-import
use crate::TokenProfile;
use serde_json::value::RawValue;
// template:end oidc-jwt:authn-claims-jwt-import

pub(crate) struct JsonObject<T>(pub(crate) T);

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for JsonObject<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for ObjectVisitor<T> {
            type Value = JsonObject<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(JsonObject)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(std::marker::PhantomData))
    }
}

/// RFC 7519 §4.1.4
const LEEWAY: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct ClaimPolicy {
    issuer: Arc<str>,
    audiences: Vec<String>,
}

impl ClaimPolicy {
    pub(crate) fn try_new(
        issuer: &str,
        mut audiences: Vec<String>,
    ) -> Result<Self, PreparationError> {
        if audiences.is_empty() || audiences.iter().any(|audience| audience.trim().is_empty()) {
            return Err(PreparationError::invalid_options());
        }
        audiences.sort_unstable();
        Ok(Self {
            issuer: Arc::from(issuer),
            audiences,
        })
    }

    fn check_issuer_and_audience(
        &self,
        issuer: &str,
        audience: &StringOrStrings<'_>,
    ) -> Result<(), VerificationError> {
        if issuer != self.issuer.as_ref() {
            return Err(VerificationError::invalid(VerificationReason::Issuer));
        }
        if !self.accepts_audience(audience) {
            return Err(VerificationError::invalid(VerificationReason::Audience));
        }
        Ok(())
    }

    fn accepts_audience(&self, audience: &StringOrStrings<'_>) -> bool {
        let values = match audience {
            StringOrStrings::One(value) => std::slice::from_ref(value),
            StringOrStrings::Many(values) => values.as_slice(),
        };
        if let [accepted] = self.audiences.as_slice() {
            return values.iter().any(|value| accepted == value);
        }
        values.iter().any(|value| {
            self.audiences
                .binary_search_by(|candidate| candidate.as_str().cmp(value.as_ref()))
                .is_ok()
        })
    }
}

enum StringOrStrings<'a> {
    One(Cow<'a, str>),
    Many(SmallVec<[Cow<'a, str>; 4]>),
}

impl<'de: 'a, 'a> Deserialize<'de> for StringOrStrings<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<'a>(std::marker::PhantomData<&'a ()>);
        impl<'de: 'a, 'a> serde::de::Visitor<'de> for Visitor<'a> {
            type Value = StringOrStrings<'a>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or an array of strings")
            }
            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                value: &'de str,
            ) -> Result<Self::Value, E> {
                Ok(StringOrStrings::One(Cow::Borrowed(value)))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StringOrStrings::One(Cow::Owned(value.to_owned())))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = SmallVec::with_capacity(seq.size_hint().unwrap_or(0).min(64));
                while let Some(BorrowableString(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(StringOrStrings::Many(values))
            }
        }
        deserializer.deserialize_any(Visitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
struct BorrowableString<'a>(#[serde(borrow)] Cow<'a, str>);

/// RFC 8693 §4.1
#[derive(Deserialize)]
struct RawAct<'a> {
    #[serde(borrow)]
    sub: Cow<'a, str>,
    #[serde(borrow)]
    client_id: Option<BorrowableString<'a>>,
}

struct EmptyActorSubject;

fn parse_actor(JsonObject(raw): JsonObject<RawAct<'_>>) -> Result<Actor, EmptyActorSubject> {
    if raw.sub.is_empty() {
        return Err(EmptyActorSubject);
    }
    Ok(Actor::new(
        CompactString::from(raw.sub),
        raw.client_id
            .map(|BorrowableString(value)| CompactString::from(value)),
    ))
}

// template:begin oidc-jwt:authn-claims-jwt-type
#[derive(Deserialize)]
struct JwtClaims<'a> {
    #[serde(borrow)]
    iss: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    aud: Option<StringOrStrings<'a>>,
    exp: Option<NumericDate>,
    #[serde(default, deserialize_with = "present_numeric_date")]
    nbf: Option<NumericDate>,
    #[serde(borrow)]
    sub: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    client_id: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    azp: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    appid: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    cid: Option<BorrowableString<'a>>,
    #[serde(borrow, default)]
    scope: ScopeClaim<'a>,
    #[serde(borrow)]
    scp: Option<StringOrStrings<'a>>,
    #[serde(borrow)]
    jti: Option<BorrowableString<'a>>,
    iat: Option<NumericDate>,
    #[serde(borrow)]
    act: Option<JsonObject<RawAct<'a>>>,
}

#[derive(Default)]
enum ScopeClaim<'a> {
    #[default]
    Missing,
    Null,
    Value(StringOrStrings<'a>),
}

impl<'a> ScopeClaim<'a> {
    fn resolve(
        self,
        profile: TokenProfile,
        scp: Option<StringOrStrings<'a>>,
    ) -> Result<Option<StringOrStrings<'a>>, VerificationError> {
        match self {
            Self::Null | Self::Value(StringOrStrings::Many(_))
                if profile == TokenProfile::Rfc9068 =>
            {
                Err(VerificationError::invalid(
                    VerificationReason::MalformedClaims,
                ))
            }
            Self::Missing | Self::Null => Ok(scp),
            Self::Value(scope) => Ok(Some(scope)),
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for ScopeClaim<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::deserialize(deserializer).map(|scope| scope.map_or(Self::Null, Self::Value))
    }
}
// template:end oidc-jwt:authn-claims-jwt-type

// template:begin oidc-introspection:authn-claims-introspection-envelope
#[derive(Deserialize)]
struct Envelope {
    active: bool,
}

#[derive(Deserialize)]
struct ActiveClaims<'a> {
    #[serde(borrow)]
    iss: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    aud: Option<StringOrStrings<'a>>,
    exp: Option<u64>,
    nbf: Option<u64>,
    #[serde(borrow)]
    sub: Option<BorrowableString<'a>>,
    #[serde(borrow)]
    client_id: Option<BorrowableString<'a>>,
    #[serde(borrow, default, deserialize_with = "present_scope_string")]
    scope: Option<Cow<'a, str>>,
    #[serde(borrow)]
    scp: Option<StringOrStrings<'a>>,
    #[serde(borrow)]
    act: Option<JsonObject<RawAct<'a>>>,
}

fn present_scope_string<'de: 'a, 'a, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Cow<'a, str>>, D::Error> {
    BorrowableString::deserialize(deserializer).map(|BorrowableString(scope)| Some(scope))
}
// template:end oidc-introspection:authn-claims-introspection-envelope

#[derive(Clone, Debug)]
pub(crate) struct VerifiedClaims {
    pub(crate) principal: Principal,
    not_before: Option<Duration>,
}

impl VerifiedClaims {
    pub(crate) fn check_at(&self, now: Duration) -> Result<(), VerificationError> {
        check_lifetime(self.principal.expiry_from_epoch(), self.not_before, now)
    }

    pub(crate) fn into_principal_checked_at(
        self,
        now: Duration,
    ) -> Result<Principal, VerificationError> {
        self.check_at(now)?;
        Ok(self.principal)
    }
}

// template:begin oidc-jwt:authn-claims-jwt-validation
pub(crate) fn validate_jwt_claims(
    payload: Vec<u8>,
    policy: &ClaimPolicy,
    token_profile: TokenProfile,
    now: Duration,
) -> Result<VerifiedClaims, VerificationError> {
    let invalid = VerificationError::invalid;
    let payload =
        String::from_utf8(payload).map_err(|_| invalid(VerificationReason::MalformedClaims))?;
    let (not_before, expiry, identifiers, scopes, actor) = {
        let JsonObject(claims) = serde_json::from_str::<JsonObject<JwtClaims<'_>>>(&payload)
            .map_err(|_| invalid(VerificationReason::MalformedClaims))?;
        let scope = claims.scope.resolve(token_profile, claims.scp)?;
        let (Some(BorrowableString(issuer)), Some(audience), Some(NumericDate(expiry))) =
            (claims.iss, claims.aud, claims.exp)
        else {
            return Err(invalid(VerificationReason::MissingClaim));
        };
        let not_before = claims.nbf.map(|NumericDate(date)| date);
        check_lifetime(expiry, not_before, now)?;
        policy.check_issuer_and_audience(&issuer, &audience)?;
        let identifiers = match token_profile {
            // RFC 9068 §2.2
            TokenProfile::Rfc9068 => {
                let (Some(subject), Some(client_id), Some(_), Some(_)) = (
                    non_empty(claims.sub),
                    non_empty(claims.client_id),
                    claims.iat,
                    claims.jti.filter(|BorrowableString(jti)| !jti.is_empty()),
                ) else {
                    return Err(invalid(VerificationReason::MissingClaim));
                };
                Identifiers::Subject {
                    subject,
                    client_id: Some(client_id),
                }
            }
            TokenProfile::ResourceServer => {
                let client_id = [claims.client_id, claims.azp, claims.appid, claims.cid]
                    .into_iter()
                    .find_map(non_empty);
                Identifiers::from_claims(non_empty(claims.sub), client_id)?
            }
        };
        let scopes = Scopes::from_claim(scope).ok_or_else(|| invalid(VerificationReason::Scope))?;
        let actor = claims
            .act
            .map(parse_actor)
            .transpose()
            .map_err(|EmptyActorSubject| invalid(VerificationReason::MalformedClaims))?;
        (not_before, expiry, identifiers, scopes, actor)
    };
    Ok(VerifiedClaims {
        not_before,
        principal: Principal::new(
            Identity {
                issuer: Arc::clone(&policy.issuer),
                identifiers,
                scopes,
                payload,
                actor,
            },
            expiry,
        ),
    })
}

/// RFC 7519 §2
struct NumericDate(Duration);

impl NumericDate {
    fn parse(raw: &RawValue) -> Option<Self> {
        let number = raw.get();
        if let Ok(seconds) = number.parse() {
            return Some(Self(Duration::from_secs(seconds)));
        }

        let (mantissa, exponent) = number.split_once(['e', 'E']).unwrap_or((number, "0"));
        let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
        if !unsigned.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            return None;
        }
        let (whole, fractional) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let digits = whole.bytes().chain(fractional.bytes());
        let Some(first_nonzero) = digits.clone().position(|digit| digit != b'0') else {
            return Some(Self(Duration::ZERO));
        };
        if mantissa.starts_with('-') {
            return None;
        }

        let exponent = exponent.parse::<i64>().unwrap_or_else(|_| {
            if exponent.starts_with('-') {
                i64::MIN
            } else {
                i64::MAX
            }
        });
        let decimal_point = i64::try_from(whole.len()).ok()? - i64::try_from(first_nonzero).ok()?;
        let nanosecond_digits = decimal_point.saturating_add(9).saturating_add(exponent);
        if nanosecond_digits > i64::from(Duration::MAX.as_nanos().ilog10() + 1) {
            return None;
        }
        if nanosecond_digits <= 0 {
            return Some(Self(Duration::from_nanos(1)));
        }

        let mut significant_digits = digits.skip(first_nonzero);
        let whole_nanoseconds = (0..nanosecond_digits).fold(0_u128, |value, _| {
            let digit = significant_digits.next().map_or(0, |digit| digit - b'0');
            value * 10 + u128::from(digit)
        });
        let nanoseconds =
            whole_nanoseconds + u128::from(significant_digits.any(|digit| digit != b'0'));
        (nanoseconds <= Duration::MAX.as_nanos())
            .then(|| Self(Duration::from_nanos_u128(nanoseconds)))
    }
}

impl<'de> Deserialize<'de> for NumericDate {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <&RawValue>::deserialize(deserializer)?;
        Self::parse(raw).ok_or_else(|| serde::de::Error::custom("invalid NumericDate"))
    }
}

fn present_numeric_date<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<NumericDate>, D::Error> {
    NumericDate::deserialize(deserializer).map(Some)
}
// template:end oidc-jwt:authn-claims-jwt-validation

// template:begin oidc-introspection:authn-claims-introspection-validation
impl VerifiedClaims {
    pub(crate) fn is_reusable_at(&self, now: Duration) -> bool {
        now < self.principal.expiry_from_epoch() && satisfies_not_before(self.not_before, now)
    }
}

/// RFC 7662 §2.2
pub(crate) fn validate_introspection_claims(
    bytes: &[u8],
    policy: &ClaimPolicy,
    now: Duration,
) -> Result<VerifiedClaims, VerificationError> {
    let malformed = || VerificationError::unavailable(VerificationReason::MalformedClaims);
    let invalid = VerificationError::invalid;
    let payload = std::str::from_utf8(bytes)
        .map_err(|_| malformed())?
        .trim_matches([' ', '\t', '\r', '\n']);
    let JsonObject(envelope) =
        serde_json::from_str::<JsonObject<Envelope>>(payload).map_err(|_| malformed())?;
    if !envelope.active {
        return Err(invalid(VerificationReason::Inactive));
    }
    let claims: ActiveClaims<'_> = serde_json::from_str(payload).map_err(|_| malformed())?;
    let (Some(BorrowableString(issuer)), Some(audience), Some(expiry)) =
        (claims.iss, claims.aud, claims.exp)
    else {
        return Err(invalid(VerificationReason::MissingClaim));
    };
    policy.check_issuer_and_audience(&issuer, &audience)?;
    let expiry = Duration::from_secs(expiry);
    let not_before = claims.nbf.map(Duration::from_secs);
    check_lifetime(expiry, not_before, now)?;
    let subject = non_empty(claims.sub);
    let client_id = non_empty(claims.client_id);
    let identifiers = Identifiers::from_claims(subject, client_id)?;
    let scopes = Scopes::from_claim(claims.scope.map(StringOrStrings::One).or(claims.scp))
        .ok_or_else(|| VerificationError::unavailable(VerificationReason::Scope))?;
    let actor = claims
        .act
        .map(parse_actor)
        .transpose()
        .map_err(|EmptyActorSubject| malformed())?;
    Ok(VerifiedClaims {
        not_before,
        principal: Principal::new(
            Identity {
                issuer: Arc::clone(&policy.issuer),
                identifiers,
                scopes,
                payload: payload.to_owned(),
                actor,
            },
            expiry,
        ),
    })
}
// template:end oidc-introspection:authn-claims-introspection-validation

fn check_lifetime(
    expiry: Duration,
    not_before: Option<Duration>,
    now: Duration,
) -> Result<(), VerificationError> {
    if now
        .checked_sub(LEEWAY)
        .is_some_and(|earliest_now| earliest_now >= expiry)
    {
        return Err(VerificationError::invalid(VerificationReason::Expired));
    }
    if !satisfies_not_before(not_before, now) {
        return Err(VerificationError::invalid(VerificationReason::NotYetValid));
    }
    Ok(())
}

fn satisfies_not_before(not_before: Option<Duration>, now: Duration) -> bool {
    not_before.is_none_or(|not_before| not_before <= now.saturating_add(LEEWAY))
}

impl Identifiers {
    fn from_claims(
        subject: Option<CompactString>,
        client_id: Option<CompactString>,
    ) -> Result<Self, VerificationError> {
        match (subject, client_id) {
            (Some(subject), client_id) => Ok(Self::Subject { subject, client_id }),
            (None, Some(client_id)) => Ok(Self::ClientOnly(client_id)),
            (None, None) => Err(VerificationError::invalid(VerificationReason::MissingClaim)),
        }
    }
}

fn non_empty(value: Option<BorrowableString<'_>>) -> Option<CompactString> {
    let BorrowableString(value) = value?;
    (!value.is_empty()).then(|| CompactString::from(value))
}

pub(crate) struct Scopes(Vec<String>);

impl Scopes {
    fn from_claim(scope: Option<StringOrStrings<'_>>) -> Option<Self> {
        let mut values: Vec<String> = match scope {
            None => Vec::new(),
            Some(StringOrStrings::One(value)) if value.is_empty() => Vec::new(),
            Some(StringOrStrings::One(value)) => value.split(' ').map(ToOwned::to_owned).collect(),
            Some(StringOrStrings::Many(values)) => {
                values.into_iter().map(Cow::into_owned).collect()
            }
        };
        if !values
            .iter()
            .all(|value| shared_rules::rfc6749::is_scope_token(value))
        {
            return None;
        }
        values.sort_unstable();
        values.dedup();
        Some(Self(values))
    }

    pub(crate) fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub(crate) fn contains_all(&self, required: impl IntoIterator<Item: AsRef<str>>) -> bool {
        required.into_iter().all(|scope| {
            let scope = scope.as_ref();
            self.0
                .binary_search_by(|candidate| candidate.as_str().cmp(scope))
                .is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ClaimPolicy;
    use crate::{Failure, VerificationReason, test_helpers::merged};

    #[test]
    fn json_object_requires_named_members() {
        #[derive(serde::Deserialize)]
        struct Metadata<'a> {
            #[serde(borrow)]
            label: &'a str,
        }

        let super::JsonObject(metadata) =
            serde_json::from_str::<super::JsonObject<Metadata<'_>>>(r#"{"label":"example"}"#)
                .unwrap();
        assert_eq!(metadata.label, "example");
        assert!(serde_json::from_str::<super::JsonObject<Metadata<'_>>>(r#"["example"]"#).is_err());
    }
    use std::time::Duration;

    // template:begin oidc-jwt:authn-claims-jwt-test-imports
    use super::validate_jwt_claims;
    use crate::TokenProfile;
    // template:end oidc-jwt:authn-claims-jwt-test-imports
    // template:begin oidc-introspection:authn-claims-introspection-test-import
    use super::validate_introspection_claims;
    // template:end oidc-introspection:authn-claims-introspection-test-import

    fn policy() -> ClaimPolicy {
        ClaimPolicy::try_new("https://issuer.example", vec!["api".to_owned()]).unwrap()
    }

    fn assert_actor_evidence(
        verify: impl Fn(serde_json::Value) -> Result<crate::Principal, crate::VerificationError>,
        malformed_failure: Failure,
    ) {
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
            let error = verify(malformed.clone()).unwrap_err();
            assert_eq!(error.failure, malformed_failure, "{malformed}");
            assert_eq!(
                error.reason,
                VerificationReason::MalformedClaims,
                "{malformed}"
            );
        }
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
                Duration::from_secs(100),
            )
            .map(|verified| verified.principal)
        };
        let principal = verify(serde_json::json!({
            "exp": 130, "sub": null, "client_id": "", "azp": "client", "cid": "other",
            "scope": "read write read",
        }))
        .unwrap();
        assert_eq!(principal.subject(), None);
        assert_eq!(principal.client_id(), Some("client"));
        assert_eq!(
            principal.identity(),
            crate::PrincipalIdentity::ClientOnly("client")
        );
        assert_eq!(principal.scopes(), ["read", "write"]);
        assert!(principal.has_scopes(["read", "write"]));
        assert!(!principal.has_scopes(["read", "admin"]));
        assert_eq!(principal.expires_at(), 130);
        assert_eq!(
            verify(serde_json::json!({"exp": 130, "sub": ""})).unwrap_err(),
            crate::VerificationError::invalid(VerificationReason::MissingClaim)
        );
    }

    fn verify_registered_claims(
        policy: &ClaimPolicy,
        extra: &serde_json::Value,
    ) -> Result<u64, VerificationReason> {
        let mut claims = serde_json::json!({
            "iss": "https://issuer.example", "aud": "api", "exp": 1000, "sub": "subject",
        });
        for (key, value) in extra.as_object().unwrap() {
            claims[key] = value.clone();
        }
        validate_jwt_claims(
            claims.to_string().into_bytes(),
            policy,
            TokenProfile::ResourceServer,
            Duration::from_secs(1000),
        )
        .map(|verified| verified.principal.expires_at())
        .map_err(|error| error.reason)
    }

    #[test]
    fn registered_claim_validation_enforces_shape_and_order() {
        let single_audience = policy();
        for (extra, expected) in [
            (serde_json::json!({}), Ok(1000)),
            (serde_json::json!({"aud": ["other", "api"]}), Ok(1000)),
            (serde_json::json!({"exp": 971, "nbf": 1030}), Ok(971)),
            (
                serde_json::json!({"nbf": 1030.4}),
                Err(VerificationReason::NotYetValid),
            ),
            (serde_json::json!({"exp": 970.4}), Ok(970)),
            (
                serde_json::json!({"iss": null}),
                Err(VerificationReason::MissingClaim),
            ),
            (
                serde_json::json!({"aud": null}),
                Err(VerificationReason::MissingClaim),
            ),
            (
                serde_json::json!({"exp": 970}),
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
            (
                serde_json::json!({"exp": 1, "iss": "other"}),
                Err(VerificationReason::Expired),
            ),
            (
                serde_json::json!({"nbf": "1"}),
                Err(VerificationReason::MalformedClaims),
            ),
            (serde_json::json!({"exp": 1000.4}), Ok(1000)),
            (
                serde_json::json!({"exp": 969.4}),
                Err(VerificationReason::Expired),
            ),
            (
                serde_json::json!({"exp": -1}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"exp": "1000"}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"iss": ["other", "https://issuer.example"]}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"iss": []}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"aud": [1]}),
                Err(VerificationReason::MalformedClaims),
            ),
        ] {
            assert_eq!(
                verify_registered_claims(&single_audience, &extra),
                expected,
                "{extra}"
            );
        }
    }

    #[test]
    fn any_configured_audience_can_match_a_string_or_array() {
        let multiple_audiences = ClaimPolicy::try_new(
            "https://issuer.example",
            Vec::from(["api", "accounts", "storefront", "billing"].map(str::to_owned)),
        )
        .unwrap();
        for (audience, expected) in [
            (serde_json::json!("api"), Ok(1000)),
            (serde_json::json!(["other", "api"]), Ok(1000)),
            (
                serde_json::json!(["API", "other"]),
                Err(VerificationReason::Audience),
            ),
        ] {
            assert_eq!(
                verify_registered_claims(
                    &multiple_audiences,
                    &serde_json::json!({"aud": audience})
                ),
                expected,
                "{audience}"
            );
        }
    }

    #[test]
    fn numeric_date_boundaries_preserve_exponents_subnanoseconds_and_wide_integers() {
        let verify = |exp: &str, nbf: &str, now: Duration| {
            let claims = format!(
                r#"{{"iss":"https://issuer.example","aud":"api","sub":"subject","exp":{exp},"nbf":{nbf}}}"#
            );
            validate_jwt_claims(
                claims.into_bytes(),
                &policy(),
                TokenProfile::ResourceServer,
                now,
            )
            .map(|verified| verified.principal.expires_at())
            .map_err(|error| error.reason)
        };
        for (exp, nbf, now, expected) in [
            ("1e-10", "0", Duration::from_secs(30), Ok(0)),
            (
                "1e-10",
                "0",
                Duration::new(30, 1),
                Err(VerificationReason::Expired),
            ),
            ("100", "1e-29", Duration::ZERO, Ok(100)),
            (
                "1e-999999999999999999999",
                "-0e+999999999999999999999",
                Duration::from_secs(30),
                Ok(0),
            ),
            (
                "1e999999999999999999999",
                "0",
                Duration::ZERO,
                Err(VerificationReason::MalformedClaims),
            ),
            (
                "100",
                "-1e-999999999999999999999",
                Duration::ZERO,
                Err(VerificationReason::MalformedClaims),
            ),
            (
                "100",
                "3.00000000001e1",
                Duration::ZERO,
                Err(VerificationReason::NotYetValid),
            ),
            ("100", "3.00000000001e1", Duration::from_nanos(1), Ok(100)),
            ("100", "-0.0", Duration::ZERO, Ok(100)),
            (
                "100",
                "-1e-10",
                Duration::ZERO,
                Err(VerificationReason::MalformedClaims),
            ),
            (
                "18446744073709551615",
                "18446744073709551615",
                Duration::from_secs(u64::MAX - 30),
                Ok(u64::MAX),
            ),
            (
                "18446744073709551615",
                "18446744073709551615",
                Duration::new(u64::MAX - 31, 999_999_999),
                Err(VerificationReason::NotYetValid),
            ),
            (
                "18446744073709551616",
                "0",
                Duration::ZERO,
                Err(VerificationReason::MalformedClaims),
            ),
        ] {
            assert_eq!(
                verify(exp, nbf, now),
                expected,
                "exp {exp}, nbf {nbf}, now {now:?}"
            );
        }
    }

    #[test]
    fn decimal_digits_beyond_the_clock_resolution_still_move_the_boundary() {
        let exp = br#"{"iss":"https://issuer.example","aud":"api","sub":"subject","exp":1700000000.40000000000000000000000000000001}"#;
        let nbf = br#"{"iss":"https://issuer.example","aud":"api","sub":"subject","exp":1800000000,"nbf":1700000000.40000000000000000000000000000001}"#;
        let admit = |claims: &[u8], now| {
            validate_jwt_claims(
                claims.to_vec(),
                &policy(),
                TokenProfile::ResourceServer,
                now,
            )
            .map(|_| ())
            .map_err(|error| error.reason)
        };
        let expiry = Duration::new(1_700_000_030, 400_000_000);
        let activation = Duration::new(1_699_999_970, 400_000_000);
        let nanosecond = Duration::from_nanos(1);
        assert_eq!(admit(exp, expiry), Ok(()));
        assert_eq!(
            admit(exp, expiry + nanosecond),
            Err(VerificationReason::Expired)
        );
        assert_eq!(admit(nbf, activation), Err(VerificationReason::NotYetValid));
        assert_eq!(admit(nbf, activation + nanosecond), Ok(()));
    }

    #[test]
    fn rfc9068_requires_its_claims() {
        let verify = |extra: serde_json::Value| {
            let claims = merged(
                serde_json::json!({
                    "iss": "https://issuer.example", "aud": "api",
                    "exp": 200, "sub": "subject", "client_id": "client", "jti": "id", "iat": 100,
                }),
                &extra,
            );
            validate_jwt_claims(
                claims.to_string().into_bytes(),
                &policy(),
                TokenProfile::Rfc9068,
                Duration::from_secs(100),
            )
            .map(|verified| verified.principal)
            .map_err(|error| error.reason)
        };
        assert!(verify(serde_json::json!({})).is_ok());
        for missing in ["sub", "client_id", "jti", "iat"] {
            assert_eq!(
                verify(serde_json::json!({ missing: null })).err(),
                Some(VerificationReason::MissingClaim),
                "{missing}"
            );
        }
        // RFC 9068 §2.2
        assert!(verify(serde_json::json!({"iat": 131})).is_ok());
        assert!(verify(serde_json::json!({"iat": 1e-29})).is_ok());
    }

    #[test]
    fn jwt_act_identifies_the_outermost_actor_and_rejects_malformed_evidence() {
        let verify = |extra: serde_json::Value| {
            let claims = merged(
                serde_json::json!({
                    "iss": "https://issuer.example", "aud": "api", "exp": 1000, "sub": "subject",
                }),
                &extra,
            );
            validate_jwt_claims(
                claims.to_string().into_bytes(),
                &policy(),
                TokenProfile::ResourceServer,
                Duration::from_secs(100),
            )
            .map(|verified| verified.principal)
        };
        assert_actor_evidence(verify, Failure::Invalid);
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
                validate_introspection_claims(response, &policy(), Duration::from_secs(100))
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
                validate_introspection_claims(response, &policy(), Duration::from_secs(100))
                    .unwrap_err();
            assert_eq!(error.failure, Failure::Unavailable, "{response:?}");
        }
    }

    #[test]
    fn typed_claim_access_reads_wide_integers_and_refuses_a_repeated_requested_member() {
        #[derive(serde::Deserialize)]
        struct ApplicationClaims {
            tenant: String,
            scope: String,
            sub: String,
            accounts: Vec<Account>,
        }
        #[derive(serde::Deserialize)]
        struct Account {
            id: u128,
            balance: i128,
        }
        #[derive(Debug, serde::Deserialize)]
        struct WrongShape {
            #[serde(rename = "tenant")]
            _tenant: u64,
        }
        #[derive(Debug, serde::Deserialize)]
        struct RepeatedMember {
            #[serde(rename = "region")]
            _region: String,
        }
        let principal = validate_introspection_claims(
            br#"{"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject","scope":"write read read","tenant":"private-value","region":"first","region":"second","accounts":[{"id":340282366920938463463374607431768211455,"balance":-170141183460469231731687303715884105728}]}"#,
            &policy(),
            Duration::from_secs(100),
        )
        .unwrap()
        .principal;
        let claims = principal.claims::<ApplicationClaims>().unwrap();
        assert_eq!(claims.tenant, "private-value");
        assert_eq!(claims.scope, "write read read");
        assert_eq!(claims.sub, "subject");
        assert_eq!(claims.accounts.len(), 1);
        assert_eq!(claims.accounts[0].id, u128::MAX);
        assert_eq!(claims.accounts[0].balance, i128::MIN);
        assert_eq!(principal.scopes(), ["read", "write"]);
        let error = principal.claims::<WrongShape>().unwrap_err();
        assert_eq!(error, crate::ClaimAccessError);
        assert!(!format!("{error:?} {error} {principal:?}").contains("private-value"));
        assert_eq!(
            principal.claims::<RepeatedMember>().unwrap_err(),
            crate::ClaimAccessError
        );
    }

    #[test]
    fn introspection_act_identifies_the_outermost_actor_and_rejects_malformed_evidence() {
        let verify = |extra: serde_json::Value| {
            let response = merged(
                serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"}),
                &extra,
            );
            validate_introspection_claims(
                response.to_string().as_bytes(),
                &policy(),
                Duration::from_secs(100),
            )
            .map(|verified| verified.principal)
        };
        assert_actor_evidence(verify, Failure::Unavailable);
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
                Duration::from_secs(100),
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
            ("exp", 70, VerificationReason::Expired),
            ("nbf", 131, VerificationReason::NotYetValid),
        ] {
            let mut response = complete.clone();
            response[claim] = value.into();
            assert_eq!(reason(&response), Err((Failure::Invalid, expected)));
        }
    }
    // template:end oidc-introspection:authn-claims-introspection-missing-test

    #[test]
    fn registered_scope_takes_precedence_over_the_provider_extension() {
        for (extra, expected, standard_shape) in [
            (
                serde_json::json!({"scope":"","scp":["read"]}),
                Some(vec![]),
                true,
            ),
            (
                serde_json::json!({"scope":"read","scp":[]}),
                Some(vec!["read"]),
                true,
            ),
            (
                serde_json::json!({"scp":"write read"}),
                Some(vec!["read", "write"]),
                true,
            ),
            (
                serde_json::json!({"scp":["read"]}),
                Some(vec!["read"]),
                true,
            ),
            (
                serde_json::json!({"scope":null,"scp":["read"]}),
                Some(vec!["read"]),
                false,
            ),
            (
                serde_json::json!({"scope":["write"],"scp":["read"]}),
                Some(vec!["write"]),
                false,
            ),
            (serde_json::json!({"scope":"","scp":[]}), Some(vec![]), true),
            (serde_json::json!({}), Some(vec![]), true),
            (serde_json::json!({"scope":42,"scp":"read"}), None, false),
            (serde_json::json!({"scope":"read","scp":42}), None, true),
            (serde_json::json!({"scope":"read  write"}), None, true),
            (serde_json::json!({"scope":["read write"]}), None, false),
        ] {
            let response = merged(
                serde_json::json!({"active":true,"iss":"https://issuer.example","aud":"api","exp":130,"sub":"subject"}),
                &extra,
            );
            // template:begin oidc-introspection:authn-claims-scope-introspection-assertion
            {
                let result = validate_introspection_claims(
                    &serde_json::to_vec(&response).unwrap(),
                    &policy(),
                    Duration::from_secs(100),
                );
                if let Some(expected) = expected.as_ref().filter(|_| standard_shape) {
                    assert_eq!(
                        result.unwrap().principal.scopes(),
                        expected.as_slice(),
                        "{extra}"
                    );
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
                    Duration::from_secs(100),
                );
                if let Some(expected) = &expected {
                    assert_eq!(
                        result.unwrap().principal.scopes(),
                        expected.as_slice(),
                        "{extra}, standard shape: {standard_shape}"
                    );
                } else {
                    assert_eq!(result.unwrap_err().failure, Failure::Invalid, "{extra}");
                }
            }
            // template:end oidc-jwt:authn-claims-scope-jwt-assertion
        }
    }
}
