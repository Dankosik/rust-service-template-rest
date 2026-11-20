use std::{mem::DropGuard, sync::OnceLock};

use enum_map::EnumMap;
use operation_context::{OperationContext, Stopped};

use crate::{Failure, PresentedToken, Principal, Verifier, parse_bearer};

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
    pub async fn authenticate<'a>(
        &self,
        context: &OperationContext,
        authorization: impl IntoIterator<Item = &'a [u8]>,
        transport: Transport,
    ) -> Result<Principal, Failure> {
        self.authenticate_presented(context, authorization, transport)
            .await
            .map(|(principal, _)| principal)
    }

    pub async fn authenticate_presented<'a>(
        &self,
        context: &OperationContext,
        authorization: impl IntoIterator<Item = &'a [u8]>,
        transport: Transport,
    ) -> Result<(Principal, PresentedToken<'a>), Failure> {
        let counters = self.transport_counters();
        let refusal_guard = DropGuard::new(transport, |transport| {
            let refusal = context
                .stopped()
                .map_or(Refusal::Cancelled, Refusal::from_stopped);
            counters.record_refusal(transport, refusal);
        });
        let result = match parse_bearer(authorization) {
            Ok(token) => self
                .verify_with_context(&token, context)
                .await
                .map(|principal| (principal, token)),
            Err(failure) => Err(failure),
        };
        DropGuard::dismiss(refusal_guard);
        if let Err(stopped) = context.check() {
            counters.record_refusal(transport, Refusal::from_stopped(stopped));
            return Err(Failure::Unavailable);
        }
        match &result {
            Ok(_) => counters.record_success(transport),
            Err(failure) => counters.record_refusal(transport, Refusal::Failure(*failure)),
        }
        result
    }
}
