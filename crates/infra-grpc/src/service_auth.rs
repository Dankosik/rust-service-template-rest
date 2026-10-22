//! Service tokens on native gRPC: caller admission for business calls and
//! the client binding that sends this service's token.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use http::header::AUTHORIZATION;
use infra_serviceauth::{Caller, CallerName, ReceiverTokens, Verifier};
use service_failure::Code;
use tonic::body::Body;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::{Client, Failure};

/// Verified callers by outcome; caller values come only from the trust registry.
pub const SERVICE_AUTH_CALLS_METRIC: &str = "service_auth_calls_total";

/// The two paths reflection serves once it is added.
pub(crate) const REFLECTION_METHODS: [&str; 2] = [
    "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
];

const NOT_ADMITTED_DETAIL: &str = "the verified caller is not admitted to this method";

/// The permission table and, unless service authentication is off, the
/// verifier that names each call's caller.
pub(crate) struct Admission {
    verifier: Option<Verifier>,
    rules: HashMap<String, Box<[CallerName]>>,
    /// Registered once per trusted caller: an admission is the hot path.
    counters: HashMap<CallerName, (metrics::Counter, metrics::Counter)>,
}

impl Admission {
    pub(crate) fn new(
        verifier: Option<Verifier>,
        rules: HashMap<String, Box<[CallerName]>>,
    ) -> Self {
        metrics::describe_counter!(
            SERVICE_AUTH_CALLS_METRIC,
            metrics::Unit::Count,
            "gRPC calls of verified callers by caller and admission outcome."
        );
        let counters = verifier
            .iter()
            .flat_map(Verifier::callers)
            .map(|caller| {
                let counter = |outcome: &'static str| {
                    metrics::counter!(SERVICE_AUTH_CALLS_METRIC, "caller" => caller.to_string(), "outcome" => outcome)
                };
                (caller.clone(), (counter("admitted"), counter("not_admitted")))
            })
            .collect();
        Self {
            verifier,
            rules,
            counters,
        }
    }

    pub(crate) const fn verifies(&self) -> bool {
        self.verifier.is_some()
    }
}

/// Names the caller of a business call and admits it only to a method whose
/// rule lists it. A path without a rule admits nobody. Either way the
/// `Authorization` metadata never reaches the handler.
pub(crate) async fn admit(admission: Arc<Admission>, mut request: Request, next: Next) -> Response {
    let Some(verifier) = &admission.verifier else {
        // Bearer authentication already admitted the call, or service
        // authentication is off, which configuration permits only in a
        // development environment.
        request.headers_mut().remove(AUTHORIZATION);
        return next.run(request).await;
    };
    let Some(context) = request
        .extensions()
        .get::<operation_context::OperationContext>()
        .cloned()
    else {
        return tonic::Status::from(Failure::new(Code::InternalServerError)).into_http();
    };
    let authorization = request
        .headers()
        .get_all(AUTHORIZATION)
        .iter()
        .map(http::HeaderValue::as_bytes);
    let caller = match verifier.authenticate(authorization) {
        Ok(caller) => caller,
        Err(failure) => return crate::router::reject(request, verification_status(failure)).await,
    };
    let path = request.uri().path();
    let rule = admission.rules.get(path);
    let counters = admission.counters.get(caller.caller_name());
    if !rule.is_some_and(|callers| callers.contains(caller.caller_name())) {
        if let Some((_, not_admitted)) = counters {
            not_admitted.increment(1);
        }
        // Only a registered method has a rule, so the label stays bounded.
        let method = if rule.is_some() { path } else { "unknown" };
        tracing::warn!(
            caller = caller.name(),
            method,
            "service_auth_caller_not_admitted"
        );
        let status = Failure::new(Code::Forbidden)
            .into_status_as(tonic::Code::PermissionDenied, NOT_ADMITTED_DETAIL);
        return crate::router::reject(request, status).await;
    }
    if context.stopped().is_some() {
        return crate::router::opening_status(&context).into_http();
    }
    if let Some((admitted, _)) = counters {
        admitted.increment(1);
    }
    tracing::Span::current().set_attribute("peer.service", caller.name().to_owned());
    request.extensions_mut().insert::<Caller>(caller);
    request.headers_mut().remove(AUTHORIZATION);
    next.run(request).await
}

/// Missing, malformed and invalid tokens are one credential failure; a
/// clock before the epoch may pass later unchanged.
fn verification_status(failure: infra_serviceauth::Failure) -> tonic::Status {
    Failure::new(match failure {
        infra_serviceauth::Failure::Missing
        | infra_serviceauth::Failure::Malformed
        | infra_serviceauth::Failure::Invalid => Code::Unauthorized,
        infra_serviceauth::Failure::Unavailable => Code::ServiceUnavailable,
    })
    .into()
}

/// A cloneable governed gRPC client that sends this service's token for one
/// receiver with every call.
#[derive(Clone, Debug)]
pub struct ServiceTokenClient {
    tokens: ReceiverTokens,
    resource: Client,
}

impl Client {
    /// Binds this client to the token source's handle for its receiver.
    #[must_use]
    pub fn with_service_tokens(self, tokens: ReceiverTokens) -> ServiceTokenClient {
        ServiceTokenClient {
            tokens,
            resource: self,
        }
    }
}

impl tower::Service<http::Request<Body>> for ServiceTokenClient {
    type Response = http::Response<Body>;
    type Error = tonic::Status;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    /// Always ready, as the governed client it wraps.
    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: http::Request<Body>) -> Self::Future {
        // A caller-supplied value is this service's composition mistake, so
        // it is `INTERNAL`, as the OAuth binding answers it.
        if request.headers().contains_key(AUTHORIZATION) {
            return Box::pin(std::future::ready(Err(tonic::Status::internal(
                "authorization conflicts with the service token",
            ))));
        }
        match self.tokens.authorization() {
            Ok(Some(value)) => {
                request.headers_mut().insert(AUTHORIZATION, value);
            }
            Ok(None) => {}
            Err(error) => {
                let mut status = tonic::Status::unavailable("service token unavailable");
                status.set_source(Arc::new(error));
                return Box::pin(std::future::ready(Err(status)));
            }
        }
        self.resource.call(request)
    }
}
