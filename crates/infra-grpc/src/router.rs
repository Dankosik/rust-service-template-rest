use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt as _;
use tonic::server::NamedService as _;

use self::admission::{AdmissionLimit, admit_business_call, admit_opening};
use crate::call::{Handoff, Side};
use crate::health::HealthServer;
use crate::protocol::grpc_timeout;
use crate::{Error, Services};

mod admission;

pub(crate) use admission::Shed;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Upper bound for a business call's time to response headers,
    /// authentication included. A caller's shorter `grpc-timeout` wins.
    pub request_timeout: Duration,
    /// Independent bounds on pre-header openings and admitted business calls
    /// through terminal status, shared by router clones. A call over a bound
    /// waits for a permit within its opening budget and the shared
    /// [`Limits::max_admission_wait`]. `None` disables both bounds.
    pub max_in_flight: Option<NonZeroU32>,
    /// Total time a call can wait for permits across both queues. Zero refuses
    /// a call at once when no permit is available. The remaining opening
    /// budget less the refusal reserve also bounds the wait.
    pub max_admission_wait: Duration,
    /// Accepted connections at once; `None` accepts without a bound.
    pub max_connections: Option<NonZeroU32>,
    /// Age after which a connection gets GOAWAY; `None` sets no age.
    pub max_connection_age: Option<Duration>,
}

pub fn router(
    mut services: Services,
    readiness: ::health::ReadinessReader,
    // template:begin authn:grpc-router-verifier
    verifier: Option<infra_bearerauthn::Verifier>,
    // template:end authn:grpc-router-verifier
    limits: Limits,
) -> Result<axum::Router, Error> {
    if services.reflection {
        services.attach_reflection()?;
    }
    services.learn(tonic_health::pb::FILE_DESCRIPTOR_SET)?;
    #[allow(
        unused_variables,
        reason = "bearer authentication alone reads it; service tokens replace it"
    )]
    let service_tokens = false;
    // template:begin service-auth:grpc-router-caller-rules
    let admission = std::sync::Arc::new(services.take_caller_admission()?);
    #[allow(
        unused_variables,
        reason = "only the authn region reads it, to skip bearer authentication"
    )]
    let service_tokens = admission.verifies();
    // template:end service-auth:grpc-router-caller-rules
    let series = crate::observe::Series::server(services.method_paths());
    let mut business = services.routes.routes().into_axum_router();
    if let Some(limit) = limits.max_in_flight {
        crate::observe::register_shed();
        business = business.layer(axum::middleware::from_fn_with_state(
            AdmissionLimit::new(limit, limits.max_admission_wait),
            admit_business_call,
        ));
    }
    // template:begin service-auth:grpc-router-admit-callers
    // Inside bearer authentication; without a service-token verifier it only
    // strips `Authorization` before the handler.
    let business = business.layer(axum::middleware::from_fn_with_state(
        admission,
        crate::service_auth::admit_caller,
    ));
    // template:end service-auth:grpc-router-admit-callers
    // template:begin authn:grpc-router-authenticate
    let scopes = std::sync::Arc::new(std::mem::take(&mut services.scopes));
    let business = if service_tokens {
        // A service token names a caller and carries no scope, so a declared
        // requirement could only be dropped.
        if !scopes.is_empty() {
            return Err(Error::ScopesWithServiceTokens);
        }
        business
    } else {
        match verifier {
            Some(verifier) => business.layer(axum::middleware::from_fn_with_state(
                (verifier, scopes),
                crate::authn::authenticate,
            )),
            None => return Err(Error::UnauthenticatedBusinessCalls),
        }
    };
    // template:end authn:grpc-router-authenticate
    let business = if let Some(limit) = limits.max_in_flight {
        business.layer(axum::middleware::from_fn_with_state(
            AdmissionLimit::new(limit, limits.max_admission_wait),
            admit_opening,
        ))
    } else {
        business
    };
    let business = business.layer(axum::middleware::from_fn_with_state(
        Some(limits.request_timeout),
        opening,
    ));
    Ok(with_health(business, readiness, services.names).layer(
        axum::middleware::from_fn_with_state(series, crate::observe::observe),
    ))
}

#[must_use]
pub fn server_options(limits: Limits) -> infra_http::ServerOptions {
    infra_http::ServerOptions {
        header_read_timeout: Duration::from_secs(5),
        max_header_bytes: 16 * 1024,
        max_connections: limits.max_connections,
        max_connection_age: limits.max_connection_age,
    }
}

fn with_health(
    router: axum::Router,
    readiness: ::health::ReadinessReader,
    names: BTreeSet<&'static str>,
) -> axum::Router {
    let health = HealthServer::new(crate::health::Adapter::new(readiness, names));
    #[expect(
        clippy::disallowed_methods,
        reason = "gRPC health registration belongs to the separate tonic listener"
    )]
    let health =
        axum::Router::new().route_service(&format!("/{}/{{*rest}}", HealthServer::NAME), health);
    router.merge(health.layer(axum::middleware::from_fn_with_state(
        None::<Duration>,
        opening,
    )))
}

/// Caller-owned lifetime after the response headers (caller `grpc-timeout`, no
/// server cap). A separate type so it never collides with the opening
/// `OperationContext` extension.
#[derive(Clone, Debug)]
pub struct ResponseContext(operation_context::OperationContext);

impl ResponseContext {
    #[must_use]
    pub const fn operation(&self) -> &operation_context::OperationContext {
        &self.0
    }

    pub(crate) fn into_operation(self) -> operation_context::OperationContext {
        self.0
    }
}

/// Once the opening context stops, that stop wins over any later result, so
/// every later layer checks it again.
async fn opening(
    State(cap): State<Option<Duration>>,
    mut request: Request,
    next: Next,
) -> Response {
    use operation_context::{Deadline, OperationContext};
    let origin = tokio::time::Instant::now();
    let caller = grpc_timeout(request.headers());
    let Some(budget) = cap.into_iter().chain(caller).min() else {
        return next.run(request).await;
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    let opening_context =
        OperationContext::new(Some(Deadline::new(origin, budget)), cancellation.clone());
    let response_context = ResponseContext(OperationContext::new(
        caller.map(|budget| Deadline::new(origin, budget)),
        cancellation.clone(),
    ));
    let guard = cancellation.drop_guard();
    request.extensions_mut().insert(opening_context.clone());
    request.extensions_mut().insert(response_context.clone());
    let mut response = tokio::select! {
        biased;
        reason = opening_context.wait_stopped() => return Side::Server.stopped(reason).into_http(),
        response = next.run(request) => response,
    };
    if let Some(reason) = opening_context.stopped() {
        return Side::Server.stopped(reason).into_http();
    }
    response.extensions_mut().insert(response_context);
    response.extensions_mut().insert(Handoff::new(guard));
    response
}

pub(crate) fn opening_context(request: &Request) -> Option<&operation_context::OperationContext> {
    request
        .extensions()
        .get::<operation_context::OperationContext>()
}

const REJECT_DRAIN: Duration = Duration::from_millis(100);
const REJECT_DRAIN_BYTES: usize = 64 * 1024;

/// Drains the rest of the request body first: answering at once closes the
/// stream while DATA is in flight, h2 resets it, and after 1024 such resets
/// closes the connection with every other call on it.
#[allow(
    dead_code,
    reason = "only the optional bearer and service authentication profiles use the full drain"
)]
pub(crate) async fn reject(request: Request, status: tonic::Status) -> Response {
    reject_with_drain(request, status, REJECT_DRAIN).await
}

async fn reject_with_drain(request: Request, status: tonic::Status, drain: Duration) -> Response {
    let mut body = request.into_body();
    let _ = tokio::time::timeout(drain, async {
        let mut read = 0;
        while let Some(Ok(frame)) = body.frame().await {
            tokio::task::consume_budget().await;
            read += frame.data_ref().map_or(0, Bytes::len);
            if read > REJECT_DRAIN_BYTES {
                break;
            }
        }
    })
    .await;
    status.into_http()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_rejected_call_reads_at_most_one_frame_past_64_kib() {
        use futures_util::StreamExt as _;
        use std::convert::Infallible;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let frames = Arc::new(AtomicUsize::new(0));
        let polled = Arc::clone(&frames);
        let body = futures_util::stream::iter(0..10).map(move |_| {
            polled.fetch_add(1, Ordering::SeqCst);
            Ok::<_, Infallible>(Bytes::from(vec![0_u8; 16 * 1024]))
        });
        let request = Request::new(axum::body::Body::from_stream(body));
        let response = reject(request, tonic::Status::unavailable("closed")).await;
        assert_eq!(response.headers()["grpc-status"], "14");
        assert_eq!(frames.load(Ordering::SeqCst), 5);
    }
}
