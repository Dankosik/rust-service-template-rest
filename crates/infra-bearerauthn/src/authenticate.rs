//! The inbound bearer boundary shared by every transport.

use std::sync::Once;

use crate::{Failure, Principal, Verifier, parse_bearer};

/// Bearer-authentication outcomes at an inbound transport boundary.
pub const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

static DESCRIBE: Once = Once::new();

impl Verifier {
    /// Parses the `Authorization` header values and verifies the bearer.
    ///
    /// Records exactly one [`AUTHN_VERIFICATIONS_METRIC`] outcome labelled with
    /// `transport`, including `cancelled` when the caller drops the future.
    ///
    /// # Errors
    ///
    /// Returns the closed [`Failure`] the transport maps to its response.
    pub async fn authenticate<'a>(
        &self,
        authorization: impl IntoIterator<Item = &'a [u8]>,
        transport: &'static str,
    ) -> Result<Principal, Failure> {
        let mut outcome = OutcomeGuard::new(transport);
        let result = match parse_bearer(authorization) {
            Ok(token) => self.verify(&token).await,
            Err(failure) => Err(failure),
        };
        match (&result, self.counters.transport(transport)) {
            (Ok(_), Some(success)) => {
                // Known transports use a prepared handle, avoiding a registry
                // lookup. Disarm the guard so drop does not also count cancellation.
                success.increment(1);
                outcome.recorded = true;
            }
            _ => outcome.record(result.as_ref().err().copied()),
        }
        result
    }
}

/// Records `cancelled` on drop unless an outcome was recorded.
struct OutcomeGuard {
    transport: &'static str,
    recorded: bool,
}

impl OutcomeGuard {
    fn new(transport: &'static str) -> Self {
        DESCRIBE.call_once(|| {
            metrics::describe_counter!(
                AUTHN_VERIFICATIONS_METRIC,
                metrics::Unit::Count,
                "Inbound bearer-authentication verification outcomes."
            );
        });
        Self {
            transport,
            recorded: false,
        }
    }

    fn record(&mut self, failure: Option<Failure>) {
        match failure {
            None => self.count("success", "none"),
            Some(failure) => self.count("failure", failure_class(failure)),
        }
        self.recorded = true;
    }

    fn count(&self, result: &'static str, failure: &'static str) {
        metrics::counter!(
            AUTHN_VERIFICATIONS_METRIC,
            "transport" => self.transport,
            "result" => result,
            "failure" => failure
        )
        .increment(1);
    }
}

impl Drop for OutcomeGuard {
    fn drop(&mut self) {
        if !self.recorded {
            self.count("cancelled", "cancelled");
        }
    }
}

const fn failure_class(failure: Failure) -> &'static str {
    match failure {
        Failure::Missing => "missing",
        Failure::Malformed => "malformed",
        Failure::Invalid => "invalid",
        Failure::Unavailable => "unavailable",
    }
}
