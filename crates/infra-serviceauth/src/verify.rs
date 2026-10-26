//! Service token verification: one header and one payload parse, then the
//! signature against the presenting caller's keys only.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, PoisonError},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::Algorithm;
use serde::{Deserialize, Deserializer, de};

use crate::{
    Caller, CallerName, JwtType, LEEWAY_SECONDS, MAX_TOKEN_LIFETIME_SECONDS, TrustRegistry,
};

/// Most verified tokens remembered at once. Callers reuse a token for half its
/// lifetime, so a receiver holds about two per caller handle.
const SEEN_TOKENS: usize = 1024;

/// Verified tokens by their bytes, with their caller and `exp`.
type Seen = HashMap<Box<[u8]>, (Caller, u64)>;

/// Service token verification outcomes by closed reason.
pub const VERIFICATIONS_METRIC: &str = "service_auth_verifications_total";

/// The fixed outcomes a transport maps to its response.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Failure {
    #[error("service token is required")]
    Missing,
    #[error("service token is malformed")]
    Malformed,
    #[error("service token is invalid")]
    Invalid,
    #[error("service token verification is unavailable")]
    Unavailable,
}

/// A closed reason for operators. Never derived from token contents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Reason {
    Missing,
    Malformed,
    Header,
    Algorithm,
    Type,
    KeyId,
    Claims,
    Issuer,
    UnknownKey,
    Signature,
    Subject,
    Audience,
    Expired,
    NotYetValid,
    Lifetime,
    Clock,
}

impl Reason {
    const fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Malformed => "malformed",
            Self::Header => "header",
            Self::Algorithm => "algorithm",
            Self::Type => "type",
            Self::KeyId => "key_id",
            Self::Claims => "claims",
            Self::Issuer => "issuer",
            Self::UnknownKey => "unknown_key",
            Self::Signature => "signature",
            Self::Subject => "subject",
            Self::Audience => "audience",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::Lifetime => "lifetime",
            Self::Clock => "clock",
        }
    }

    const fn failure(self) -> Failure {
        match self {
            Self::Missing => Failure::Missing,
            Self::Malformed => Failure::Malformed,
            Self::Clock => Failure::Unavailable,
            _ => Failure::Invalid,
        }
    }
}

/// Verifies service tokens addressed to one receiver. Cloning shares it.
#[derive(Clone)]
pub struct Verifier(Arc<Inner>);

struct Inner {
    audience: CallerName,
    jwt_type: JwtType,
    trust: TrustRegistry,
    /// Registered once: a success is the hot path, and a registry lookup per
    /// call costs more than the increment.
    verified: metrics::Counter,
    /// Tokens that passed every check, with their caller and `exp`. Every
    /// check but expiry depends only on the token's bytes and on the audience,
    /// type and registry fixed for the process, so the same bytes pass again
    /// until they expire. It is not a replay cache (S5): nothing is refused for
    /// having been seen.
    seen: Mutex<Seen>,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verifier")
            .field("audience", &self.0.audience)
            .field("trusted_callers", &self.0.trust.len())
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// A verifier for tokens addressed to `audience`, this service's name.
    /// Bootstrap installs the metrics recorder first.
    #[must_use]
    pub fn new(audience: CallerName, jwt_type: JwtType, trust: TrustRegistry) -> Self {
        metrics::describe_counter!(
            VERIFICATIONS_METRIC,
            metrics::Unit::Count,
            "Service token verifications by outcome and closed reason."
        );
        Self(Arc::new(Inner {
            audience,
            jwt_type,
            trust,
            verified: metrics::counter!(VERIFICATIONS_METRIC, "outcome" => "success", "reason" => "verified"),
            seen: Mutex::new(HashMap::new()),
        }))
    }

    #[must_use]
    pub fn trusted_callers(&self) -> usize {
        self.0.trust.len()
    }

    pub fn callers(&self) -> impl Iterator<Item = &CallerName> {
        self.0.trust.callers()
    }

    /// Verifies the `Authorization` values of one call and names its caller.
    /// Records one outcome; a failure logs only its closed reason.
    ///
    /// # Errors
    /// Returns [`Failure::Missing`] without a `Bearer` value,
    /// [`Failure::Malformed`] for several values or bearer syntax outside
    /// RFC 6750, [`Failure::Invalid`] for any token outside the contract, and
    /// [`Failure::Unavailable`] when the system clock precedes the Unix epoch.
    pub fn authenticate<'a>(
        &self,
        authorization: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Caller, Failure> {
        let result =
            bearer(authorization.into_iter()).and_then(|token| self.remembered(token, unix_now()?));
        match result {
            Ok(caller) => {
                self.0.verified.increment(1);
                Ok(caller)
            }
            Err(reason) => {
                metrics::counter!(VERIFICATIONS_METRIC, "outcome" => "failure", "reason" => reason.label()).increment(1);
                tracing::debug!(reason = reason.label(), "service_auth_verification_failed");
                Err(reason.failure())
            }
        }
    }

    /// Every check at `now`, without repeating the signature check for a
    /// token that already passed and has not expired.
    fn remembered(&self, token: &[u8], now: u64) -> Result<Caller, Reason> {
        let mut seen = self.0.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((caller, expires_at)) = seen.get(token) {
            if now <= expires_at.saturating_add(LEEWAY_SECONDS) {
                return Ok(caller.clone());
            }
            seen.remove(token);
        }
        drop(seen);
        let (caller, expires_at) = self.checked(token, || Ok(now))?;
        let mut seen = self.0.seen.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.len() >= SEEN_TOKENS {
            seen.retain(|_, (_, expires_at)| now <= expires_at.saturating_add(LEEWAY_SECONDS));
            if seen.len() >= SEEN_TOKENS {
                seen.clear();
            }
        }
        seen.insert(token.into(), (caller.clone(), expires_at));
        Ok(caller)
    }

    #[cfg(test)]
    pub(crate) fn verify(
        &self,
        token: &[u8],
        now: impl FnOnce() -> Result<u64, Reason>,
    ) -> Result<Caller, Reason> {
        self.checked(token, now).map(|(caller, _)| caller)
    }

    /// Every check of the contract; the caller and the token's `exp`.
    fn checked(
        &self,
        token: &[u8],
        now: impl FnOnce() -> Result<u64, Reason>,
    ) -> Result<(Caller, u64), Reason> {
        let (message, signature) = split_last_dot(token).ok_or(Reason::Header)?;
        let (header, payload) = split_last_dot(message).ok_or(Reason::Header)?;
        let header = URL_SAFE_NO_PAD.decode(header).map_err(|_| Reason::Header)?;
        let header: TokenHeader<'_> =
            serde_json::from_slice(&header).map_err(|_| Reason::Header)?;
        // The contract defines no extension, so `crit` is never understood.
        if header.crit {
            return Err(Reason::Header);
        }
        if header.alg != Algorithm::ES256 {
            return Err(Reason::Algorithm);
        }
        if !header
            .typ
            .as_deref()
            .is_some_and(|typ| self.0.jwt_type.admits(typ))
        {
            return Err(Reason::Type);
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty())
            .ok_or(Reason::KeyId)?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| Reason::Claims)?;
        let payload = std::str::from_utf8(&payload).map_err(|_| Reason::Claims)?;
        let claims: Claims<'_> = serde_json::from_str(payload).map_err(|_| Reason::Claims)?;
        // The key comes only from the caller this token names.
        let issuer = claims.iss.as_deref().ok_or(Reason::Issuer)?;
        let (caller, keys) = self.0.trust.caller(issuer).ok_or(Reason::Issuer)?;
        let key = keys
            .iter()
            .find(|key| key.kid == kid)
            .ok_or(Reason::UnknownKey)?;
        let mut signature_bytes = [0_u8; 64];
        let length = URL_SAFE_NO_PAD
            .decode_slice(signature, &mut signature_bytes)
            .map_err(|_| Reason::Signature)?;
        if length != signature_bytes.len() {
            return Err(Reason::Signature);
        }
        key.key
            .verify_sig(message, &signature_bytes)
            .map_err(|_| Reason::Signature)?;
        if claims.sub.as_deref() != Some(issuer) {
            return Err(Reason::Subject);
        }
        if !claims
            .aud
            .as_ref()
            .is_some_and(|audience| audience.is_only(self.0.audience.as_str()))
        {
            return Err(Reason::Audience);
        }
        let (Some(NumericDate(issued_at)), Some(NumericDate(expires_at))) =
            (claims.iat, claims.exp)
        else {
            return Err(Reason::Claims);
        };
        if expires_at <= issued_at || expires_at - issued_at > MAX_TOKEN_LIFETIME_SECONDS {
            return Err(Reason::Lifetime);
        }
        let now = now()?;
        if now > expires_at.saturating_add(LEEWAY_SECONDS) {
            return Err(Reason::Expired);
        }
        let earliest = now.saturating_add(LEEWAY_SECONDS);
        if issued_at > earliest || claims.nbf.is_some_and(|not_before| not_before > earliest) {
            return Err(Reason::NotYetValid);
        }
        Ok((
            Caller {
                name: caller.clone(),
            },
            expires_at,
        ))
    }
}

/// Exactly one `Authorization` value with an RFC 6750 `Bearer` credential:
/// the scheme, one or more spaces, and a `b64token`. A foreign scheme is
/// missing evidence and is never parsed.
fn bearer<'a>(mut values: impl Iterator<Item = &'a [u8]>) -> Result<&'a [u8], Reason> {
    let value = values.next().ok_or(Reason::Missing)?;
    if values.next().is_some() {
        return Err(Reason::Malformed);
    }
    let (scheme, rest) = value.split_at(
        value
            .iter()
            .position(|byte| *byte == b' ')
            .unwrap_or(value.len()),
    );
    let tchar = |byte: &u8| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(byte);
    if scheme.is_empty() || !scheme.iter().all(tchar) {
        return Err(Reason::Malformed);
    }
    if !scheme.eq_ignore_ascii_case(b"Bearer") {
        return Err(Reason::Missing);
    }
    let token = &rest[rest
        .iter()
        .position(|byte| *byte != b' ')
        .unwrap_or(rest.len())..];
    let padding = token.iter().rev().take_while(|byte| **byte == b'=').count();
    let b64token = |byte: &u8| byte.is_ascii_alphanumeric() || b"-._~+/".contains(byte);
    if token.len() == padding || !token[..token.len() - padding].iter().all(b64token) {
        return Err(Reason::Malformed);
    }
    Ok(token)
}

fn split_last_dot(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let dot = bytes.iter().rposition(|byte| *byte == b'.')?;
    Some((&bytes[..dot], &bytes[dot + 1..]))
}

fn unix_now() -> Result<u64, Reason> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| Reason::Clock)
}

/// The header members this crate reads; no member ever selects a key, and
/// every other member is ignored. A repeated member, or one of the wrong
/// type, is malformed.
#[derive(Deserialize)]
struct TokenHeader<'a> {
    alg: Algorithm,
    #[serde(borrow)]
    typ: Option<Cow<'a, str>>,
    #[serde(borrow)]
    kid: Option<Cow<'a, str>>,
    /// Present in any form, `null` included.
    #[serde(default, deserialize_with = "present")]
    crit: bool,
}

/// The claims this crate reads; every other claim is ignored. A repeated
/// claim, or one of the wrong type, is malformed.
#[derive(Deserialize)]
struct Claims<'a> {
    #[serde(borrow)]
    iss: Option<Cow<'a, str>>,
    #[serde(borrow)]
    sub: Option<Cow<'a, str>>,
    #[serde(borrow)]
    aud: Option<Audience<'a>>,
    iat: Option<NumericDate>,
    exp: Option<NumericDate>,
    /// A `null` or non-numeric `nbf` is malformed, not absent.
    #[serde(default, deserialize_with = "present_numeric_date")]
    nbf: Option<u64>,
}

/// `aud` as RFC 7519 allows it: one string or an array of strings.
#[derive(Deserialize)]
#[serde(untagged)]
enum Audience<'a> {
    One(#[serde(borrow)] Cow<'a, str>),
    Many(#[serde(borrow)] Vec<Cow<'a, str>>),
}

impl Audience<'_> {
    /// Exactly one audience, and it is `name`.
    fn is_only(&self, name: &str) -> bool {
        match self {
            Self::One(audience) => audience == name,
            Self::Many(audiences) => matches!(audiences.as_slice(), [audience] if audience == name),
        }
    }
}

/// An RFC 7519 `NumericDate`: an unsigned integer, or a finite non-negative
/// number rounded to one.
struct NumericDate(u64);

impl<'de> Deserialize<'de> for NumericDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = NumericDate;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a NumericDate")
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(NumericDate(value))
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "the range check above the cast bounds the value"
            )]
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                if value.is_finite() && value >= 0.0 && value < u64::MAX as f64 {
                    Ok(NumericDate(value.round() as u64))
                } else {
                    Err(E::custom("NumericDate out of range"))
                }
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

fn present_numeric_date<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    NumericDate::deserialize(deserializer).map(|date| Some(date.0))
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    de::IgnoredAny::deserialize(deserializer).map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestKey, registry};

    const NOW: u64 = 1_800_000_000;
    const TYPE: &str = "example-svc+jwt";

    type Change = Box<dyn Fn(&mut serde_json::Value)>;

    struct Case {
        consultant: TestKey,
        console: TestKey,
        verifier: Verifier,
    }

    impl Case {
        fn new() -> Self {
            let consultant = TestKey::new("consultant/2026-10");
            let console = TestKey::new("console/2026-10");
            let trust = TrustRegistry::parse(&registry(&[
                ("consultant", &[&consultant]),
                ("console", &[&console]),
            ]))
            .unwrap();
            let verifier = Verifier::new(
                CallerName::parse("catalog").unwrap(),
                JwtType::parse(TYPE).unwrap(),
                trust,
            );
            Self {
                consultant,
                console,
                verifier,
            }
        }

        fn header(key: &TestKey) -> serde_json::Value {
            serde_json::json!({"alg": "ES256", "typ": TYPE, "kid": key.kid()})
        }

        fn claims() -> serde_json::Value {
            serde_json::json!({
                "iss": "consultant", "sub": "consultant", "aud": "catalog",
                "iat": NOW, "exp": NOW + 60, "jti": "ignored",
            })
        }

        fn check(&self, token: &str) -> Result<String, Reason> {
            self.verifier
                .verify(token.as_bytes(), || Ok(NOW))
                .map(|caller| caller.name().to_owned())
        }

        fn signed(
            &self,
            change: impl FnOnce(&mut serde_json::Value, &mut serde_json::Value),
        ) -> String {
            let mut header = Self::header(&self.consultant);
            let mut claims = Self::claims();
            change(&mut header, &mut claims);
            self.consultant.sign(&header, &claims)
        }
    }

    #[test]
    fn a_verified_token_is_remembered_only_until_it_expires() {
        let case = Case::new();
        let token = case.signed(|_, _| {});
        let name = |result: Result<Caller, Reason>| result.map(|caller| caller.name().to_owned());
        assert_eq!(
            name(case.verifier.remembered(token.as_bytes(), NOW)),
            Ok("consultant".into())
        );
        assert_eq!(case.verifier.0.seen.lock().unwrap().len(), 1);
        assert_eq!(
            name(
                case.verifier
                    .remembered(token.as_bytes(), NOW + 60 + LEEWAY_SECONDS)
            ),
            Ok("consultant".into())
        );
        assert_eq!(
            name(
                case.verifier
                    .remembered(token.as_bytes(), NOW + 61 + LEEWAY_SECONDS)
            ),
            Err(Reason::Expired)
        );
        assert!(case.verifier.0.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn a_refused_token_is_never_remembered_and_memory_stays_bounded() {
        let case = Case::new();
        let forged = case.signed(|_, claims| claims["aud"] = "other".into());
        for _ in 0..2 {
            assert_eq!(
                case.verifier.remembered(forged.as_bytes(), NOW).map(|_| ()),
                Err(Reason::Audience)
            );
        }
        assert!(case.verifier.0.seen.lock().unwrap().is_empty());
        for index in 0..=SEEN_TOKENS {
            let token = case.signed(|_, claims| claims["jti"] = index.into());
            assert!(case.verifier.remembered(token.as_bytes(), NOW).is_ok());
        }
        assert!(case.verifier.0.seen.lock().unwrap().len() <= SEEN_TOKENS);
    }

    #[test]
    fn a_contract_token_names_its_caller() {
        let case = Case::new();
        assert_eq!(
            case.check(&case.signed(|_, _| {})),
            Ok("consultant".to_owned())
        );
        // The audience may be a one-element array, typ a full media type,
        // and an unknown claim or a fractional date is not a reason to refuse.
        let token = case.signed(|header, claims| {
            header["typ"] = "Application/Example-SVC+JWT".into();
            claims["aud"] = serde_json::json!(["catalog"]);
            claims["nbf"] = NOW.into();
            claims["exp"] = serde_json::json!(1_800_000_059.6_f64);
            claims["store"] = "anything".into();
        });
        assert_eq!(case.check(&token), Ok("consultant".to_owned()));
        let console = case.console.sign(
            &Case::header(&case.console),
            &serde_json::json!({"iss": "console", "sub": "console", "aud": "catalog", "iat": NOW, "exp": NOW + 30}),
        );
        assert_eq!(case.check(&console), Ok("console".to_owned()));
    }

    #[test]
    fn another_issuers_key_never_verifies_a_caller() {
        let case = Case::new();
        // console's key and kid, claiming to be consultant: the kid is looked
        // up among consultant's keys only.
        let header = Case::header(&case.console);
        let forged = case.console.sign(&header, &Case::claims());
        assert_eq!(case.check(&forged), Err(Reason::UnknownKey));
        // console's key under consultant's kid: the signature fails.
        let forged = case
            .console
            .sign(&Case::header(&case.consultant), &Case::claims());
        assert_eq!(case.check(&forged), Err(Reason::Signature));
        // An issuer the registry does not name.
        let unknown = case.signed(|_, claims| {
            claims["iss"] = "catalog".into();
            claims["sub"] = "catalog".into();
        });
        assert_eq!(case.check(&unknown), Err(Reason::Issuer));
    }

    #[test]
    fn audience_lifetime_and_identity_follow_the_contract() {
        let case = Case::new();
        let cases: Vec<(Change, Reason)> = vec![
            (
                Box::new(|claims| claims["aud"] = "llm-gateway".into()),
                Reason::Audience,
            ),
            (
                Box::new(|claims| claims["aud"] = serde_json::json!(["catalog", "console"])),
                Reason::Audience,
            ),
            (
                Box::new(|claims| claims["aud"] = serde_json::json!([])),
                Reason::Audience,
            ),
            (
                Box::new(|claims| {
                    claims.as_object_mut().unwrap().remove("aud");
                }),
                Reason::Audience,
            ),
            (
                Box::new(|claims| claims["sub"] = "console".into()),
                Reason::Subject,
            ),
            (
                Box::new(|claims| {
                    claims.as_object_mut().unwrap().remove("sub");
                }),
                Reason::Subject,
            ),
            (
                Box::new(|claims| {
                    claims.as_object_mut().unwrap().remove("iss");
                }),
                Reason::Issuer,
            ),
            (
                Box::new(|claims| {
                    claims.as_object_mut().unwrap().remove("iat");
                }),
                Reason::Claims,
            ),
            (
                Box::new(|claims| {
                    claims.as_object_mut().unwrap().remove("exp");
                }),
                Reason::Claims,
            ),
            (
                Box::new(|claims| claims["exp"] = (NOW + 61).into()),
                Reason::Lifetime,
            ),
            (
                Box::new(|claims| claims["exp"] = NOW.into()),
                Reason::Lifetime,
            ),
            (
                Box::new(|claims| {
                    claims["iat"] = (NOW - 120).into();
                    claims["exp"] = (NOW - 61).into();
                }),
                Reason::Expired,
            ),
            (
                Box::new(|claims| {
                    claims["iat"] = (NOW + 31).into();
                    claims["exp"] = (NOW + 90).into();
                }),
                Reason::NotYetValid,
            ),
            (
                Box::new(|claims| claims["nbf"] = (NOW + 31).into()),
                Reason::NotYetValid,
            ),
            (
                Box::new(|claims| claims["nbf"] = serde_json::Value::Null),
                Reason::Claims,
            ),
            (Box::new(|claims| claims["iss"] = 7.into()), Reason::Claims),
            (
                Box::new(|claims| claims["exp"] = "soon".into()),
                Reason::Claims,
            ),
        ];
        for (change, expected) in cases {
            let token = case.signed(|_, claims| change(claims));
            assert_eq!(case.check(&token), Err(expected), "{token}");
        }
        // Inside the 30-second allowance on both sides.
        let token = case.signed(|_, claims| {
            claims["iat"] = (NOW - 89).into();
            claims["exp"] = (NOW - 30).into();
        });
        assert_eq!(case.check(&token), Ok("consultant".to_owned()));
        let token = case.signed(|_, claims| {
            claims["iat"] = (NOW + 30).into();
            claims["exp"] = (NOW + 90).into();
        });
        assert_eq!(case.check(&token), Ok("consultant".to_owned()));
    }

    #[test]
    fn the_header_selects_nothing_but_the_contract_algorithm_type_and_kid() {
        let case = Case::new();
        let cases: Vec<(Change, Reason)> = vec![
            (
                Box::new(|header| header["alg"] = "none".into()),
                Reason::Header,
            ),
            (
                Box::new(|header| header["alg"] = "HS256".into()),
                Reason::Algorithm,
            ),
            (
                Box::new(|header| header["alg"] = "RS256".into()),
                Reason::Algorithm,
            ),
            (
                Box::new(|header| header["typ"] = "JWT".into()),
                Reason::Type,
            ),
            (
                Box::new(|header| header["typ"] = "at+jwt".into()),
                Reason::Type,
            ),
            (
                Box::new(|header| {
                    header.as_object_mut().unwrap().remove("typ");
                }),
                Reason::Type,
            ),
            (
                Box::new(|header| {
                    header.as_object_mut().unwrap().remove("kid");
                }),
                Reason::KeyId,
            ),
            (Box::new(|header| header["kid"] = "".into()), Reason::KeyId),
            (
                Box::new(|header| header["crit"] = serde_json::json!(["exp"])),
                Reason::Header,
            ),
            (
                Box::new(|header| header["crit"] = serde_json::json!([])),
                Reason::Header,
            ),
            (
                Box::new(|header| header["crit"] = serde_json::Value::Null),
                Reason::Header,
            ),
            (
                Box::new(|header| header["kid"] = serde_json::Value::Null),
                Reason::KeyId,
            ),
            (
                Box::new(|header| header["typ"] = serde_json::Value::Null),
                Reason::Type,
            ),
        ];
        for (change, expected) in cases {
            let token = case.signed(|header, _| change(header));
            assert_eq!(case.check(&token), Err(expected), "{token}");
        }
        // A key embedded in the header is never used: console's public key
        // offered for a token signed by console and claiming consultant.
        let mut header = Case::header(&case.consultant);
        header["jwk"] = case.console.jwk();
        let forged = case.console.sign(&header, &Case::claims());
        assert_eq!(case.check(&forged), Err(Reason::Signature));
        // A repeated header member.
        let duplicate = case.consultant.sign_raw(
            format!(
                r#"{{"alg":"ES256","typ":"{TYPE}","kid":"{}","kid":"other"}}"#,
                case.consultant.kid()
            )
            .as_bytes(),
            Case::claims().to_string().as_bytes(),
        );
        assert_eq!(case.check(&duplicate), Err(Reason::Header));
        // A repeated claim the verifier reads.
        let duplicate = case.consultant.sign_raw(
            Case::header(&case.consultant).to_string().as_bytes(),
            format!(r#"{{"iss":"consultant","sub":"consultant","aud":"catalog","aud":"console","iat":{NOW},"exp":{}}}"#, NOW + 60).as_bytes(),
        );
        assert_eq!(case.check(&duplicate), Err(Reason::Claims));
    }

    #[test]
    fn signatures_and_compact_form_are_checked_exactly() {
        let case = Case::new();
        let token = case.signed(|_, _| {});
        let (message, _) = token.rsplit_once('.').unwrap();
        for signature in [
            String::new(),
            URL_SAFE_NO_PAD.encode([0_u8; 64]),
            URL_SAFE_NO_PAD.encode([1_u8; 63]),
            URL_SAFE_NO_PAD.encode([1_u8; 65]),
            "!!".to_owned(),
        ] {
            assert_eq!(
                case.check(&format!("{message}.{signature}")),
                Err(Reason::Signature),
                "{signature}"
            );
        }
        // A changed payload under the original signature.
        let (header, rest) = token.split_once('.').unwrap();
        let (_, signature) = rest.split_once('.').unwrap();
        let mut claims = Case::claims();
        claims["exp"] = (NOW + 59).into();
        let tampered = format!(
            "{header}.{}.{signature}",
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        assert_eq!(case.check(&tampered), Err(Reason::Signature));
        for malformed in ["", "a", "a.b", "a.b.c.d", "e30.e30.", "!.e30.AA"] {
            assert!(
                matches!(
                    case.check(malformed),
                    Err(Reason::Header | Reason::Claims | Reason::Signature)
                ),
                "{malformed}"
            );
        }
        let not_utf8 = case.consultant.sign_raw(
            Case::header(&case.consultant).to_string().as_bytes(),
            b"{\"iss\":\"\xff\"}",
        );
        assert_eq!(case.check(&not_utf8), Err(Reason::Claims));
    }

    #[test]
    fn the_envelope_distinguishes_missing_from_malformed() {
        assert_eq!(bearer(std::iter::empty()), Err(Reason::Missing));
        assert_eq!(
            bearer([b"Basic abc".as_slice()].into_iter()),
            Err(Reason::Missing)
        );
        assert_eq!(
            bearer([b"bEaReR  a.b.c".as_slice()].into_iter()),
            Ok(b"a.b.c".as_slice())
        );
        assert_eq!(
            bearer([b"Bearer a.b=".as_slice()].into_iter()),
            Ok(b"a.b=".as_slice())
        );
        for value in [
            b"Bearer".as_slice(),
            b"Bearer ",
            b"Bearer a b",
            b"Bearer a\tb",
            b"Bearer a=b",
            b" Bearer a",
            b"Bearer a ",
            b"B@d a",
            b"Bearer \xff",
        ] {
            assert_eq!(
                bearer([value].into_iter()),
                Err(Reason::Malformed),
                "{value:?}"
            );
        }
        assert_eq!(
            bearer([b"Bearer a".as_slice(), b"Bearer b".as_slice()].into_iter()),
            Err(Reason::Malformed)
        );
    }

    #[test]
    fn outcomes_are_counted_by_closed_reason_and_the_clock_is_unavailable() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);
        let case = Case::new();
        // `authenticate` reads the system clock.
        let now = unix_now().unwrap();
        let token = case.signed(|_, claims| {
            claims["iat"] = now.into();
            claims["exp"] = (now + 60).into();
        });
        let header = format!("Bearer {token}");
        assert_eq!(
            case.verifier
                .authenticate([header.as_bytes()])
                .unwrap()
                .name(),
            "consultant"
        );
        assert_eq!(
            case.verifier.authenticate(std::iter::empty()),
            Err(Failure::Missing)
        );
        assert_eq!(
            case.verifier.authenticate([b"Bearer a b".as_slice()]),
            Err(Failure::Malformed)
        );
        assert_eq!(
            case.verifier.authenticate([b"Bearer a.b.c".as_slice()]),
            Err(Failure::Invalid)
        );
        assert_eq!(
            case.verifier
                .verify(token.as_bytes(), || Err(Reason::Clock)),
            Err(Reason::Clock)
        );
        assert_eq!(Reason::Clock.failure(), Failure::Unavailable);
        let rendered = handle.render();
        for line in [
            r#"service_auth_verifications_total{outcome="success",reason="verified"} 1"#,
            r#"service_auth_verifications_total{outcome="failure",reason="missing"} 1"#,
            r#"service_auth_verifications_total{outcome="failure",reason="malformed"} 1"#,
            r#"service_auth_verifications_total{outcome="failure",reason="header"} 1"#,
        ] {
            assert!(rendered.contains(line), "{line} in {rendered}");
        }
        assert!(!rendered.contains(&token), "{rendered}");
        assert_eq!(
            format!("{:?}", case.verifier),
            r#"Verifier { audience: "catalog", trusted_callers: 2, .. }"#
        );
    }

    /// A committed token, so another implementation of the contract can be
    /// checked against the same bytes: valid at `now`, refused for anything
    /// it was not issued for.
    #[test]
    fn the_committed_contract_vector_verifies_exactly() {
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/contract-vector.json")).unwrap();
        let text = |key: &str| vector[key].as_str().unwrap().to_owned();
        let now = vector["now"].as_u64().unwrap();
        let verifier = |receiver: &str, jwt_type: &str| {
            Verifier::new(
                CallerName::parse(receiver).unwrap(),
                JwtType::parse(jwt_type).unwrap(),
                TrustRegistry::parse(&vector["trust"].to_string()).unwrap(),
            )
        };
        let segments: Vec<&str> = vector["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment.as_str().unwrap())
            .collect();
        let token = segments.join(".");
        let check = |verifier: &Verifier, at: u64| {
            verifier
                .verify(token.as_bytes(), || Ok(at))
                .map(|caller| caller.name().to_owned())
        };
        let receiver = verifier(&text("receiver"), &text("jwt_type"));
        assert_eq!(check(&receiver, now), Ok(text("caller")));
        assert_eq!(check(&receiver, now + 61), Err(Reason::Expired));
        assert_eq!(
            check(&verifier("console", &text("jwt_type")), now),
            Err(Reason::Audience)
        );
        assert_eq!(
            check(&verifier(&text("receiver"), "other-svc+jwt"), now),
            Err(Reason::Type)
        );
    }
}
