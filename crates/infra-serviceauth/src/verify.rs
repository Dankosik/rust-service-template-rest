//! Service token verification: one header and one payload parse, then the
//! signature against the presenting caller's keys only.

use std::{
    borrow::Cow,
    fmt,
    sync::{Arc, OnceLock},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use enum_map::EnumMap;
use moka::sync::Cache;
use serde::{Deserialize, Deserializer, de};
use shared_rules::{
    rfc6750::{self, NoBearer},
    rfc7515,
};

use crate::{JwtType, MAX_TOKEN_LIFETIME_SECONDS, ServiceName, TrustRegistry, unix_seconds};

const LEEWAY_SECONDS: u64 = 30;

const MAX_REMEMBERED: u64 = 1024;

const VERIFICATIONS_METRIC: &str = "service_auth_verifications_total";

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
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

/// The verified calling service. Only verification constructs it, so
/// unverified request data can never pose as a caller.
#[derive(Clone, Eq, PartialEq, derive_more::Debug)]
#[debug("Caller({name:?})")]
pub struct Caller {
    name: ServiceName,
}

impl Caller {
    #[must_use]
    pub const fn name(&self) -> &ServiceName {
        &self.name
    }
}

#[derive(Clone)]
pub struct Verifier(Arc<Inner>);

struct Inner {
    audience: ServiceName,
    jwt_type: JwtType,
    trust: TrustRegistry,
    /// Registered once: a registry lookup per call costs more than the increment.
    verified: metrics::Counter,
    /// Registered on a reason's first refusal, so the exposition lists only
    /// reasons that happened.
    refused: EnumMap<Reason, OnceLock<metrics::Counter>>,
    /// Every check but the validity window depends only on the token's bytes
    /// and on the audience, type and registry fixed for the process, so a
    /// remembered token is decided by its window alone, as a full check would
    /// decide it. It is not a replay cache: nothing is refused for
    /// having been remembered. The cache bounds memory only; the window is
    /// checked here on every hit. Only a token whose signature verified is
    /// inserted, so a caller cannot choose colliding keys; `SipHash` over the
    /// whole token cost more than the rest of a hit.
    remembered: Cache<Box<[u8]>, (Caller, Window), foldhash::quality::RandomState>,
}

impl fmt::Debug for Verifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Verifier")
            .field("audience", &self.0.audience)
            .field("trusted_callers", &self.0.trust.caller_count())
            .finish_non_exhaustive()
    }
}

impl Verifier {
    /// A verifier for tokens addressed to `audience`, this service's name.
    /// The success counter is registered here, so a metrics recorder
    /// installed later counts failures but never successes; bootstrap
    /// installs it first.
    #[must_use]
    pub fn new(audience: ServiceName, jwt_type: JwtType, trust: TrustRegistry) -> Self {
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
            refused: EnumMap::default(),
            // A token first accepted one clock allowance before `iat` can
            // remain valid until one allowance after `exp`. The cache TTL
            // starts at insertion; every hit checks the token's own window.
            remembered: Cache::builder()
                .max_capacity(MAX_REMEMBERED)
                .time_to_live(std::time::Duration::from_secs(
                    MAX_TOKEN_LIFETIME_SECONDS + 2 * LEEWAY_SECONDS,
                ))
                .build_with_hasher(foldhash::quality::RandomState::default()),
        }))
    }

    pub fn callers(&self) -> impl Iterator<Item = &ServiceName> {
        self.0.trust.callers()
    }

    /// Records one outcome; a failure logs only its closed reason.
    pub fn authenticate<'a>(
        &self,
        authorization: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Caller, Failure> {
        let result = rfc6750::token(authorization)
            .map_err(|refusal| match refusal {
                NoBearer::Missing => Reason::Missing,
                NoBearer::Malformed => Reason::Malformed,
            })
            .and_then(|token| self.verify_at(token.as_bytes(), unix_now()?));
        match result {
            Ok(caller) => {
                self.0.verified.increment(1);
                Ok(caller)
            }
            Err(reason) => {
                self.0.refused[reason]
                    .get_or_init(|| {
                        metrics::counter!(VERIFICATIONS_METRIC, "outcome" => "failure", "reason" => reason.label())
                    })
                    .increment(1);
                tracing::debug!(reason = reason.label(), "service_auth_verification_failed");
                Err(reason.failure())
            }
        }
    }

    fn verify_at(&self, token: &[u8], now: u64) -> Result<Caller, Reason> {
        if let Some((caller, window)) = self.0.remembered.get(token) {
            return match window.admits(now) {
                Ok(()) => Ok(caller),
                Err(Reason::Expired) => {
                    self.0.remembered.invalidate(token);
                    Err(Reason::Expired)
                }
                // A clock stepped back before the window; it may step forward.
                Err(reason) => Err(reason),
            };
        }
        let (caller, window) = self.verify_uncached(token, now)?;
        self.0
            .remembered
            .insert(token.into(), (caller.clone(), window));
        Ok(caller)
    }

    #[cfg(test)]
    pub(crate) fn verify(
        &self,
        token: &[u8],
        now: impl FnOnce() -> Result<u64, Reason>,
    ) -> Result<Caller, Reason> {
        self.verify_uncached(token, now()?)
            .map(|(caller, _)| caller)
    }

    fn verify_uncached(&self, token: &[u8], now: u64) -> Result<(Caller, Window), Reason> {
        let rfc7515::Compact {
            signing_input: message,
            header,
            payload,
            signature,
        } = rfc7515::compact(token).ok_or(Reason::Header)?;
        let header = URL_SAFE_NO_PAD.decode(header).map_err(|_| Reason::Header)?;
        // RFC 7515 section 5.2 requires UTF-8 for the whole header and payload;
        // `serde_json::from_slice` skips an ignored member's bytes unchecked.
        let header = std::str::from_utf8(&header).map_err(|_| Reason::Header)?;
        let header: TokenHeader<'_> = serde_json::from_str(header).map_err(|_| Reason::Header)?;
        // The contract defines no extension, so `crit` is never understood.
        if header.crit {
            return Err(Reason::Header);
        }
        if header.alg != "ES256" {
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
        let issuer = claims.iss.as_deref().ok_or(Reason::Issuer)?;
        let (caller, keys) = self.0.trust.caller(issuer).ok_or(Reason::Issuer)?;
        let key = keys
            .iter()
            .find(|key| key.kid.as_str() == kid)
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
        let window = Window {
            valid_from: claims
                .nbf
                .map_or(issued_at, |not_before| not_before.max(issued_at)),
            expires_at,
        };
        window.admits(now)?;
        Ok((
            Caller {
                name: caller.clone(),
            },
            window,
        ))
    }
}

/// When a verified token is valid: from the later of its `iat` and `nbf`
/// to its `exp`, each widened by the clock allowance.
#[derive(Clone, Copy)]
struct Window {
    valid_from: u64,
    expires_at: u64,
}

impl Window {
    /// Expiry is checked first, so a token outside both bounds is expired.
    const fn admits(self, now: u64) -> Result<(), Reason> {
        if shared_rules::rfc7519::is_expired(self.expires_at, now, LEEWAY_SECONDS) {
            Err(Reason::Expired)
        } else if shared_rules::rfc7519::is_not_yet_valid(self.valid_from, now, LEEWAY_SECONDS) {
            Err(Reason::NotYetValid)
        } else {
            Ok(())
        }
    }
}

fn unix_now() -> Result<u64, Reason> {
    unix_seconds().ok_or(Reason::Clock)
}

/// The header members this crate reads; every other member is ignored, so no
/// member supplies or fetches a key (`jwk`, `jku`, `x5u` and `x5c` included),
/// and `kid` chooses only among the keys of the caller `iss` names. Repeated
/// recognized members cause a parse error. The `alg` member requires a
/// string. The `typ` and `kid` members also require strings when non-null
/// and read `null` as missing. Any value for `crit` counts as present.
/// `alg` is compared as the exact string, as RFC 7515 section 4.1.1 requires,
/// so any other string (`none` and unregistered names included) is an
/// `algorithm` failure.
#[derive(Deserialize)]
struct TokenHeader<'a> {
    #[serde(borrow)]
    alg: Cow<'a, str>,
    #[serde(borrow)]
    typ: Option<Cow<'a, str>>,
    #[serde(borrow)]
    kid: Option<Cow<'a, str>>,
    #[serde(default, deserialize_with = "present")]
    crit: bool,
}

/// The claims this crate reads; every other claim is ignored. Repeating a
/// recognized claim or giving it the wrong non-null type is a parse error.
/// A `null` value for `nbf` also causes a parse error. The other claims
/// read `null` as missing.
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
    #[serde(default, deserialize_with = "present_numeric_date")]
    nbf: Option<u64>,
}

/// `aud` as RFC 7519 allows it: one string or an array of strings. Only a
/// single audience can match, and `#[serde(untagged)]` would buffer into owned
/// storage instead of borrowing from the payload.
#[derive(Debug)]
enum Audience<'a> {
    Single(Cow<'a, str>),
    Other,
}

impl<'de: 'a, 'a> Deserialize<'de> for Audience<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<'a>(std::marker::PhantomData<Audience<'a>>);
        impl<'de: 'a, 'a> de::Visitor<'de> for Visitor<'a> {
            type Value = Audience<'a>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a string or an array of strings")
            }

            fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(Audience::Single(Cow::Borrowed(value)))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Audience::Single(Cow::Owned(value.to_owned())))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let Some(first) = seq.next_element::<BorrowableString<'de>>()? else {
                    return Ok(Audience::Other);
                };
                if seq.next_element::<BorrowableString<'de>>()?.is_none() {
                    return Ok(Audience::Single(first.0));
                }
                while seq.next_element::<BorrowableString<'de>>()?.is_some() {}
                Ok(Audience::Other)
            }
        }
        deserializer.deserialize_any(Visitor(std::marker::PhantomData))
    }
}

/// A bare [`Cow<str>`](Cow) element always deserializes as owned; `#[serde(borrow)]`
/// lets an escape-free string borrow from the payload.
#[derive(Deserialize)]
struct BorrowableString<'a>(#[serde(borrow)] Cow<'a, str>);

impl Audience<'_> {
    fn is_only(&self, name: &str) -> bool {
        matches!(self, Self::Single(audience) if audience == name)
    }
}

/// An RFC 7519 `NumericDate`; fraction rounding (via
/// [`shared_rules::rfc7519::numeric_date_seconds`]) moves a bound by at most
/// half a second, inside [`LEEWAY_SECONDS`].
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
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
                shared_rules::rfc7519::numeric_date_seconds(value)
                    .map(NumericDate)
                    .ok_or_else(|| E::custom("NumericDate out of range"))
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
    use std::assert_matches;

    const NOW: u64 = 1_800_000_000;
    const TYPE: &str = "example-svc+jwt";

    type Change = Box<dyn Fn(&mut serde_json::Value)>;

    struct Case {
        orders: TestKey,
        billing: TestKey,
        verifier: Verifier,
    }

    impl Case {
        fn new() -> Self {
            let orders = TestKey::new("orders/2026-10");
            let billing = TestKey::new("billing/2026-10");
            let trust = TrustRegistry::parse(&registry(&[
                ("orders", &[&orders]),
                ("billing", &[&billing]),
            ]))
            .unwrap();
            let verifier = Verifier::new(
                ServiceName::parse("catalog").unwrap(),
                JwtType::parse(TYPE).unwrap(),
                trust,
            );
            Self {
                orders,
                billing,
                verifier,
            }
        }

        fn header(key: &TestKey) -> serde_json::Value {
            serde_json::json!({"alg": "ES256", "typ": TYPE, "kid": key.kid()})
        }

        fn claims() -> serde_json::Value {
            serde_json::json!({
                "iss": "orders", "sub": "orders", "aud": "catalog",
                "iat": NOW, "exp": NOW + 60, "jti": "ignored",
            })
        }

        fn check(&self, token: &str) -> Result<String, Reason> {
            self.verifier
                .verify(token.as_bytes(), || Ok(NOW))
                .map(|caller| caller.name().as_str().to_owned())
        }

        fn signed(
            &self,
            change: impl FnOnce(&mut serde_json::Value, &mut serde_json::Value),
        ) -> String {
            let mut header = Self::header(&self.orders);
            let mut claims = Self::claims();
            change(&mut header, &mut claims);
            self.orders.sign(&header, &claims)
        }
    }

    #[test]
    fn a_verified_token_is_remembered_only_until_it_expires() {
        let case = Case::new();
        let token = case.signed(|_, _| {});
        let name =
            |result: Result<Caller, Reason>| result.map(|caller| caller.name().as_str().to_owned());
        assert_eq!(
            name(case.verifier.verify_at(token.as_bytes(), NOW)),
            Ok("orders".into())
        );
        assert_eq!(
            name(
                case.verifier
                    .verify_at(token.as_bytes(), NOW + 59 + LEEWAY_SECONDS)
            ),
            Ok("orders".into())
        );
        assert_eq!(
            name(
                case.verifier
                    .verify_at(token.as_bytes(), NOW + 60 + LEEWAY_SECONDS)
            ),
            Err(Reason::Expired)
        );
    }

    #[test]
    fn a_remembered_token_is_not_yet_valid_after_the_clock_steps_back() {
        let case = Case::new();
        let token = case.signed(|_, _| {});
        let earlier = NOW - LEEWAY_SECONDS - 1;
        assert!(case.verifier.verify_at(token.as_bytes(), NOW).is_ok());
        assert_eq!(
            case.verifier
                .verify_at(token.as_bytes(), earlier)
                .map(|_| ()),
            Err(Reason::NotYetValid)
        );
        assert_eq!(
            case.verifier
                .verify(token.as_bytes(), || Ok(earlier))
                .map(|_| ()),
            Err(Reason::NotYetValid)
        );
        assert!(case.verifier.verify_at(token.as_bytes(), NOW).is_ok());
    }

    #[test]
    fn a_refused_token_is_never_remembered_and_memory_stays_bounded() {
        let case = Case::new();
        let forged = case.signed(|_, claims| claims["aud"] = "other".into());
        for _ in 0..2 {
            assert_eq!(
                case.verifier.verify_at(forged.as_bytes(), NOW).map(|_| ()),
                Err(Reason::Audience)
            );
        }
        case.verifier.0.remembered.run_pending_tasks();
        assert_eq!(case.verifier.0.remembered.entry_count(), 0);
        for index in 0..=MAX_REMEMBERED {
            let token = case.signed(|_, claims| claims["jti"] = index.into());
            assert!(case.verifier.verify_at(token.as_bytes(), NOW).is_ok());
        }
        case.verifier.0.remembered.run_pending_tasks();
        assert!(case.verifier.0.remembered.entry_count() <= MAX_REMEMBERED);
    }

    #[test]
    fn a_contract_token_names_its_caller() {
        let case = Case::new();
        assert_eq!(case.check(&case.signed(|_, _| {})), Ok("orders".to_owned()));
        // The audience may be a one-element array, typ a full media type,
        // and an unknown claim or a fractional date is not a reason to refuse.
        let token = case.signed(|header, claims| {
            header["typ"] = "Application/Example-SVC+JWT".into();
            claims["aud"] = serde_json::json!(["catalog"]);
            claims["nbf"] = NOW.into();
            claims["exp"] = serde_json::json!(1_800_000_059.6_f64);
            claims["store"] = "anything".into();
        });
        assert_eq!(case.check(&token), Ok("orders".to_owned()));
        let billing = case.billing.sign(
            &Case::header(&case.billing),
            &serde_json::json!({"iss": "billing", "sub": "billing", "aud": "catalog", "iat": NOW, "exp": NOW + 30}),
        );
        assert_eq!(case.check(&billing), Ok("billing".to_owned()));
    }

    #[test]
    fn another_issuers_key_never_verifies_a_caller() {
        let case = Case::new();
        // billing's key and kid, claiming to be orders: the kid is looked
        // up among orders's keys only.
        let header = Case::header(&case.billing);
        let forged = case.billing.sign(&header, &Case::claims());
        assert_eq!(case.check(&forged), Err(Reason::UnknownKey));
        let forged = case
            .billing
            .sign(&Case::header(&case.orders), &Case::claims());
        assert_eq!(case.check(&forged), Err(Reason::Signature));
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
                Box::new(|claims| claims["aud"] = "inventory".into()),
                Reason::Audience,
            ),
            (
                Box::new(|claims| claims["aud"] = serde_json::json!(["catalog", "billing"])),
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
                Box::new(|claims| claims["sub"] = "billing".into()),
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
                    claims["iat"] = (NOW - 90).into();
                    claims["exp"] = (NOW - 30).into();
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
        let token = case.signed(|_, claims| {
            claims["iat"] = (NOW - 89).into();
            claims["exp"] = (NOW - 29).into();
        });
        assert_eq!(case.check(&token), Ok("orders".to_owned()));
        let token = case.signed(|_, claims| {
            claims["iat"] = (NOW + 30).into();
            claims["exp"] = (NOW + 90).into();
        });
        assert_eq!(case.check(&token), Ok("orders".to_owned()));
    }

    #[test]
    fn audience_borrows_strings_decodes_escapes_and_checks_trailing_array_types() {
        let single: Claims<'_> = serde_json::from_str(
            r#"{"iss":"orders","sub":"orders","aud":"catalog","iat":1800000000,"exp":1800000060}"#,
        )
        .unwrap();
        assert!(matches!(
            single.aud,
            Some(Audience::Single(Cow::Borrowed("catalog")))
        ));

        let singleton: Claims<'_> = serde_json::from_str(
            r#"{"iss":"orders","sub":"orders","aud":["catalog"],"iat":1800000000,"exp":1800000060}"#,
        )
        .unwrap();
        assert!(matches!(
            singleton.aud,
            Some(Audience::Single(Cow::Borrowed("catalog")))
        ));

        let escaped_payload = format!(
            r#"{{"iss":"orders","sub":"orders","aud":"cata\u006cog","iat":{NOW},"exp":{}}}"#,
            NOW + 60
        );
        let escaped_claims: Claims<'_> = serde_json::from_str(&escaped_payload).unwrap();
        assert!(matches!(
            escaped_claims.aud.as_ref(),
            Some(Audience::Single(Cow::Owned(value))) if value == "catalog"
        ));

        let case = Case::new();
        let escaped_token = case.orders.sign_raw(
            Case::header(&case.orders).to_string().as_bytes(),
            escaped_payload.as_bytes(),
        );
        assert_eq!(case.check(&escaped_token), Ok("orders".to_owned()));

        let empty_aud = case.signed(|_, claims| claims["aud"] = serde_json::json!([]));
        assert_eq!(case.check(&empty_aud), Err(Reason::Audience));

        let multi_aud =
            case.signed(|_, claims| claims["aud"] = serde_json::json!(["catalog", "other"]));
        assert_eq!(case.check(&multi_aud), Err(Reason::Audience));

        let mixed_aud =
            case.signed(|_, claims| claims["aud"] = serde_json::json!(["catalog", "extra", 123]));
        assert_eq!(case.check(&mixed_aud), Err(Reason::Claims));
    }

    #[test]
    fn the_header_selects_nothing_but_the_contract_algorithm_type_and_kid() {
        let case = Case::new();
        let cases: Vec<(Change, Reason)> = vec![
            (
                Box::new(|header| header["alg"] = "none".into()),
                Reason::Algorithm,
            ),
            (
                Box::new(|header| header["alg"] = "es256".into()),
                Reason::Algorithm,
            ),
            (Box::new(|header| header["alg"] = 7.into()), Reason::Header),
            (
                Box::new(|header| {
                    header.as_object_mut().unwrap().remove("alg");
                }),
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
        // A key embedded in the header is never used: billing's public key
        // offered for a token signed by billing and claiming orders.
        let mut header = Case::header(&case.orders);
        header["jwk"] = case.billing.jwk();
        let forged = case.billing.sign(&header, &Case::claims());
        assert_eq!(case.check(&forged), Err(Reason::Signature));
        let duplicate = case.orders.sign_raw(
            format!(
                r#"{{"alg":"ES256","typ":"{TYPE}","kid":"{}","kid":"other"}}"#,
                case.orders.kid()
            )
            .as_bytes(),
            Case::claims().to_string().as_bytes(),
        );
        assert_eq!(case.check(&duplicate), Err(Reason::Header));
        let duplicate = case.orders.sign_raw(
            Case::header(&case.orders).to_string().as_bytes(),
            format!(r#"{{"iss":"orders","sub":"orders","aud":"catalog","aud":"billing","iat":{NOW},"exp":{}}}"#, NOW + 60).as_bytes(),
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
            assert_matches!(
                case.check(malformed),
                Err(Reason::Header | Reason::Claims | Reason::Signature),
                "{malformed}"
            );
        }
        let not_utf8 = case.orders.sign_raw(
            Case::header(&case.orders).to_string().as_bytes(),
            b"{\"iss\":\"\xff\"}",
        );
        assert_eq!(case.check(&not_utf8), Err(Reason::Claims));
        // Invalid UTF-8 in a member the verifier ignores is still refused.
        let header = format!(
            r#"{{"alg":"ES256","typ":"{TYPE}","kid":"{}","x":""#,
            case.orders.kid()
        );
        let not_utf8 = case.orders.sign_raw(
            &[header.as_bytes(), b"\xff\"}"].concat(),
            Case::claims().to_string().as_bytes(),
        );
        assert_eq!(case.check(&not_utf8), Err(Reason::Header));
    }

    #[test]
    fn outcomes_are_counted_by_closed_reason_and_the_clock_is_unavailable() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let token = metrics::with_local_recorder(&recorder, || {
            let case = Case::new();
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
                    .name()
                    .as_str(),
                "orders"
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
            token
        });
        let snapshot = snapshotter.snapshot();
        assert!(!format!("{snapshot:?}").contains(&token), "{snapshot:?}");
        let mut counts: Vec<_> = snapshot
            .into_vec()
            .into_iter()
            .map(|(key, _, _, value)| {
                let mut labels: Vec<_> = key
                    .key()
                    .labels()
                    .map(|label| (label.key().to_owned(), label.value().to_owned()))
                    .collect();
                labels.sort();
                (key.key().name().to_owned(), labels, value)
            })
            .collect();
        counts.sort_by(|left, right| (left.0.as_str(), &left.1).cmp(&(right.0.as_str(), &right.1)));
        let counter = |outcome: &str, reason: &str| {
            (
                VERIFICATIONS_METRIC.to_owned(),
                vec![
                    ("outcome".to_owned(), outcome.to_owned()),
                    ("reason".to_owned(), reason.to_owned()),
                ],
                DebugValue::Counter(1),
            )
        };
        assert_eq!(
            counts,
            [
                counter("failure", "header"),
                counter("failure", "malformed"),
                counter("failure", "missing"),
                counter("success", "verified"),
            ]
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
                ServiceName::parse(receiver).unwrap(),
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
                .map(|caller| caller.name().as_str().to_owned())
        };
        let receiver = verifier(&text("receiver"), &text("jwt_type"));
        assert_eq!(check(&receiver, now), Ok(text("caller")));
        assert_eq!(check(&receiver, now + 61), Err(Reason::Expired));
        assert_eq!(
            check(&verifier("billing", &text("jwt_type")), now),
            Err(Reason::Audience)
        );
        assert_eq!(
            check(&verifier(&text("receiver"), "other-svc+jwt"), now),
            Err(Reason::Type)
        );
    }
}
