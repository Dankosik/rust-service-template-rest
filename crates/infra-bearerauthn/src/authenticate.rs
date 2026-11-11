//! The inbound bearer boundary shared by every transport.

use operation_context::OperationContext;

use crate::{Failure, Principal, Verifier, parse_bearer};

/// Bearer-authentication outcomes at an inbound transport boundary.
pub const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

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
            http: counter(Transport::Http, Outcome::Success),
            grpc: counter(Transport::Grpc, Outcome::Success),
        }
    }

    const fn get(&self, transport: Transport) -> &metrics::Counter {
        match transport {
            Transport::Http => &self.http,
            Transport::Grpc => &self.grpc,
        }
    }
}

/// One recorded transport outcome; it owns the `result` and `failure` label pair.
#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Failure(Failure),
    Cancelled,
}

impl Outcome {
    const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Success => ("success", "none"),
            Self::Failure(Failure::Missing) => ("failure", "missing"),
            Self::Failure(Failure::Malformed) => ("failure", "malformed"),
            Self::Failure(Failure::Invalid) => ("failure", "invalid"),
            Self::Failure(Failure::Unavailable) => ("failure", "unavailable"),
            Self::Cancelled => ("cancelled", "cancelled"),
        }
    }
}

fn counter(transport: Transport, outcome: Outcome) -> metrics::Counter {
    let (result, failure) = outcome.labels();
    metrics::counter!(
        AUTHN_VERIFICATIONS_METRIC,
        "transport" => transport.label(),
        "result" => result,
        "failure" => failure
    )
}

impl Verifier {
    /// Fixture-only: [`Verifier::authenticate_with_context`] under a fresh
    /// 3-second budget that is not linked to any caller's deadline or
    /// cancellation.
    ///
    /// # Errors
    ///
    /// Returns the closed [`Failure`] the transport maps to its response.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn authenticate<'a>(
        &self,
        authorization: impl IntoIterator<Item = &'a [u8]>,
        transport: Transport,
    ) -> Result<Principal, Failure> {
        self.authenticate_with_context(
            authorization,
            transport,
            &OperationContext::with_timeout(crate::provider::PROVIDER_TIMEOUT),
        )
        .await
    }

    /// Parses and verifies under the caller's original deadline and cancellation.
    /// A stopped caller is counted as cancelled, never as invalid credentials.
    ///
    /// `authorization` is every `Authorization` value of the request; see
    /// [`parse_bearer`] for how a second value or another scheme is classified.
    /// Records exactly one [`AUTHN_VERIFICATIONS_METRIC`] outcome labelled with
    /// `transport`, including `cancelled` when the caller drops the future.
    ///
    /// # Errors
    /// Returns [`Failure::Unavailable`] when the context stops; otherwise returns
    /// the closed [`Failure`] the transport maps to its response.
    pub async fn authenticate_with_context<'a>(
        &self,
        authorization: impl IntoIterator<Item = &'a [u8]>,
        transport: Transport,
        context: &OperationContext,
    ) -> Result<Principal, Failure> {
        let mut outcome = OutcomeGuard {
            transport,
            recorded: false,
        };
        context.check().map_err(|_| Failure::Unavailable)?;
        let result = match parse_bearer(authorization) {
            Ok(token) => self.verify_with_context(&token, context).await,
            Err(failure) => Err(failure),
        };
        context.check().map_err(|_| Failure::Unavailable)?;
        match &result {
            Ok(_) => self.counters.transport.get(transport).increment(1),
            Err(failure) => {
                counter(transport, Outcome::Failure(*failure)).increment(1);
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
            counter(self.transport, Outcome::Cancelled).increment(1);
        }
    }
}
