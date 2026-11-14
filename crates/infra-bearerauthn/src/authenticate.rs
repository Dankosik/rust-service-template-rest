//! The inbound bearer boundary shared by every transport.

use std::mem::DropGuard;

use operation_context::{OperationContext, Stopped};

use crate::{Failure, Principal, Verifier, parse_bearer};

/// Bearer-authentication outcomes at an inbound transport boundary.
pub(crate) const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

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
    TimedOut,
    Cancelled,
}

impl Outcome {
    const fn from_stopped(stopped: Stopped) -> Self {
        match stopped {
            Stopped::Deadline => Self::TimedOut,
            Stopped::Cancelled => Self::Cancelled,
        }
    }

    const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Success => ("success", "none"),
            Self::Failure(Failure::Missing) => ("failure", "missing"),
            Self::Failure(Failure::Malformed) => ("failure", "malformed"),
            Self::Failure(Failure::Invalid) => ("failure", "invalid"),
            Self::Failure(Failure::Unavailable) => ("failure", "unavailable"),
            Self::TimedOut => ("timeout", "timeout"),
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
    /// A stopped caller is never counted as invalid credentials.
    ///
    /// Pass every `Authorization` value of the request. A second value is
    /// [`Failure::Malformed`]. No value or a single value with a valid scheme
    /// other than Bearer is [`Failure::Missing`].
    /// Records exactly one `authn_verifications_total` outcome labelled with
    /// `transport`: `timeout` when the context's deadline has expired, including
    /// when the transport drops this future to enforce it. Cancellation and
    /// abandonment before the deadline are recorded as `cancelled`.
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
        let outcome = DropGuard::new(transport, |transport| {
            let outcome = context
                .stopped()
                .map_or(Outcome::Cancelled, Outcome::from_stopped);
            counter(transport, outcome).increment(1);
        });
        if let Err(stopped) = context.check() {
            counter(transport, Outcome::from_stopped(stopped)).increment(1);
            DropGuard::dismiss(outcome);
            return Err(Failure::Unavailable);
        }
        let result = match parse_bearer(authorization) {
            Ok(token) => self.verify_with_context(&token, context).await,
            Err(failure) => Err(failure),
        };
        if let Err(stopped) = context.check() {
            counter(transport, Outcome::from_stopped(stopped)).increment(1);
            DropGuard::dismiss(outcome);
            return Err(Failure::Unavailable);
        }
        match &result {
            Ok(_) => self.counters.transport.get(transport).increment(1),
            Err(failure) => counter(transport, Outcome::Failure(*failure)).increment(1),
        }
        DropGuard::dismiss(outcome);
        result
    }
}
