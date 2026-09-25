//! Declaration checks and composition for idempotent route carriers.
//!
//! Key handling is the only policy this module owns. The final contract layer
//! authenticates first and inserts a sealed principal before this carrier runs.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{FromRequestParts, Request, State};
use axum::http::HeaderValue;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use infra_idempotency_store::Store;
use utoipa::OpenApi as _;

use super::declaration::{self, AgreementError, Rule};
use super::execute::{Attempt, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, sanitized};
use super::identity::{self, Caller};
use super::openapi::{IdempotencyComponents, KEY_HEADER};
use crate::authn::VerifiedPrincipal;
use crate::contract::RegisteredRoutes;
use crate::harden::RequestDeadline;
use crate::problem::{Code, Problem};
use crate::request_id;

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const INVALID_KEY_REASON: &str =
    "must be one Idempotency-Key field of 1 to 255 RFC 9110 token characters";

/// Declaration checks and composition for idempotent route carriers.
#[derive(Debug)]
pub struct Composer {
    store: Store,
    composed: BTreeSet<String>,
    failures: Vec<AgreementError>,
}

/// Whether the boundary serves any idempotent operation.
#[derive(Debug)]
pub enum Activation {
    /// No operation is composed: the boundary makes no database query,
    /// starts no task, and requires no configuration value.
    Inactive,
    /// At least one operation is composed; the store must pass its startup
    /// check before readiness admission.
    #[non_exhaustive]
    Active {
        store: Store,
        operations: NonZeroUsize,
    },
}

impl Composer {
    /// A composer whose routes arbitrate through `store`.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            composed: BTreeSet::new(),
            failures: Vec::new(),
        }
    }

    /// A composer over an inert store for rendering the document and tests.
    /// It performs no I/O.
    #[must_use]
    pub fn inert() -> Self {
        Self::new(Store::inert())
    }

    /// Compose one idempotent annotated carrier.
    ///
    /// Key handling stays inside final authentication. A carrier that breaks a
    /// declaration rule remains fail-closed as sanitized 500 endpoints and
    /// [`Composer::agree`] reports the declaration error before admission.
    pub fn route<S>(&mut self, routes: RegisteredRoutes<S>) -> RegisteredRoutes<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        metrics::describe_counter!(
            HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Outcomes of requests to idempotent operations, by outcome."
        );
        let operation = routes
            .documented_paths()
            .ok_or_else(|| AgreementError::new("registered route", Rule::Shape))
            .and_then(declaration::check_route);
        let operation = match operation {
            Ok(operation) => operation,
            Err(failure) => {
                self.failures.push(failure);
                return refuse(routes);
            }
        };
        let keys = KeyLayer {
            store: self.store.clone(),
            operation: Arc::from(operation.as_str()),
        };
        self.composed.insert(operation);
        routes.map_method_routers(|method_router| {
            let keys = keys.clone();
            method_router.route_layer(middleware::from_fn_with_state(keys, handle_key))
        })
    }

    /// The idempotency response components for the contract document merge.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "the family registers through the composer the contract receives, its only path"
    )]
    pub fn components(&self) -> utoipa::openapi::OpenApi {
        IdempotencyComponents::openapi()
    }

    /// Check the assembled document against the composed routes.
    ///
    /// # Errors
    ///
    /// Returns [`AgreementError`] when a composed route or the final document
    /// breaks an idempotency declaration rule.
    pub fn agree(self, document: &utoipa::openapi::OpenApi) -> Result<Activation, AgreementError> {
        if let Some(failure) = self.failures.into_iter().next() {
            return Err(failure);
        }
        declaration::agree(document, &self.composed)?;
        Ok(match NonZeroUsize::new(self.composed.len()) {
            Some(operations) => Activation::Active {
                store: self.store,
                operations,
            },
            None => Activation::Inactive,
        })
    }
}

/// The fail-closed form of a carrier that broke a declaration rule.
fn refuse<S>(routes: RegisteredRoutes<S>) -> RegisteredRoutes<S>
where
    S: Clone + Send + Sync + 'static,
{
    routes.map_method_routers(|method_router| {
        method_router.route_layer(middleware::from_fn(|request: Request, _: Next| {
            std::future::ready(sanitized(request_id::request_id(request.extensions())))
        }))
    })
}

#[derive(Clone)]
struct KeyLayer {
    store: Store,
    operation: Arc<str>,
}

/// Key handling inside authentication: the verified caller and request budget,
/// key grammar, and handler attempt. A success that never enters the seam is
/// refused.
async fn handle_key(State(keys): State<KeyLayer>, request: Request, next: Next) -> Response {
    let request_id = request_id::request_id(request.extensions());
    let (mut parts, body) = request.into_parts();
    let Some(deadline) = parts
        .extensions
        .get::<RequestDeadline>()
        .map(RequestDeadline::at)
    else {
        return wiring_failure(&keys.operation, "deadline_missing", request_id);
    };
    let Ok(principal) = VerifiedPrincipal::from_request_parts(&mut parts, &()).await else {
        return wiring_failure(&keys.operation, "principal_missing", request_id);
    };
    let Some(caller) = Caller::of(principal.subject(), principal.client_id()) else {
        return wiring_failure(&keys.operation, "caller_missing", request_id);
    };
    let Some(key) = identity::valid_key(
        parts
            .headers
            .get_all(KEY_HEADER)
            .iter()
            .map(HeaderValue::as_bytes),
    ) else {
        Outcome::InvalidKey.record();
        return invalid_key(request_id);
    };
    let Some(scope) = identity::scope_key(principal.issuer(), caller, &keys.operation, key) else {
        return wiring_failure(&keys.operation, "scope_unencodable", request_id);
    };
    let seam_used = Arc::new(AtomicBool::new(false));
    let operation = Arc::clone(&keys.operation);
    parts.extensions.insert(Attempt {
        store: keys.store,
        scope,
        operation: keys.operation,
        deadline,
        request_id: request_id.clone(),
        seam_used: Arc::clone(&seam_used),
    });
    let response = next.run(Request::from_parts(parts, body)).await;
    guard_wiring(
        response,
        seam_used.load(Ordering::Relaxed),
        &operation,
        request_id,
    )
}

/// The 400 for an invalid key: a fixed detail and one `invalid_params` entry
/// that names the rule and never echoes the value.
fn invalid_key(request_id: Option<String>) -> Response {
    Problem::new(Code::BadRequest)
        .detail(INVALID_KEY_DETAIL)
        .invalid_param(format!("header.{KEY_HEADER}"), INVALID_KEY_REASON)
        .request_id(request_id)
        .into_response()
}

/// A handler that never entered the seam cannot return a success.
fn guard_wiring(
    response: Response,
    seam_used: bool,
    operation: &str,
    request_id: Option<String>,
) -> Response {
    if seam_used || !response.status().is_success() {
        return response;
    }
    wiring_failure(operation, "seam_unused", request_id)
}

fn wiring_failure(operation: &str, failure: &'static str, request_id: Option<String>) -> Response {
    tracing::error!(operation, failure, "http_idempotency_wiring_failed");
    sanitized(request_id)
}
