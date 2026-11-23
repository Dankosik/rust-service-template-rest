use std::sync::{Arc, OnceLock};

use enum_map::EnumMap;
use operation_context::{OperationContext, Stopped};

use crate::{
    Failure, PresentedToken, Principal, VerificationError, VerificationReason,
    authenticate::TransportCounters,
};

const TOKEN_VERIFICATIONS_METRIC: &str = "authn_token_verifications_total";

pub(crate) fn unix_now() -> Result<std::time::Duration, VerificationError> {
    unix_time(std::time::SystemTime::now())
}

pub(crate) fn unix_time(
    now: std::time::SystemTime,
) -> Result<std::time::Duration, VerificationError> {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| VerificationError::unavailable(VerificationReason::Clock))
}

fn record_verification(
    counters: &VerificationCounters,
    result: Result<Principal, VerificationError>,
) -> VerificationOutcome {
    match result {
        Ok(principal) => {
            counters.verified.increment(1);
            VerificationOutcome::Verified(principal)
        }
        Err(error) => {
            let mode = counters.mode;
            let reason = error.reason.label();
            counters.refused[error.reason]
                .get_or_init(|| {
                    metrics::counter!(
                        TOKEN_VERIFICATIONS_METRIC,
                        "mode" => mode,
                        "outcome" => "failure",
                        "reason" => reason
                    )
                })
                .increment(1);
            tracing::debug!(mode, reason, "authn_verification_failed");
            VerificationOutcome::Failed(error.failure)
        }
    }
}

struct VerificationCounters {
    mode: &'static str,
    verified: metrics::Counter,
    refused: EnumMap<VerificationReason, OnceLock<metrics::Counter>>,
    transport: TransportCounters,
}

impl VerificationCounters {
    fn new(mode: &'static str) -> Self {
        metrics::describe_counter!(
            TOKEN_VERIFICATIONS_METRIC,
            "Token verification decisions by engine and closed reason"
        );
        Self {
            mode,
            verified: metrics::counter!(TOKEN_VERIFICATIONS_METRIC, "mode" => mode, "outcome" => "success", "reason" => "verified"),
            refused: EnumMap::default(),
            transport: TransportCounters::new(),
        }
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("Verifier([REDACTED])")]
pub struct Verifier(Arc<Inner>);

struct Inner {
    engine: Engine,
    counters: VerificationCounters,
}

enum Engine {
    // template:begin oidc-jwt:authn-jwt-engine
    Jwt(crate::jwt::JwtVerifier),
    // template:end oidc-jwt:authn-jwt-engine
    // template:begin oidc-introspection:authn-introspection-engine
    Introspection(Arc<crate::introspection::IntrospectionVerifier>),
    // template:end oidc-introspection:authn-introspection-engine
}

pub(crate) enum VerificationOutcome {
    Verified(Principal),
    Failed(Failure),
    Stopped(Stopped),
}

impl Verifier {
    fn new(engine: Engine) -> Self {
        let mode = match &engine {
            // template:begin oidc-jwt:authn-jwt-engine-mode
            Engine::Jwt(_) => "jwt",
            // template:end oidc-jwt:authn-jwt-engine-mode
            // template:begin oidc-introspection:authn-introspection-engine-mode
            Engine::Introspection(_) => "introspection",
            // template:end oidc-introspection:authn-introspection-engine-mode
        };
        Self(Arc::new(Inner {
            engine,
            counters: VerificationCounters::new(mode),
        }))
    }

    // template:begin oidc-jwt:authn-jwt-verifier
    pub(crate) fn jwt(engine: crate::jwt::JwtVerifier) -> Self {
        Self::new(Engine::Jwt(engine))
    }
    // template:end oidc-jwt:authn-jwt-verifier

    // template:begin oidc-introspection:authn-introspection-verifier
    pub(crate) fn introspection(engine: Arc<crate::introspection::IntrospectionVerifier>) -> Self {
        Self::new(Engine::Introspection(engine))
    }
    // template:end oidc-introspection:authn-introspection-verifier

    pub(crate) fn transport_counters(&self) -> &TransportCounters {
        &self.0.counters.transport
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn verify(&self, token: &PresentedToken<'_>) -> Result<Principal, Failure> {
        self.verify_with_context(
            token,
            &OperationContext::with_timeout(crate::provider::PROVIDER_TIMEOUT),
        )
        .await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn verify_with_context(
        &self,
        token: &PresentedToken<'_>,
        context: &OperationContext,
    ) -> Result<Principal, Failure> {
        match self.verify_checked(token, context).await {
            VerificationOutcome::Verified(principal) => Ok(principal),
            VerificationOutcome::Failed(failure) => Err(failure),
            VerificationOutcome::Stopped(_) => Err(Failure::Unavailable),
        }
    }

    pub(crate) async fn verify_checked(
        &self,
        token: &PresentedToken<'_>,
        context: &OperationContext,
    ) -> VerificationOutcome {
        if let Err(stopped) = context.check() {
            return VerificationOutcome::Stopped(stopped);
        }
        let verification = async {
            match &self.0.engine {
                // template:begin oidc-jwt:authn-jwt-verify
                Engine::Jwt(engine) => engine.verify(token).await,
                // template:end oidc-jwt:authn-jwt-verify
                // template:begin oidc-introspection:authn-introspection-verify
                Engine::Introspection(engine) => engine.verify(token, context).await,
                // template:end oidc-introspection:authn-introspection-verify
            }
        };
        let result = tokio::select! {
            biased;
            result = verification => result,
            stopped = context.wait_stopped() => return VerificationOutcome::Stopped(stopped),
        };
        if let Err(stopped) = context.check() {
            return VerificationOutcome::Stopped(stopped);
        }
        record_verification(&self.0.counters, result)
    }
}
