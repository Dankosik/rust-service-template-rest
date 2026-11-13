//! JWS verification against admitted keys.

use std::{
    sync::Arc,
    time::{Duration, Instant as MonotonicInstant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::Header;
use moka::{Expiry, sync::Cache};
use shared_rules::rfc7515;

use crate::{
    BearerToken, JwtAlgorithm, Principal, VerificationError, VerificationReason,
    claims::{ClaimPolicy, VerifiedJwt, validate_jwt_claims},
    jwks::MAX_SIGNATURE_BYTES,
    refresh::{KeyStore, REFRESH_INTERVAL, SignatureRefresh},
};

mod prepare;

pub use prepare::{JwtOptions, RefreshTask, prepare_jwt};

/// Target entry count after cache maintenance.
const MAX_REMEMBERED: u64 = 1024;
const MAX_REMEMBERED_FOR: Duration = REFRESH_INTERVAL;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenProfile {
    /// Accepts a token with no `typ` header, a generic JWT type, or an access-token type; `client_id` falls back to the `azp`, `appid`, and `cid` claims.
    ResourceServer,
    /// Requires an access-token `typ` header and the RFC 9068 claims `sub`, `client_id`, `jti`, and `iat`.
    Rfc9068,
}

/// A token whose signature a key set verified, and everything else its full
/// verification decided from the token bytes alone.
#[derive(Clone)]
struct Remembered {
    verified: VerifiedJwt,
    /// [`KeySet::generation`](crate::jwks::KeySet::generation) of the set that
    /// verified the signature.
    key_generation: u64,
    sampled_at: MonotonicInstant,
    held_for: Duration,
}

struct RememberedExpiry;

impl Expiry<Box<[u8]>, Remembered> for RememberedExpiry {
    fn expire_after_create(
        &self,
        _token: &Box<[u8]>,
        remembered: &Remembered,
        created_at: MonotonicInstant,
    ) -> Option<Duration> {
        // The wall-clock sample predates insertion. Spend that elapsed time
        // from the retention budget rather than starting it over at insertion.
        let elapsed = created_at.saturating_duration_since(remembered.sampled_at);
        Some(remembered.held_for.saturating_sub(elapsed))
    }
}

#[derive(derive_more::Debug)]
#[debug("JwtVerifier(..)")]
pub(crate) struct JwtVerifier {
    // Claim policy, token profile and admitted algorithms never change after
    // construction, so a verification depends on its token, the installed key
    // set (`key_store`) and the clock alone.
    claim_policy: ClaimPolicy,
    token_profile: TokenProfile,
    algorithms: Vec<JwtAlgorithm>,
    key_store: Arc<KeyStore>,
    /// Decides a token that already passed every check as a full verification
    /// would: the signature part by the key set's generation, the lifetime
    /// part by the clock, on every hit. Only a success is inserted, so a
    /// caller cannot choose what it holds. It is not a replay cache. The
    /// default `SipHash` over a whole token costs more than the rest of a hit.
    remembered: Cache<Box<[u8]>, Remembered, foldhash::quality::RandomState>,
}

impl JwtVerifier {
    fn new(
        claim_policy: ClaimPolicy,
        token_profile: TokenProfile,
        algorithms: Vec<JwtAlgorithm>,
        key_store: Arc<KeyStore>,
    ) -> Self {
        Self {
            claim_policy,
            token_profile,
            algorithms,
            key_store,
            remembered: Cache::builder()
                .max_capacity(MAX_REMEMBERED)
                .expire_after(RememberedExpiry)
                .build_with_hasher(foldhash::quality::RandomState::default()),
        }
    }
}

impl JwtVerifier {
    pub(crate) async fn verify(
        &self,
        token: &BearerToken<'_>,
    ) -> Result<Principal, VerificationError> {
        self.verify_with_now(token, crate::unix_now).await
    }

    async fn verify_with_now(
        &self,
        token: &BearerToken<'_>,
        now: impl Fn() -> Result<Duration, VerificationError>,
    ) -> Result<Principal, VerificationError> {
        let bytes = token.as_bytes();
        if let Some(remembered) = self.remembered.get(bytes) {
            // Verified against the set installed now, so the full path would
            // find the same signature valid on its first check.
            if remembered.key_generation == self.key_store.generation() {
                let verified = remembered.verified.deliver(now()?.as_secs());
                if matches!(&verified, Err(error) if error.reason == VerificationReason::Expired) {
                    self.remembered.invalidate(bytes);
                }
                return verified;
            }
            self.remembered.invalidate(bytes);
        }
        let remembered = self.verify_uncached(token, &now).await?;
        let principal = remembered.verified.principal.clone();
        self.remembered.insert(bytes.into(), remembered);
        Ok(principal)
    }

    async fn verify_uncached(
        &self,
        token: &BearerToken<'_>,
        now: &impl Fn() -> Result<Duration, VerificationError>,
    ) -> Result<Remembered, VerificationError> {
        let rfc7515::Compact {
            signing_input: message,
            header,
            payload,
            signature,
        } = rfc7515::compact(token.as_bytes())
            .ok_or_else(|| VerificationError::invalid(VerificationReason::Header))?;
        let (header, algorithm) = self.admit_header(header)?;
        // Decode into bounded stack storage; admitted keys cap the signature size.
        // Keep `message` as the original encoded bytes that the issuer signed.
        let mut signature_bytes = [0_u8; MAX_SIGNATURE_BYTES];
        let signature = URL_SAFE_NO_PAD
            .decode_slice(signature, &mut signature_bytes)
            .map(|length| &signature_bytes[..length])
            .map_err(|_| VerificationError::invalid(VerificationReason::Signature))?;
        let key_generation = self
            .verify_signature(message, signature, header.kid.as_deref(), algorithm)
            .await?;
        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| VerificationError::invalid(VerificationReason::MalformedClaims))?;
        let sampled_at = MonotonicInstant::now();
        let now = now()?;
        let verified = validate_jwt_claims(
            payload,
            &self.claim_policy,
            self.token_profile,
            now.as_secs(),
            token.to_secret(),
        )?;
        Ok(Remembered {
            held_for: MAX_REMEMBERED_FOR
                .min(Duration::from_secs(verified.principal.expires_at()).saturating_sub(now)),
            sampled_at,
            verified,
            key_generation,
        })
    }

    fn admit_header(&self, header: &[u8]) -> Result<(Header, JwtAlgorithm), VerificationError> {
        let malformed = || VerificationError::invalid(VerificationReason::Header);
        let header = URL_SAFE_NO_PAD.decode(header).map_err(|_| malformed())?;
        // jsonwebtoken's `Header` keeps the library's header admission rules.
        let header: Header = serde_json::from_slice(&header).map_err(|_| malformed())?;
        // RFC 7515 section 4.1.11: this verifier understands no critical extension.
        if header.crit.as_ref().is_some_and(|crit| !crit.is_empty()) {
            return Err(malformed());
        }
        let algorithm = JwtAlgorithm::from_jsonwebtoken(header.alg)
            .filter(|algorithm| self.algorithms.contains(algorithm))
            .ok_or_else(|| VerificationError::invalid(VerificationReason::Algorithm))?;
        let typ = header.typ.as_deref();
        let admitted_type = match self.token_profile {
            TokenProfile::Rfc9068 => typ.is_some_and(is_access_token_type),
            // An untyped or generic JWT may be an access token. One explicitly
            // typed as another kind of JWT is not (RFC 8725 section 3.11).
            TokenProfile::ResourceServer => {
                typ.is_none_or(|typ| is_generic_jwt_type(typ) || is_access_token_type(typ))
            }
        };
        if !admitted_type {
            return Err(VerificationError::invalid(VerificationReason::Profile));
        }
        Ok((header, algorithm))
    }

    /// A provider can replace a key under the same `kid`, so any signature
    /// miss can need a newer set. The store limits refresh work across callers.
    async fn verify_signature(
        &self,
        message: &[u8],
        signature: &[u8],
        kid: Option<&str>,
        algorithm: JwtAlgorithm,
    ) -> Result<u64, VerificationError> {
        let checked = self.key_store.current();
        let miss = match checked.verify(message, signature, kid, algorithm) {
            Ok(()) => return Ok(checked.generation()),
            Err(miss) => miss,
        };
        match self.key_store.refresh_for_signature_miss(&checked).await {
            SignatureRefresh::NewerSet(key_set) => {
                key_set.verify(message, signature, kid, algorithm)?;
                Ok(key_set.generation())
            }
            SignatureRefresh::NoNewerSet => Err(miss.into()),
            SignatureRefresh::Unavailable => {
                Err(VerificationError::unavailable(VerificationReason::Refresh))
            }
        }
    }
}

fn is_access_token_type(value: &str) -> bool {
    rfc7515::typ_is(value, "at+jwt")
}

fn is_generic_jwt_type(value: &str) -> bool {
    rfc7515::typ_is(value, "jwt")
}

#[cfg(test)]
mod tests {
    use std::assert_matches;
    use std::sync::Arc;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode, jwk::Jwk};

    use std::time::Duration;

    use super::{ClaimPolicy, JwtAlgorithm, JwtVerifier, KeyStore, MAX_REMEMBERED, TokenProfile};
    use crate::{
        Failure, Transport, VerificationReason,
        jwks::{
            KeySet,
            tests::{JWT_SIGNING_DER, ec_signing, jwk, key_set, rsa_signing},
        },
        parse_bearer,
        refresh::SignatureRefresh,
        test_metrics::Diagnostics,
    };

    fn rsa_key_set(kid: &str, algorithm: Option<Algorithm>) -> Arc<KeySet> {
        let key = jwk(
            &rsa_signing(),
            algorithm.unwrap_or(Algorithm::RS256),
            Some(kid),
            algorithm.is_some(),
        );
        key_set(&[key], &[JwtAlgorithm::Rs256])
    }

    fn verifier(keys: Arc<KeySet>, algorithms: &[JwtAlgorithm]) -> JwtVerifier {
        let claim_policy =
            ClaimPolicy::try_new("https://issuer.example".to_owned(), vec!["api".to_owned()])
                .unwrap();
        JwtVerifier::new(
            claim_policy,
            TokenProfile::ResourceServer,
            algorithms.to_vec(),
            KeyStore::new(keys),
        )
    }

    fn signed(
        signing: &EncodingKey,
        algorithm: Algorithm,
        kid: Option<&str>,
        extra: &serde_json::Value,
    ) -> String {
        let mut claims = serde_json::json!({
            "iss": "https://issuer.example", "aud": "api",
            "exp": jsonwebtoken::get_current_timestamp() + 60, "sub": "subject",
        });
        claims
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut header = Header::new(algorithm);
        header.kid = kid.map(ToOwned::to_owned);
        encode(&header, &claims, signing).unwrap()
    }

    async fn check(
        verifier: &JwtVerifier,
        token: &str,
    ) -> Result<crate::Principal, VerificationReason> {
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        verifier.verify(&token).await.map_err(|error| error.reason)
    }

    #[tokio::test]
    async fn unavailable_clock_refuses_even_extreme_expiry_and_recovers() {
        let verifier = verifier(
            rsa_key_set("fixture", Some(Algorithm::RS256)),
            &[JwtAlgorithm::Rs256],
        );
        let signed = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({"exp": u64::MAX}),
        );
        let header = format!("Bearer {signed}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        let unavailable =
            || crate::unix_time(std::time::UNIX_EPOCH - std::time::Duration::from_secs(1));
        let error = verifier
            .verify_with_now(&token, unavailable)
            .await
            .unwrap_err();
        assert_eq!(error.failure, Failure::Unavailable);
        assert_eq!(error.reason, VerificationReason::Clock);
        assert_eq!(error.reason.label(), "clock");
        assert!(
            verifier
                .verify_with_now(&token, || Ok(Duration::from_secs(100)))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_jwk_without_alg_serves_every_configured_algorithm_of_its_family() {
        let both = [JwtAlgorithm::Rs256, JwtAlgorithm::Ps256];
        let keys = key_set(
            &[jwk(&rsa_signing(), Algorithm::RS256, Some("entra"), false)],
            &both,
        );
        let verifier = verifier(keys, &both);
        for algorithm in [Algorithm::RS256, Algorithm::PS256] {
            let token = signed(
                &rsa_signing(),
                algorithm,
                Some("entra"),
                &serde_json::json!({}),
            );
            check(&verifier, &token).await.unwrap();
        }
    }

    #[tokio::test]
    async fn an_explicit_jwk_alg_binds_the_key() {
        let both = [JwtAlgorithm::Rs256, JwtAlgorithm::Ps256];
        let keys = key_set(
            &[jwk(&rsa_signing(), Algorithm::RS256, Some("bound"), true)],
            &both,
        );
        let verifier = verifier(keys, &both);
        let token = signed(
            &rsa_signing(),
            Algorithm::PS256,
            Some("bound"),
            &serde_json::json!({}),
        );
        // The only key refuses PS256; the refresh answer is "still unknown".
        assert_eq!(
            check(&verifier, &token).await.unwrap_err(),
            VerificationReason::UnknownKey
        );
    }

    #[tokio::test]
    async fn kid_less_keys_are_tried_in_turn() {
        let (old, new) = (ec_signing(), ec_signing());
        let keys = key_set(
            &[
                jwk(&old, Algorithm::ES256, None, true),
                jwk(&new, Algorithm::ES256, None, true),
            ],
            &[JwtAlgorithm::Es256],
        );
        let verifier = verifier(keys, &[JwtAlgorithm::Es256]);
        for signing in [&old, &new] {
            let token = signed(signing, Algorithm::ES256, None, &serde_json::json!({}));
            check(&verifier, &token).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_kid_less_signature_miss_waits_for_one_refresh() {
        let (installed, rotated) = (ec_signing(), ec_signing());
        let verifier = Arc::new(verifier(
            key_set(
                &[jwk(&installed, Algorithm::ES256, None, true)],
                &[JwtAlgorithm::Es256],
            ),
            &[JwtAlgorithm::Es256],
        ));
        verifier.key_store.permit_unknown_refresh_for_test();
        let token = signed(&rotated, Algorithm::ES256, None, &serde_json::json!({}));
        let call = tokio::spawn({
            let verifier = Arc::clone(&verifier);
            async move { check(&verifier, &token).await.map(|_| ()) }
        });
        tokio::task::yield_now().await;
        assert_eq!(verifier.key_store.pending(), Some(1));
        let rotated_set = key_set(
            &[jwk(&rotated, Algorithm::ES256, None, true)],
            &[JwtAlgorithm::Es256],
        );
        verifier.key_store.finish(1, Ok(rotated_set));
        assert_eq!(call.await.unwrap(), Ok(()));
        // Within the cooldown a second miss keeps the precise reason.
        let stale = signed(&installed, Algorithm::ES256, None, &serde_json::json!({}));
        assert_eq!(
            check(&verifier, &stale).await.unwrap_err(),
            VerificationReason::Signature
        );
        assert_matches!(
            verifier
                .key_store
                .refresh_for_signature_miss(&verifier.key_store.current())
                .await,
            SignatureRefresh::NoNewerSet
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_expired_unknown_key_waiter_leaves_refresh_available_to_a_live_caller() {
        use std::{
            future::{Future, poll_fn},
            task::Poll,
            time::Duration,
        };

        use operation_context::OperationContext;

        let engine = verifier(rsa_key_set("old", None), &[JwtAlgorithm::Rs256]);
        engine.key_store.permit_unknown_refresh_for_test();
        let keys = Arc::clone(&engine.key_store);
        let verifier = crate::Verifier::jwt(engine);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("rotated"),
            &serde_json::json!({}),
        );
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        let short = OperationContext::with_timeout(Duration::from_millis(10));
        let live = OperationContext::with_timeout(Duration::from_secs(1));
        let mut first = Box::pin(verifier.verify_with_context(&token, &short));
        let mut survivor = Box::pin(verifier.verify_with_context(&token, &live));
        assert!(
            poll_fn(|cx| Poll::Ready(first.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(
            poll_fn(|cx| Poll::Ready(survivor.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        tokio::time::advance(Duration::from_millis(11)).await;
        assert_eq!(first.await, Err(Failure::Unavailable));
        assert_eq!(keys.pending(), Some(1));
        keys.finish(1, Ok(rsa_key_set("rotated", None)));
        let principal = tokio::time::timeout(Duration::from_secs(1), survivor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.subject(), Some("subject"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_signing_key_rotated_under_the_same_kid_is_picked_up_on_demand() {
        use std::{future::Future, task::Poll};

        let (installed, rotated) = (ec_signing(), ec_signing());
        let verifier = ec_verifier(&installed, "stable");
        verifier.key_store.permit_unknown_refresh_for_test();
        let token = ec_token(&rotated, "stable", &serde_json::json!({}));
        let mut call = std::pin::pin!(verify_at(&verifier, &token, 1_000));
        std::future::poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let ticket = verifier.key_store.pending().unwrap();
        let replacement = key_set(
            &[jwk(&rotated, Algorithm::ES256, Some("stable"), true)],
            &[JwtAlgorithm::Es256],
        );
        verifier.key_store.finish(ticket, Ok(replacement));
        assert_eq!(call.await.unwrap().subject(), Some("subject"));

        let stale = ec_token(&installed, "stable", &serde_json::json!({}));
        assert_eq!(
            verify_at(&verifier, &stale, 1_000).await,
            Err(VerificationReason::Signature)
        );
    }

    #[tokio::test]
    async fn registered_claim_nulls_keep_jsonwebtoken_rules_and_null_identity_is_absent() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        for (extra, expected) in [
            (
                serde_json::json!({"nbf": null}),
                Err(VerificationReason::MalformedClaims),
            ),
            (
                serde_json::json!({"exp": null}),
                Err(VerificationReason::MissingClaim),
            ),
            (
                serde_json::json!({"sub": null, "azp": "client"}),
                Ok(Some("client")),
            ),
        ] {
            let token = signed(&rsa_signing(), Algorithm::RS256, Some("fixture"), &extra);
            let result = check(&verifier, &token).await;
            let client_id = result.as_ref().map(crate::Principal::client_id);
            assert_eq!(client_id.map_err(|error| *error), expected, "{extra}");
        }
    }

    #[tokio::test]
    async fn the_compact_form_and_header_are_checked_before_any_key() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let (message, signature) = token.rsplit_once('.').unwrap();
        let (_, payload) = message.split_once('.').unwrap();
        let header = |json: &str| URL_SAFE_NO_PAD.encode(json);
        for (token, expected) in [
            (format!("{token}.x"), VerificationReason::Header),
            (message.to_owned(), VerificationReason::Header),
            (
                format!(
                    "{}.{payload}.{signature}",
                    header(r#"{"alg":"RS256","crit":["exp"]}"#)
                ),
                VerificationReason::Header,
            ),
            (
                format!("{}.{payload}.{signature}", header(r#"{"alg":"none"}"#)),
                VerificationReason::Header,
            ),
            // jsonwebtoken's header rules also reject malformed unused members.
            (
                format!(
                    "{}.{payload}.{signature}",
                    header(r#"{"alg":"RS256","kid":"fixture","x5c":5}"#)
                ),
                VerificationReason::Header,
            ),
            (
                format!("{message}.{signature}~"),
                VerificationReason::Signature,
            ),
            (
                format!("{message}.{}", URL_SAFE_NO_PAD.encode([7_u8; 1025])),
                VerificationReason::Signature,
            ),
        ] {
            assert_eq!(
                check(&verifier, &token).await.unwrap_err(),
                expected,
                "{token}"
            );
        }
    }

    #[tokio::test]
    async fn a_token_explicitly_typed_as_another_kind_of_jwt_is_not_an_access_token() {
        let resource_server = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let rfc9068 = JwtVerifier {
            token_profile: TokenProfile::Rfc9068,
            ..verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256])
        };
        let typed = |typ: Option<&str>| {
            let mut header = Header::new(Algorithm::RS256);
            header.kid = Some("fixture".to_owned());
            header.typ = typ.map(ToOwned::to_owned);
            let claims = serde_json::json!({
                "iss": "https://issuer.example", "aud": "api",
                "exp": jsonwebtoken::get_current_timestamp() + 60,
                "iat": jsonwebtoken::get_current_timestamp(),
                "sub": "subject", "client_id": "client", "jti": "id",
            });
            encode(&header, &claims, &rsa_signing()).unwrap()
        };
        for (typ, generic) in [
            (None, true),
            (Some("JWT"), true),
            (Some("jwt"), true),
            (Some("application/jwt"), true),
            (Some("at+jwt"), false),
            (Some("application/at+jwt"), false),
        ] {
            let token = typed(typ);
            assert!(check(&resource_server, &token).await.is_ok(), "{typ:?}");
            assert_eq!(
                check(&rfc9068, &token).await.err(),
                generic.then_some(VerificationReason::Profile),
                "{typ:?}"
            );
        }
        for typ in [
            "client-authentication+jwt",
            "logout+jwt",
            "dpop+jwt",
            "secevent+jwt",
            "",
        ] {
            let token = typed(Some(typ));
            for verifier in [&resource_server, &rfc9068] {
                assert_eq!(
                    check(verifier, &token).await.unwrap_err(),
                    VerificationReason::Profile,
                    "{typ}"
                );
            }
        }
    }

    #[tokio::test]
    async fn signed_payload_supplies_typed_custom_claims_without_changing_identity() {
        #[derive(serde::Deserialize)]
        struct ApplicationClaims {
            tenant: String,
            permissions: Vec<String>,
        }
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({"tenant": "verified-tenant", "permissions": ["read", "write"]}),
        );
        let principal = check(&verifier, &token).await.unwrap();
        let claims = principal.claims::<ApplicationClaims>().unwrap();
        assert_eq!(claims.tenant, "verified-tenant");
        assert_eq!(claims.permissions, ["read", "write"]);
        assert_eq!(principal.subject(), Some("subject"));
        let error = principal.claims::<Vec<String>>().unwrap_err();
        for rendered in [
            error.to_string(),
            format!("{error:?}"),
            format!("{principal:?}"),
        ] {
            assert!(!rendered.contains("verified-tenant"));
            assert!(!rendered.contains("permissions"));
        }
    }

    #[tokio::test]
    async fn access_token_equals_the_presented_bearer_text() {
        use secrecy::ExposeSecret;
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let principal = check(&verifier, &token).await.unwrap();
        assert_eq!(principal.access_token().expose_secret(), token);
    }

    #[tokio::test]
    async fn a_leading_zero_modulus_still_verifies() {
        let mut padded = jwk(&rsa_signing(), Algorithm::RS256, Some("padded"), true);
        let modulus = URL_SAFE_NO_PAD
            .decode(padded["n"].as_str().unwrap())
            .unwrap();
        padded["n"] = URL_SAFE_NO_PAD
            .encode([[0].as_slice(), &modulus].concat())
            .into();
        let verifier = verifier(
            key_set(&[padded], &[JwtAlgorithm::Rs256]),
            &[JwtAlgorithm::Rs256],
        );
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("padded"),
            &serde_json::json!({}),
        );
        check(&verifier, &token).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn unconfigured_algorithm_does_not_wait_for_refresh() {
        let verifier = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        verifier.key_store.permit_unknown_refresh_for_test();
        let token = signed(
            &rsa_signing(),
            Algorithm::PS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        assert_eq!(
            check(&verifier, &token).await.unwrap_err(),
            VerificationReason::Algorithm
        );
        assert_eq!(verifier.key_store.pending(), None);
    }

    /// Signs arbitrary header and payload bytes, which `jsonwebtoken::encode`
    /// cannot produce.
    fn sign_raw(header: &str, payload: &[u8]) -> String {
        use aws_lc_rs::{rand::SystemRandom, signature};
        let key = signature::RsaKeyPair::from_der(JWT_SIGNING_DER).unwrap();
        let message = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(payload)
        );
        let mut signed = vec![0; key.public_modulus_len()];
        key.sign(
            &signature::RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            message.as_bytes(),
            &mut signed,
        )
        .unwrap();
        format!("{message}.{}", URL_SAFE_NO_PAD.encode(signed))
    }

    const CORPUS_ISSUER: &str = "https://issuer.example";
    const CORPUS_HEADER: &str = r#"{"alg":"RS256","kid":"fixture"}"#;

    /// Headers the library reads without a key set or a token type rule.
    const CORPUS_HEADERS: [&str; 27] = [
        r#"{"alg":"RS256"}"#,
        r#"{"alg":"RS256","kid":"unknown"}"#,
        r#"{"alg":"RS256","kid":5}"#,
        r#"{"alg":"RS256","kid":null}"#,
        r#"{"alg":"none","kid":"fixture"}"#,
        r#"{"alg":"HS256","kid":"fixture"}"#,
        r#"{"alg":"ES256","kid":"fixture"}"#,
        r#"{"alg":"PS256","kid":"fixture"}"#,
        r#"{"alg":"rs256","kid":"fixture"}"#,
        r#"{"kid":"fixture"}"#,
        r#"{"alg":"RS256","kid":"fixture","crit":[]}"#,
        r#"{"alg":"RS256","kid":"fixture","crit":["exp"]}"#,
        r#"{"alg":"RS256","kid":"fixture","crit":"exp"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":"JWT"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":"at+jwt"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":"dpop+jwt"}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":1}"#,
        r#"{"alg":"RS256","kid":"fixture","typ":null}"#,
        r#"{"alg":"RS256","kid":"fixture","x5c":5}"#,
        r#"{"alg":"RS256","kid":"fixture","jwk":"x"}"#,
        r#"{"alg":"RS256","kid":"fixture","cty":"JWT"}"#,
        r#"{"alg":"RS256","kid":"fixture","zzz":{"a":[1]}}"#,
        r#"{"alg":"RS256","alg":"RS256","kid":"fixture"}"#,
        r#"{"alg":"RS256","kid":"fixture","kid":"fixture"}"#,
        "[1]",
        "",
        "{",
    ];

    /// Every registered claim in every shape, and pairs that fail two checks.
    fn claim_cases(base: &serde_json::Value, now: u64) -> Vec<(String, String)> {
        use serde_json::{Value, json};
        const ISSUER: &str = CORPUS_ISSUER;
        const HEADER: &str = CORPUS_HEADER;
        #[expect(
            clippy::cast_precision_loss,
            reason = "current epoch seconds fit an f64"
        )]
        let shapes: Vec<Option<Value>> = vec![
            None,
            Some(Value::Null),
            Some("".into()),
            Some("api".into()),
            Some("other".into()),
            Some(ISSUER.into()),
            Some("a\"pi\\u0041".into()),
            Some(42.into()),
            Some(json!(1.5)),
            Some(json!(-5)),
            Some(json!(u64::MAX)),
            Some(json!(1e30)),
            Some((now - 100).into()),
            Some((now - 25).into()),
            Some((now + 25).into()),
            Some((now + 100).into()),
            Some(json!(now as f64 + 100.4)),
            Some(json!(now as f64 - 1000.6)),
            Some(json!(["api"])),
            Some(json!([ISSUER])),
            Some(json!(["x", "y"])),
            Some(json!(["x", "api"])),
            Some(json!(["x", ISSUER])),
            Some(json!([])),
            Some(json!([1])),
            Some(json!([null])),
            Some(json!({"a": 1})),
            Some(true.into()),
        ];
        let mut exact = Vec::new();
        for claim in ["iss", "aud", "exp", "nbf"] {
            for shape in &shapes {
                let mut claims = base.clone();
                match shape {
                    None => drop(claims.as_object_mut().unwrap().remove(claim)),
                    Some(value) => claims[claim] = value.clone(),
                }
                exact.push((
                    format!("{claim}={shape:?}"),
                    sign_raw(HEADER, claims.to_string().as_bytes()),
                ));
            }
        }
        for (first, first_value, second, second_value) in [
            ("iss", json!("x"), "exp", json!(now - 100)),
            ("aud", json!("x"), "nbf", json!(now + 100)),
            ("iss", json!("x"), "aud", json!("x")),
            ("exp", json!(now - 100), "nbf", json!(now + 100)),
        ] {
            let mut claims = base.clone();
            claims[first] = first_value;
            claims[second] = second_value;
            exact.push((
                format!("{first} and {second}"),
                sign_raw(HEADER, claims.to_string().as_bytes()),
            ));
        }
        exact
    }

    /// Payloads that are not one plain claims object, and broken compact forms.
    fn payload_and_compact_cases(body: &str) -> Vec<(String, String)> {
        const HEADER: &str = CORPUS_HEADER;
        let mut exact = Vec::new();
        let members = &body[1..body.len() - 1];
        for (name, payload) in [
            ("duplicate iss", format!(r#"{{{members},"iss":"x"}}"#)),
            ("duplicate aud", format!(r#"{{{members},"aud":"api"}}"#)),
            ("duplicate exp", format!(r#"{{{members},"exp":1}}"#)),
            ("duplicate nbf", format!(r#"{{{members},"nbf":1,"nbf":2}}"#)),
            ("duplicate custom", format!(r#"{{{members},"t":1,"t":2}}"#)),
            ("trailing space", format!("{body} ")),
            ("leading space", format!(" {body}")),
            ("array", "[1]".to_owned()),
            ("string", "\"x\"".to_owned()),
            ("two documents", format!("{body}{body}")),
            (
                "deep nesting",
                format!(
                    r#"{{{members},"d":{}0{}}}"#,
                    "[".repeat(200),
                    "]".repeat(200)
                ),
            ),
        ] {
            exact.push((name.to_owned(), sign_raw(HEADER, payload.as_bytes())));
        }
        exact.push((
            "payload is not UTF-8".to_owned(),
            sign_raw(HEADER, &[b'{', b'"', 0xff, b'"', b':', b'1', b'}']),
        ));
        let good = sign_raw(HEADER, body.as_bytes());
        let (message, signature) = good.rsplit_once('.').unwrap();
        let (header, payload) = message.split_once('.').unwrap();
        for (name, token) in [
            ("valid", good.clone()),
            ("four parts", format!("{good}.x")),
            ("two parts", message.to_owned()),
            ("empty signature", format!("{message}.")),
            ("padded signature", format!("{message}.{signature}=")),
            (
                "truncated signature",
                format!("{message}.{}", &signature[..signature.len() - 4]),
            ),
            (
                "long signature",
                format!("{message}.{}", URL_SAFE_NO_PAD.encode([7_u8; 1500])),
            ),
            (
                "signature outside the alphabet",
                format!("{message}.{}~", &signature[..signature.len() - 1]),
            ),
            (
                "another token's signature",
                format!(
                    "{message}.{}",
                    sign_raw(HEADER, b"{}").rsplit_once('.').unwrap().1
                ),
            ),
            (
                "payload outside the alphabet",
                format!("{header}.a~b.{signature}"),
            ),
            ("empty header", format!(".{payload}.{signature}")),
            ("padded header", format!("{header}=.{payload}.{signature}")),
        ] {
            exact.push((name.to_owned(), token));
        }
        exact
    }

    #[derive(serde::Deserialize)]
    struct StringIssuer {
        #[serde(rename = "iss")]
        _issuer: String,
    }

    /// This crate and `jsonwebtoken::decode`, deciding the same token under
    /// the same issuer, audience, leeway and key. The oracle also requires a
    /// string issuer because the library permits issuer arrays.
    struct Deciders {
        verifier: JwtVerifier,
        key: jsonwebtoken::DecodingKey,
        validation: jsonwebtoken::Validation,
    }

    impl Deciders {
        fn new() -> Self {
            let key = serde_json::from_value::<Jwk>(jwk(
                &rsa_signing(),
                Algorithm::RS256,
                Some("fixture"),
                true,
            ))
            .unwrap();
            let mut validation = jsonwebtoken::Validation::new(Algorithm::RS256);
            validation.leeway = 30;
            validation.validate_nbf = true;
            validation.set_issuer(&[CORPUS_ISSUER]);
            validation.set_audience(&["api"]);
            validation.set_required_spec_claims(&["exp", "iss", "aud"]);
            Self {
                verifier: verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]),
                key: jsonwebtoken::DecodingKey::from_jwk(&key).unwrap(),
                validation,
            }
        }

        fn library(&self, token: &str) -> bool {
            jsonwebtoken::decode::<StringIssuer>(token, &self.key, &self.validation).is_ok()
        }

        /// A token outside the bearer grammar never reaches the verifier. The
        /// second verification of an accepted token is a memo hit and must
        /// decide as the first did.
        async fn here(&self, token: &str) -> bool {
            let header = format!("Bearer {token}");
            match parse_bearer([header.as_bytes()]) {
                Ok(token) => {
                    let first = self.verifier.verify(&token).await;
                    let second = self.verifier.verify(&token).await;
                    assert_eq!(first.is_ok(), second.is_ok());
                    first.is_ok()
                }
                Err(_) => false,
            }
        }
    }

    /// Compare the registered claims with the library under the same issuer
    /// shape rule. No header that the library refuses may pass here.
    #[tokio::test]
    async fn registered_claim_decisions_match_the_reference_validation() {
        let deciders = Deciders::new();
        let now = jsonwebtoken::get_current_timestamp();
        let base = serde_json::json!({
            "iss": CORPUS_ISSUER, "aud": "api", "exp": now + 600, "sub": "subject",
        });
        let body = base.to_string();
        let mut exact = claim_cases(&base, now);
        exact.extend(payload_and_compact_cases(&body));
        let mut differences = Vec::new();
        for (name, token) in &exact {
            let (here, there) = (deciders.here(token).await, deciders.library(token));
            if here != there {
                differences.push(format!("{name}: {here} here, {there} in jsonwebtoken"));
            }
        }
        // This crate adds key selection and a token type rule, so it may
        // refuse more headers than the library, never fewer.
        for header in CORPUS_HEADERS {
            let token = sign_raw(header, body.as_bytes());
            if deciders.here(&token).await && !deciders.library(&token) {
                differences.push(format!("{header}: accepted here, refused by jsonwebtoken"));
            }
        }
        assert!(differences.is_empty(), "{differences:#?}");
        // The corpus exercises both decisions, not one constant answer.
        let accepted = exact
            .iter()
            .filter(|(_, token)| deciders.library(token))
            .count();
        assert!(accepted > 0 && accepted < exact.len());
    }

    /// The one deliberate difference in reading a payload. RFC 7519 section
    /// 7.2 requires a UTF-8 payload and this crate keeps it as text; the
    /// library does not validate the text of a member it skips.
    #[tokio::test]
    async fn a_payload_that_is_not_utf8_is_refused_where_jsonwebtoken_skips_it() {
        let deciders = Deciders::new();
        let exp = jsonwebtoken::get_current_timestamp() + 600;
        let mut payload =
            format!(r#"{{"sub":"subject","iss":"{CORPUS_ISSUER}","aud":"api","exp":{exp},"t":""#)
                .into_bytes();
        payload.extend_from_slice(b"\xff\"}");
        let token = sign_raw(CORPUS_HEADER, &payload);
        assert!(deciders.library(&token));
        assert_eq!(
            check(&deciders.verifier, &token).await.unwrap_err(),
            VerificationReason::MalformedClaims
        );
    }

    #[derive(Clone, Debug)]
    enum Member {
        Absent,
        Json(serde_json::Value),
        /// Seconds from the moment the case runs, outside the leeway's edges
        /// so that the two deciders' clock readings cannot disagree.
        Time {
            offset: i64,
            fractional: bool,
        },
    }

    impl Member {
        fn put(&self, claims: &mut serde_json::Value, name: &str, now: u64) {
            #[expect(clippy::cast_precision_loss, reason = "epoch seconds fit an f64")]
            let value = match self {
                Self::Absent => return,
                Self::Json(value) => value.clone(),
                Self::Time { offset, fractional } => {
                    let seconds = now.saturating_add_signed(*offset);
                    if *fractional {
                        serde_json::json!(seconds as f64 + 0.4)
                    } else {
                        seconds.into()
                    }
                }
            };
            claims[name] = value;
        }
    }

    fn any_json() -> impl proptest::strategy::Strategy<Value = serde_json::Value> {
        use proptest::prelude::*;
        use serde_json::Value;
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            any::<f64>().prop_map(Value::from),
            prop_oneof![
                Just("api".to_owned()),
                Just(CORPUS_ISSUER.to_owned()),
                any::<String>()
            ]
            .prop_map(Value::from),
        ];
        leaf.prop_recursive(2, 8, 3, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..3).prop_map(Value::from),
                inner.prop_map(|value| serde_json::json!({ "a": value })),
            ]
        })
    }

    /// The accepted value half of the time, so both decisions are exercised.
    fn member(accepted: Member) -> impl proptest::strategy::Strategy<Value = Member> {
        use proptest::prelude::*;
        let time = (
            prop_oneof![-1000_i64..=-40, -20_i64..=20, 40_i64..=1000],
            any::<bool>(),
        )
            .prop_map(|(offset, fractional)| Member::Time { offset, fractional });
        prop_oneof![
            4 => Just(accepted),
            1 => Just(Member::Absent),
            2 => time,
            2 => any_json().prop_map(Member::Json),
        ]
    }

    /// Rewrites every ASCII letter inside a JSON string, member names
    /// included, as a `\u` escape, which a deserializer cannot borrow from
    /// the payload.
    fn escape_letters(json: &str) -> String {
        use std::fmt::Write as _;
        let mut escaped = String::with_capacity(json.len());
        // Characters of an escape sequence still to copy unchanged.
        let (mut in_string, mut copy) = (false, 0_usize);
        let mut characters = json.chars().peekable();
        while let Some(character) = characters.next() {
            if copy > 0 {
                copy -= 1;
            } else if in_string && character == '\\' {
                copy = if characters.peek() == Some(&'u') {
                    5
                } else {
                    1
                };
            } else if character == '"' {
                in_string.toggle();
            } else if in_string && character.is_ascii_alphabetic() {
                write!(escaped, "\\u{:04x}", u32::from(character)).unwrap();
                continue;
            }
            escaped.push(character);
        }
        escaped
    }

    #[derive(Clone, Debug)]
    enum Edit {
        Replace(u8),
        Insert(u8),
        Delete,
    }

    fn edits() -> impl proptest::strategy::Strategy<Value = Vec<(f64, Edit)>> {
        use proptest::prelude::*;
        // Mostly the bytes JSON gives a meaning to.
        let byte = prop_oneof![
            3 => proptest::sample::select(br#"{}[]",:\ 0123456789eE+-.truefalsn"#.to_vec()),
            1 => any::<u8>(),
        ];
        let edit = prop_oneof![
            byte.clone().prop_map(Edit::Replace),
            byte.prop_map(Edit::Insert),
            Just(Edit::Delete),
        ];
        proptest::collection::vec((0.0..1.0_f64, edit), 1..4)
    }

    proptest::proptest! {
        // A failing case is printed; no seed file is left beside the source.
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: 256,
            failure_persistence: None,
            ..proptest::prelude::ProptestConfig::default()
        })]

        /// Registered claim shapes and escaped strings agree with the reference validation.
        #[test]
        fn generated_registered_claims_match_the_reference_validation(
            iss in member(Member::Json(CORPUS_ISSUER.into())),
            aud in member(Member::Json("api".into())),
            exp in member(Member::Time { offset: 600, fractional: false }),
            nbf in member(Member::Absent),
            custom in any_json(),
            escaped in proptest::prelude::any::<bool>(),
        ) {
            let deciders = Deciders::new();
            let now = jsonwebtoken::get_current_timestamp();
            let mut claims = serde_json::json!({"sub": "subject", "x": custom});
            for (name, member) in [("iss", &iss), ("aud", &aud), ("exp", &exp), ("nbf", &nbf)] {
                member.put(&mut claims, name, now);
            }
            let payload = if escaped {
                escape_letters(&claims.to_string())
            } else {
                claims.to_string()
            };
            let token = sign_raw(CORPUS_HEADER, payload.as_bytes());
            let here = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(deciders.here(&token));
            proptest::prop_assert_eq!(here, deciders.library(&token), "payload {}", payload);
        }

        /// A payload damaged anywhere after its subject agrees with the
        /// reference validation, unless the damage leaves it
        /// outside UTF-8. The subject stays intact because only this crate
        /// requires an identity.
        #[test]
        fn damaged_payloads_match_the_reference_validation(edits in edits()) {
            const KEPT: &str = r#"{"sub":"subject","#;
            let deciders = Deciders::new();
            let now = jsonwebtoken::get_current_timestamp();
            let mut rest = format!(
                r#""iss":"{CORPUS_ISSUER}","aud":["api"],"exp":{},"nbf":{},"t":[1.5,{{"a":"b\n"}}]}}"#,
                now + 600,
                now - 600,
            )
            .into_bytes();
            for (position, edit) in edits {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss,
                    reason = "a fraction of a short length"
                )]
                let index = (position * rest.len() as f64) as usize;
                match edit {
                    Edit::Replace(byte) if index < rest.len() => rest[index] = byte,
                    Edit::Delete if index < rest.len() => drop(rest.remove(index)),
                    Edit::Insert(byte) => rest.insert(index, byte),
                    Edit::Replace(_) | Edit::Delete => {}
                }
            }
            let payload = [KEPT.as_bytes(), &rest].concat();
            let token = sign_raw(CORPUS_HEADER, &payload);
            let here = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(deciders.here(&token));
            if std::str::from_utf8(&payload).is_ok() {
                proptest::prop_assert_eq!(
                    here,
                    deciders.library(&token),
                    "payload {}",
                    String::from_utf8_lossy(&payload)
                );
            } else {
                // See `a_payload_that_is_not_utf8_is_refused_where_jsonwebtoken_skips_it`.
                proptest::prop_assert!(!here, "payload {}", String::from_utf8_lossy(&payload));
            }
        }
    }

    #[test]
    fn a_verification_failure_reports_its_closed_reason_without_claim_values() {
        let diagnostics = Diagnostics::default();
        // Two independent registrations avoid tracing's single-dispatch shortcut
        // caching a sibling thread's NoSubscriber interest for shared callsites.
        let _registration_anchor =
            tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let dispatch = tracing::Dispatch::new(diagnostics.clone());
        let verifier = crate::Verifier::jwt(verifier(
            rsa_key_set("fixture", None),
            &[JwtAlgorithm::Rs256],
        ));
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({"sub":null,"jti":"private-claim-value"}),
        );
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        metrics::with_local_recorder(&diagnostics, || {
            tracing::dispatcher::with_default(&dispatch, || {
                assert_eq!(
                    runtime.block_on(verifier.verify(&token)),
                    Err(Failure::Invalid)
                );
            });
        });
        let counters = diagnostics.counters.lock().unwrap();
        assert!(counters.iter().any(|(key, value)| {
            key.name() == "authn_token_verifications_total"
                && *value == 1
                && key
                    .labels()
                    .any(|label| label.key() == "reason" && label.value() == "missing_claim")
        }));
        let events = diagnostics.events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.contains("authn_verification_failed")
                    && event.contains("missing_claim")),
            "recorded events: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|event| !event.contains("private-claim-value")),
            "recorded events: {events:?}"
        );
    }

    #[test]
    fn authentication_metrics_preserve_success_failure_timeout_and_cancellation_labels() {
        use std::{future::Future, task::Poll};

        let diagnostics = Diagnostics::default();
        let engine = verifier(rsa_key_set("fixture", None), &[JwtAlgorithm::Rs256]);
        engine.key_store.permit_unknown_refresh_for_test();
        let token = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("fixture"),
            &serde_json::json!({}),
        );
        let header = format!("Bearer {token}");
        let unknown = signed(
            &rsa_signing(),
            Algorithm::RS256,
            Some("rotated"),
            &serde_json::json!({}),
        );
        let unknown_header = format!("Bearer {unknown}");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        metrics::with_local_recorder(&diagnostics, || {
            // Success counters bind to the recorder installed at preparation.
            let verifier = crate::Verifier::jwt(engine);
            runtime.block_on(async {
                for transport in [Transport::Http, Transport::Grpc] {
                    verifier
                        .authenticate([header.as_bytes()], transport)
                        .await
                        .unwrap();
                }
                assert_eq!(
                    verifier
                        .authenticate(std::iter::empty(), Transport::Http)
                        .await,
                    Err(Failure::Missing)
                );
                assert_eq!(
                    verifier
                        .authenticate([b"Bearer =".as_slice()], Transport::Grpc)
                        .await,
                    Err(Failure::Malformed)
                );
                let expired = operation_context::OperationContext::with_timeout(Duration::ZERO);
                assert_eq!(
                    verifier
                        .authenticate_with_context([header.as_bytes()], Transport::Grpc, &expired)
                        .await,
                    Err(Failure::Unavailable)
                );
                let cancelled = operation_context::OperationContext::unbounded();
                cancelled.cancel();
                assert_eq!(
                    verifier
                        .authenticate_with_context([header.as_bytes()], Transport::Grpc, &cancelled)
                        .await,
                    Err(Failure::Unavailable)
                );
                let mut pending =
                    Box::pin(verifier.authenticate([unknown_header.as_bytes()], Transport::Http));
                std::future::poll_fn(|cx| {
                    assert!(pending.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                drop(pending);
            });
        });
        assert_authentication_counters(&diagnostics.counters.lock().unwrap());
    }

    fn labeled(key: &metrics::Key, labels: &[(&str, &str)]) -> bool {
        labels.iter().all(|(name, value)| {
            key.labels()
                .any(|label| label.key() == *name && label.value() == *value)
        })
    }

    fn assert_authentication_counters(counters: &[(metrics::Key, u64)]) {
        let verifications = counters
            .iter()
            .filter(|(key, _)| key.name() == "authn_token_verifications_total")
            .collect::<Vec<_>>();
        assert_eq!(verifications.len(), 2);
        assert!(verifications.iter().all(|(key, value)| {
            *value == 1
                && labeled(
                    key,
                    &[
                        ("mode", "jwt"),
                        ("outcome", "success"),
                        ("reason", "verified"),
                    ],
                )
        }));
        let outcomes = counters
            .iter()
            .filter(|(key, _)| key.name() == crate::authenticate::AUTHN_VERIFICATIONS_METRIC)
            .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 7);
        for (transport, result, failure) in [
            ("http", "success", "none"),
            ("grpc", "success", "none"),
            ("http", "failure", "missing"),
            ("grpc", "failure", "malformed"),
            ("grpc", "timeout", "timeout"),
            ("grpc", "cancelled", "cancelled"),
            ("http", "cancelled", "cancelled"),
        ] {
            assert!(outcomes.iter().any(|(key, value)| {
                *value == 1
                    && labeled(
                        key,
                        &[
                            ("transport", transport),
                            ("result", result),
                            ("failure", failure),
                        ],
                    )
            }));
        }
    }

    const EXP: u64 = 2_000;

    fn ec_verifier(signing: &EncodingKey, kid: &str) -> JwtVerifier {
        verifier(
            key_set(
                &[jwk(signing, Algorithm::ES256, Some(kid), true)],
                &[JwtAlgorithm::Es256],
            ),
            &[JwtAlgorithm::Es256],
        )
    }

    fn ec_token(signing: &EncodingKey, kid: &str, extra: &serde_json::Value) -> String {
        let mut claims = serde_json::json!({"exp": EXP});
        claims
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        signed(signing, Algorithm::ES256, Some(kid), &claims)
    }

    async fn verify_at(
        verifier: &JwtVerifier,
        token: &str,
        now: u64,
    ) -> Result<crate::Principal, VerificationReason> {
        let header = format!("Bearer {token}");
        let token = parse_bearer([header.as_bytes()]).unwrap();
        verifier
            .verify_with_now(&token, || Ok(Duration::from_secs(now)))
            .await
            .map_err(|error| error.reason)
    }

    fn remembered_count(verifier: &JwtVerifier) -> u64 {
        verifier.remembered.run_pending_tasks();
        verifier.remembered.entry_count()
    }

    #[tokio::test]
    async fn a_remembered_token_yields_the_principal_of_a_full_verification() {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        let token = ec_token(
            &signing,
            "k",
            &serde_json::json!({"scope": "b a", "client_id": "client", "act": {"sub": "actor"}}),
        );
        let full = verify_at(&memo, &token, 1_000).await.unwrap();
        assert_eq!(remembered_count(&memo), 1);
        let hit = verify_at(&memo, &token, 1_000).await.unwrap();
        let never_remembered = verify_at(&ec_verifier(&signing, "k"), &token, 1_000)
            .await
            .unwrap();
        assert_eq!(hit, full);
        assert_eq!(hit, never_remembered);
        assert_eq!(hit.scopes(), ["a", "b"]);
        assert_eq!(hit.expires_at(), EXP);
    }

    #[tokio::test]
    async fn a_remembered_token_is_refused_by_the_clock_as_a_full_verification_refuses_it() {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        let token = ec_token(&signing, "k", &serde_json::json!({"nbf": 1_000}));
        verify_at(&memo, &token, 1_000).await.unwrap();
        for (now, expected) in [
            (970, Ok(())),
            (969, Err(VerificationReason::NotYetValid)),
            (EXP + 29, Ok(())),
            (EXP + 30, Err(VerificationReason::Expired)),
            (u64::MAX, Err(VerificationReason::Expired)),
        ] {
            let hit = verify_at(&memo, &token, now).await.map(|_| ());
            let full = verify_at(&ec_verifier(&signing, "k"), &token, now)
                .await
                .map(|_| ());
            assert_eq!(hit, expected, "{now}");
            assert_eq!(hit, full, "{now}");
        }
        // Expiry drops the entry; a clock that steps back decides it afresh.
        assert_eq!(remembered_count(&memo), 0);
        assert!(verify_at(&memo, &token, 1_000).await.is_ok());
    }

    #[tokio::test]
    async fn a_remembered_token_follows_the_clock_failure_of_a_full_verification() {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        let token = ec_token(&signing, "k", &serde_json::json!({}));
        let header = format!("Bearer {token}");
        let bearer = parse_bearer([header.as_bytes()]).unwrap();
        verify_at(&memo, &token, 1_000).await.unwrap();
        let unavailable =
            || crate::unix_time(std::time::UNIX_EPOCH - std::time::Duration::from_secs(1));
        let error = memo
            .verify_with_now(&bearer, unavailable)
            .await
            .unwrap_err();
        assert_eq!(error.reason, VerificationReason::Clock);
        assert!(verify_at(&memo, &token, 1_000).await.is_ok());
    }

    #[tokio::test]
    async fn a_remembered_token_is_refused_once_its_key_leaves_the_installed_set() {
        let (signing, other) = (ec_signing(), ec_signing());
        let memo = ec_verifier(&signing, "k");
        let token = ec_token(&signing, "k", &serde_json::json!({}));
        verify_at(&memo, &token, 1_000).await.unwrap();
        let replacement = key_set(
            &[jwk(&other, Algorithm::ES256, Some("other"), true)],
            &[JwtAlgorithm::Es256],
        );
        memo.key_store.finish(1, Ok(Arc::clone(&replacement)));
        let fresh = verifier(replacement, &[JwtAlgorithm::Es256]);
        let expected = verify_at(&fresh, &token, 1_000).await.unwrap_err();
        assert_eq!(expected, VerificationReason::UnknownKey);
        assert_eq!(verify_at(&memo, &token, 1_000).await.unwrap_err(), expected);
        assert_eq!(remembered_count(&memo), 0);
    }

    #[tokio::test]
    async fn a_remembered_token_is_refused_once_its_key_rotates_to_another_key_under_the_same_kid()
    {
        let (signing, other) = (ec_signing(), ec_signing());
        let memo = ec_verifier(&signing, "k");
        let token = ec_token(&signing, "k", &serde_json::json!({}));
        verify_at(&memo, &token, 1_000).await.unwrap();
        let replacement = key_set(
            &[jwk(&other, Algorithm::ES256, Some("k"), true)],
            &[JwtAlgorithm::Es256],
        );
        memo.key_store.finish(1, Ok(replacement));
        assert_eq!(
            verify_at(&memo, &token, 1_000).await.unwrap_err(),
            VerificationReason::Signature
        );
        assert_eq!(remembered_count(&memo), 0);
    }

    #[tokio::test]
    async fn a_refresh_that_keeps_the_key_makes_the_next_verification_a_full_one_that_remembers_again()
     {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        let token = ec_token(&signing, "k", &serde_json::json!({}));
        let generation = |memo: &JwtVerifier| {
            memo.remembered
                .get(token.as_bytes())
                .map(|remembered| remembered.key_generation)
        };
        verify_at(&memo, &token, 1_000).await.unwrap();
        assert_eq!(generation(&memo), Some(memo.key_store.generation()));
        let before = memo.key_store.generation();
        memo.key_store.finish(
            1,
            Ok(key_set(
                &[jwk(&signing, Algorithm::ES256, Some("k"), true)],
                &[JwtAlgorithm::Es256],
            )),
        );
        assert_ne!(before, memo.key_store.generation());
        verify_at(&memo, &token, 1_000).await.unwrap();
        assert_eq!(generation(&memo), Some(memo.key_store.generation()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_verified_after_a_refresh_is_remembered_under_the_refreshed_set() {
        let (installed, rotated) = (ec_signing(), ec_signing());
        let memo = Arc::new(ec_verifier(&installed, "old"));
        memo.key_store.permit_unknown_refresh_for_test();
        let token = ec_token(&rotated, "new", &serde_json::json!({}));
        let call = tokio::spawn({
            let (memo, token) = (Arc::clone(&memo), token.clone());
            async move { verify_at(&memo, &token, 1_000).await.map(|_| ()) }
        });
        tokio::task::yield_now().await;
        let rotated_set = key_set(
            &[jwk(&rotated, Algorithm::ES256, Some("new"), true)],
            &[JwtAlgorithm::Es256],
        );
        memo.key_store.finish(1, Ok(rotated_set));
        assert_eq!(call.await.unwrap(), Ok(()));
        let remembered = memo.remembered.get(token.as_bytes()).unwrap();
        assert_eq!(remembered.key_generation, memo.key_store.generation());
        // The unchanged set now verifies it without another refresh.
        assert!(verify_at(&memo, &token, 1_000).await.is_ok());
    }

    #[tokio::test]
    async fn a_refused_token_is_never_remembered() {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        let good = ec_token(&signing, "k", &serde_json::json!({}));
        let (message, signature) = good.rsplit_once('.').unwrap();
        let tampered = format!("{message}.{}", signature.replacen(&signature[..2], "AA", 1));
        let claim_refusals = [
            ec_token(&signing, "k", &serde_json::json!({"aud": "other"})),
            ec_token(
                &signing,
                "k",
                &serde_json::json!({"iss": "https://other.example"}),
            ),
            ec_token(&signing, "k", &serde_json::json!({"nbf": EXP})),
            ec_token(&signing, "k", &serde_json::json!({"exp": 10})),
            ec_token(&signing, "k", &serde_json::json!({"sub": null})),
            ec_token(&signing, "k", &serde_json::json!({"scope": "a\"b"})),
        ];
        for token in std::iter::once(&tampered).chain(&claim_refusals) {
            for _ in 0..2 {
                assert!(verify_at(&memo, token, 1_000).await.is_err());
            }
        }
        assert_eq!(remembered_count(&memo), 0);
        verify_at(&memo, &good, 1_000).await.unwrap();
        assert_eq!(remembered_count(&memo), 1);
    }

    #[tokio::test]
    async fn the_memo_never_holds_more_than_its_entry_bound() {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        for index in 0..MAX_REMEMBERED + 200 {
            let token = ec_token(
                &signing,
                "k",
                &serde_json::json!({"jti": index.to_string()}),
            );
            verify_at(&memo, &token, 1_000).await.unwrap();
        }
        assert!(remembered_count(&memo) <= MAX_REMEMBERED);
    }

    #[tokio::test]
    async fn an_entry_is_held_no_longer_than_its_token_or_one_refresh_period() {
        let signing = ec_signing();
        let memo = ec_verifier(&signing, "k");
        for (exp, ttl_millis) in [(1_060, 58_950), (1_000 + 10 * 86_400, 899_750)] {
            let token = ec_token(&signing, "k", &serde_json::json!({ "exp": exp }));
            let header = format!("Bearer {token}");
            let bearer = parse_bearer([header.as_bytes()]).unwrap();
            memo.verify_with_now(&bearer, || Ok(Duration::from_millis(1_000_800)))
                .await
                .unwrap();
            let remembered = memo.remembered.get(token.as_bytes()).unwrap();
            let insertion_delay = Duration::from_millis(250);
            let ttl = moka::Expiry::expire_after_create(
                &super::RememberedExpiry,
                &token.into_bytes().into_boxed_slice(),
                &remembered,
                remembered.sampled_at + insertion_delay,
            );
            assert_eq!(ttl, Some(Duration::from_millis(ttl_millis)), "{exp}");
        }
    }
}
