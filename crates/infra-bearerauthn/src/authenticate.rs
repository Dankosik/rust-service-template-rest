use std::{mem::DropGuard, sync::OnceLock};

use enum_map::EnumMap;
use operation_context::{OperationContext, Stopped};

use crate::{Failure, Principal, Verifier, parse_bearer};

pub(crate) const AUTHN_VERIFICATIONS_METRIC: &str = "authn_verifications_total";

#[derive(Clone, Copy, Debug, Eq, PartialEq, enum_map::Enum)]
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

struct PerTransport {
    success: metrics::Counter,
    refused: EnumMap<Refusal, OnceLock<metrics::Counter>>,
}

/// Prepared success handles and lazy refusal handles per inbound transport.
pub(crate) struct TransportCounters(EnumMap<Transport, PerTransport>);

impl TransportCounters {
    pub(crate) fn new() -> Self {
        metrics::describe_counter!(
            AUTHN_VERIFICATIONS_METRIC,
            metrics::Unit::Count,
            "Inbound bearer-authentication verification outcomes."
        );
        Self(EnumMap::from_fn(|transport: Transport| PerTransport {
            success: metrics::counter!(
                AUTHN_VERIFICATIONS_METRIC,
                "transport" => transport.label(),
                "result" => "success",
                "failure" => "none"
            ),
            refused: EnumMap::default(),
        }))
    }

    fn record_success(&self, transport: Transport) {
        self.0[transport].success.increment(1);
    }

    fn record_refusal(&self, transport: Transport, refusal: Refusal) {
        let (result, failure) = refusal.labels();
        self.0[transport].refused[refusal]
            .get_or_init(|| {
                metrics::counter!(
                    AUTHN_VERIFICATIONS_METRIC,
                    "transport" => transport.label(),
                    "result" => result,
                    "failure" => failure
                )
            })
            .increment(1);
    }
}

#[derive(Clone, Copy, enum_map::Enum)]
enum Refusal {
    Failure(Failure),
    TimedOut,
    Cancelled,
}

impl Refusal {
    const fn from_stopped(stopped: Stopped) -> Self {
        match stopped {
            Stopped::Deadline => Self::TimedOut,
            Stopped::Cancelled => Self::Cancelled,
        }
    }

    const fn labels(self) -> (&'static str, &'static str) {
        match self {
            Self::Failure(Failure::Missing) => ("failure", "missing"),
            Self::Failure(Failure::Malformed) => ("failure", "malformed"),
            Self::Failure(Failure::Invalid) => ("failure", "invalid"),
            Self::Failure(Failure::Unavailable) => ("failure", "unavailable"),
            Self::TimedOut => ("timeout", "timeout"),
            Self::Cancelled => ("cancelled", "cancelled"),
        }
    }
}

impl Verifier {
    /// Fixture-only: [`Verifier::authenticate_with_context`] under a fresh
    /// 3-second budget that is not linked to any caller's deadline or
    /// cancellation.
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
    pub async fn authenticate_with_context<'a>(
        &self,
        authorization: impl IntoIterator<Item = &'a [u8]>,
        transport: Transport,
        context: &OperationContext,
    ) -> Result<Principal, Failure> {
        let counters = &self.counters.transport;
        let outcome = DropGuard::new(transport, |transport| {
            let refusal = context
                .stopped()
                .map_or(Refusal::Cancelled, Refusal::from_stopped);
            counters.record_refusal(transport, refusal);
        });
        let result = match parse_bearer(authorization) {
            Ok(token) => self.verify_with_context(&token, context).await,
            Err(failure) => Err(failure),
        };
        if let Err(stopped) = context.check() {
            counters.record_refusal(transport, Refusal::from_stopped(stopped));
            DropGuard::dismiss(outcome);
            return Err(Failure::Unavailable);
        }
        match &result {
            Ok(_) => counters.record_success(transport),
            Err(failure) => counters.record_refusal(transport, Refusal::Failure(*failure)),
        }
        DropGuard::dismiss(outcome);
        result
    }
}
