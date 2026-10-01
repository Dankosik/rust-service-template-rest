//! The inbound bearer boundary shared by every transport.

use crate::{Failure, Principal, Verifier, parse_bearer};

/// Bearer-authentication outcomes at an inbound transport boundary.
pub const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

const SUCCESS_RESULT: &str = "success";
const NO_FAILURE: &str = "none";

/// The inbound transport an authentication outcome is counted under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    Http,
    Grpc,
}

impl Transport {
    const fn label(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Grpc => "grpc",
        }
    }
}

/// Prepared success handles, so a success needs no registry lookup.
pub(crate) struct TransportSuccessCounters {
    http: metrics::Counter,
    grpc: metrics::Counter,
}

impl TransportSuccessCounters {
    pub(crate) fn new() -> Self {
        metrics::describe_counter!(
            AUTHN_VERIFICATIONS_METRIC,
            metrics::Unit::Count,
            "Inbound bearer-authentication verification outcomes."
        );
        Self {
            http: counter(Transport::Http, SUCCESS_RESULT, NO_FAILURE),
            grpc: counter(Transport::Grpc, SUCCESS_RESULT, NO_FAILURE),
        }
    }

    const fn get(&self, transport: Transport) -> &metrics::Counter {
        match transport {
            Transport::Http => &self.http,
            Transport::Grpc => &self.grpc,
        }
    }
}

fn counter(transport: Transport, result: &'static str, failure: &'static str) -> metrics::Counter {
    metrics::counter!(
        AUTHN_VERIFICATIONS_METRIC,
        "transport" => transport.label(),
        "result" => result,
        "failure" => failure
    )
}

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
        transport: Transport,
    ) -> Result<Principal, Failure> {
        let mut outcome = OutcomeGuard {
            transport,
            recorded: false,
        };
        let result = match parse_bearer(authorization) {
            Ok(token) => self.verify(&token).await,
            Err(failure) => Err(failure),
        };
        match &result {
            Ok(_) => self.counters.transport.get(transport).increment(1),
            Err(failure) => {
                counter(transport, "failure", failure_class(*failure)).increment(1);
            }
        }
        outcome.recorded = true;
        result
    }
}

/// Records `cancelled` on drop unless an outcome was recorded.
struct OutcomeGuard {
    transport: Transport,
    recorded: bool,
}

impl Drop for OutcomeGuard {
    fn drop(&mut self) {
        if !self.recorded {
            counter(self.transport, "cancelled", "cancelled").increment(1);
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
