//! HTTP transport adapter.
//!
//! Owns the hardened middleware chain, the problem envelope, the health
//! probe handlers with their OpenAPI contract, and a bounded server over
//! hyper. It does not own business rules, configuration loading, or process
//! lifecycle; the composition root in the `service` crate wires those
//! together and assembles the full API document.
//!
//! Most of the chain is `tower-http`; the template-owned pieces and the
//! contract decisions are recorded in `docs/architecture/http.md`.

pub mod contract;
pub mod problem;
// template:begin inbound-webhooks:http-webhooks-module
pub mod webhooks;
// template:end inbound-webhooks:http-webhooks-module
// template:begin authn:infra-http-authn-module
pub mod authn;
// template:end authn:infra-http-authn-module
// template:begin http-idempotency:infra-http-idempotency-module
pub mod idempotency;
// template:end http-idempotency:infra-http-idempotency-module

mod access_log;
mod harden;
mod probes;
mod request_id;
mod router;
mod server;

pub use contract::{ContractRouter, FinalizeError, RegisteredRoutes, RouteMethod};
pub use harden::{
    HTTP_METRICS_NAMES, HTTP_REQUESTS_DURATION_SECONDS, HardenOptions, SHED_REQUESTS_METRIC, harden,
};
// template:begin authn:infra-http-authn-exports
pub use authn::{AUTHN_VERIFICATIONS_METRIC, VerifiedPrincipal, require_scope};
// template:end authn:infra-http-authn-exports
// template:begin request-budget:infra-http-request-deadline-export
pub use harden::RequestDeadline;
// template:end request-budget:infra-http-request-deadline-export
pub use problem::{Code, InvalidParam, Problem};
pub use request_id::{REQUEST_ID_HEADER, request_id};
pub use router::router;
pub use server::{CONNECTIONS_REFUSED_METRIC, Drained, Server, ServerError, ServerOptions};

#[doc(hidden)]
pub use utoipa_axum::routes as __utoipa_routes;

/// Register annotated handlers through the tracked contract carrier.
///
/// The underlying Utoipa macro still owns annotation and schema extraction;
/// this wrapper supplies the same handler to the carrier so it constructs the
/// served methods from that metadata rather than accepting Utoipa's opaque
/// method router.
#[macro_export]
macro_rules! routes {
    ($handler:path $(,)?) => {
        $crate::RegisteredRoutes::documented($handler, $crate::__utoipa_routes!($handler))
    };
    ($head:path, $($tail:path),+ $(,)?) => {{
        let routes = $crate::routes!($head);
        $(
            let routes = routes.merge($crate::routes!($tail));
        )+
        routes
    }};
}
