//! Composition and authenticated request capture for idempotent route carriers.
//!
//! Key handling and request capture are the only policy this module owns. The
//! final contract layer authenticates first and inserts a sealed principal
//! before this carrier runs.

use std::convert::Infallible;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, OriginalUri, Request, State};
use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, Full};
use infra_idempotency_store::Store;
use utoipa::OpenApi as _;

use super::declaration::{self, CompositionError};
use super::execute::{Attempt, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, Provenance};
use super::identity::{self, KEY_GRAMMAR, MAX_KEY_BYTES};
use super::openapi::{IdempotencyComponents, KEY_HEADER, REPLAYED_HEADER, REPLAYED_VALUE};
use crate::authn::VerifiedPrincipal;
use crate::harden::RequestDeadline;
use crate::problem::{Code, Problem, body_too_large, body_unreadable, sanitized_internal_error};
use utoipa_axum::router::UtoipaMethodRouter;

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";

/// The generated contract and runtime composition owner for idempotent route
/// carriers.
#[derive(Debug)]
pub struct Composer {
    /// `None` for a composer that only renders the contract.
    store: Option<Store>,
    operations: usize,
}

/// Whether composition served any idempotent operation, and with what store.
#[derive(Debug)]
pub enum Activation {
    /// No idempotent operation needs a store or maintenance task.
    Inactive,
    /// The store must be checked and maintained before readiness admission.
    Active {
        /// The store the composed routes arbitrate through.
        store: Store,
        /// How many idempotent operations the composition serves.
        operations: NonZeroUsize,
    },
    /// Idempotent operations are served by a composer without a store, which
    /// startup must refuse.
    Unconfigured {
        /// How many idempotent operations the composition serves.
        operations: NonZeroUsize,
    },
}

impl Composer {
    /// A composer whose routes arbitrate through `store`.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store: Some(store),
            operations: 0,
        }
    }

    /// A composer without a store, for rendering the document and tests.
    /// Its routes answer a sanitized 500 and it performs no I/O.
    #[must_use]
    pub fn inert() -> Self {
        Self {
            store: None,
            operations: 0,
        }
    }

    /// Make one annotated route carrier idempotent and generate its served
    /// contract metadata.
    ///
    /// Key handling stays inside final authentication. An invalid declaration
    /// stops route assembly before the service can serve it.
    ///
    /// # Errors
    ///
    /// Returns [`CompositionError`] when this route's local contract cannot
    /// support idempotent execution and replay.
    pub fn route<S>(
        &mut self,
        routes: UtoipaMethodRouter<S>,
    ) -> Result<UtoipaMethodRouter<S>, CompositionError>
    where
        S: Clone + Send + Sync + 'static,
    {
        metrics::describe_counter!(
            HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Outcomes of requests to idempotent operations, by outcome."
        );
        let (schemas, mut paths, method_router) = routes;
        let operation_id = declaration::prepare(&mut paths)?;
        let state = AttemptState {
            store: self.store.clone(),
            operation_id: Arc::from(operation_id),
        };
        self.operations += 1;
        Ok((
            schemas,
            paths,
            method_router.route_layer(middleware::from_fn_with_state(state, run_attempt)),
        ))
    }

    /// Register the generated idempotency Problem components for the contract
    /// document merge.
    ///
    /// The target document must already include the `ProblemComponents`
    /// registration from [`crate::router()`]: the generated family references
    /// the shared `Problem` and `InvalidParam` schemas and the shared 413/500
    /// responses.
    #[must_use]
    pub fn components(&self) -> utoipa::openapi::OpenApi {
        IdempotencyComponents::openapi()
    }

    /// Select activation from successfully composed routes alone.
    #[must_use]
    pub fn finish(self) -> Activation {
        let Some(operations) = NonZeroUsize::new(self.operations) else {
            return Activation::Inactive;
        };
        match self.store {
            Some(store) => Activation::Active { store, operations },
            None => Activation::Unconfigured { operations },
        }
    }
}

/// Per-route middleware state for one idempotent operation.
#[derive(Clone)]
struct AttemptState {
    /// `None` under an inert composer, whose routes answer a wiring failure.
    store: Option<Store>,
    operation_id: Arc<str>,
}

/// Capture the attempt after final contract authentication and before
/// ordinary extraction, run the handler, then seal replay metadata.
async fn run_attempt(State(state): State<AttemptState>, request: Request, next: Next) -> Response {
    let operation_id = Arc::clone(&state.operation_id);
    match capture_attempt(state, request).await {
        ControlFlow::Continue(request) => {
            seal_replay_metadata(next.run(request).await, &operation_id)
        }
        ControlFlow::Break(refusal) => refusal,
    }
}

/// Insert the [`Attempt`] the handler's extractor takes and restore the
/// bounded body, or answer the refusal.
async fn capture_attempt(state: AttemptState, request: Request) -> ControlFlow<Response, Request> {
    let AttemptState {
        store,
        operation_id,
    } = state;
    let Some(store) = store else {
        return ControlFlow::Break(wiring_failure(&operation_id, "store_missing"));
    };
    let request = request.with_limited_body();
    let (mut parts, body) = request.into_parts();
    let Some(deadline) = parts.extensions.get::<RequestDeadline>().copied() else {
        return ControlFlow::Break(wiring_failure(&operation_id, "deadline_missing"));
    };
    let Ok(principal) = VerifiedPrincipal::from_request_parts(&mut parts, &()).await else {
        return ControlFlow::Break(wiring_failure(&operation_id, "principal_missing"));
    };
    let Some(caller) = identity::caller_identity(
        principal.issuer(),
        principal.subject(),
        principal.client_id(),
    ) else {
        return ControlFlow::Break(wiring_failure(&operation_id, "caller_missing"));
    };
    let Some(key) = identity::decode_key(
        parts
            .headers
            .get_all(KEY_HEADER)
            .iter()
            .map(HeaderValue::as_bytes),
    ) else {
        Outcome::InvalidKey.record();
        return ControlFlow::Break(invalid_key());
    };
    let Some(scope) = identity::scope_key(&caller, &key) else {
        return ControlFlow::Break(wiring_failure(&operation_id, "scope_unencodable"));
    };
    let uri = parts
        .extensions
        .get::<OriginalUri>()
        .map_or_else(|| parts.uri.clone(), |original| original.0.clone());
    let content_types = parts
        .headers
        .get_all(CONTENT_TYPE)
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect::<Vec<_>>();
    let collected = match body.collect().await {
        Ok(collected) => collected,
        Err(error) => {
            Outcome::NotStored.record();
            let problem = if caused_by_length_limit(&error) {
                body_too_large()
            } else {
                body_unreadable()
            };
            return ControlFlow::Break(problem.into_response());
        }
    };
    let trailers = collected.trailers().cloned();
    let body = collected.to_bytes();
    let Some(fingerprint) =
        identity::request_fingerprint(&parts.method, &uri, &content_types, &body)
    else {
        return ControlFlow::Break(wiring_failure(&operation_id, "fingerprint_unencodable"));
    };
    parts.extensions.insert(Attempt {
        store,
        scope,
        caller,
        fingerprint,
        operation_id,
        deadline,
    });
    ControlFlow::Continue(Request::from_parts(parts, restore_body(body, trailers)))
}

fn restore_body(body: Bytes, trailers: Option<axum::http::HeaderMap>) -> Body {
    match trailers {
        Some(trailers) => {
            Body::new(
                Full::new(body)
                    .with_trailers(std::future::ready(Some(Ok::<_, Infallible>(trailers)))),
            )
        }
        None => Body::from(body),
    }
}

fn invalid_key() -> Response {
    Problem::new(Code::BadRequest)
        .detail(INVALID_KEY_DETAIL)
        .invalid_param(
            format!("header.{KEY_HEADER}"),
            format!(
                "must be one Idempotency-Key field; {KEY_GRAMMAR} The decoded value must contain 1 to {MAX_KEY_BYTES} bytes"
            ),
        )
        .into_response()
}

fn seal_replay_metadata(mut response: Response, operation_id: &str) -> Response {
    // Seal replay metadata after the handler returns. Neither a supplied header
    // nor a 2xx status proves replay: a success must carry execute's private
    // provenance.
    let provenance = response.extensions_mut().remove::<Provenance>();
    response.headers_mut().remove(REPLAYED_HEADER);
    if !response.status().is_success() {
        return response;
    }
    match provenance {
        Some(Provenance::Executed) => response,
        Some(Provenance::Replayed) => {
            response
                .headers_mut()
                .insert(REPLAYED_HEADER, HeaderValue::from_static(REPLAYED_VALUE));
            response
        }
        None => wiring_failure(operation_id, "seam_unused"),
    }
}

fn wiring_failure(operation_id: &str, failure: &'static str) -> Response {
    tracing::error!(
        operation = operation_id,
        failure,
        "http_idempotency_wiring_failed"
    );
    sanitized_internal_error()
}

/// Whether a body read failed at a length limit. axum boxes the transport
/// limit's error into [`axum::Error`] when a request has no `Content-Length`,
/// so the limit can sit anywhere in the source chain. The inbound-webhooks
/// and http-idempotency profiles each keep a copy: a shared one in
/// `problem.rs` is dead code in a projection that drops both.
fn caused_by_length_limit(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(error), |&error| error.source())
        .any(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::http::{HeaderMap, HeaderValue};
    use http_body_util::BodyExt;

    use super::*;

    #[tokio::test]
    async fn body_restoration_preserves_bytes_and_collected_trailers() {
        let mut trailers = HeaderMap::new();
        trailers.insert("x-test-trailer", HeaderValue::from_static("kept"));
        let collected = restore_body(Bytes::from_static(b"raw bytes"), Some(trailers))
            .collect()
            .await
            .expect("body");
        assert_eq!(
            collected
                .trailers()
                .and_then(|headers| headers.get("x-test-trailer")),
            Some(&HeaderValue::from_static("kept"))
        );
        assert_eq!(collected.to_bytes(), Bytes::from_static(b"raw bytes"));
    }

    #[test]
    fn sealing_replay_metadata_after_the_handler_trusts_only_provenance() {
        let mut replay = Response::new(Body::empty());
        *replay.status_mut() = StatusCode::CREATED;
        replay
            .headers_mut()
            .insert(REPLAYED_HEADER, HeaderValue::from_static("forged"));
        let replay = super::super::execute::mark_provenance(replay, Provenance::Replayed);
        let replay = seal_replay_metadata(replay, "test");
        assert_eq!(
            replay.headers().get(REPLAYED_HEADER),
            Some(&HeaderValue::from_static("true"))
        );
        assert_eq!(replay.headers().get_all(REPLAYED_HEADER).iter().count(), 1);

        let mut executed = Response::new(Body::empty());
        *executed.status_mut() = StatusCode::CREATED;
        executed
            .headers_mut()
            .insert(REPLAYED_HEADER, HeaderValue::from_static("forged"));
        let executed = super::super::execute::mark_provenance(executed, Provenance::Executed);
        let executed = seal_replay_metadata(executed, "test");
        assert!(!executed.headers().contains_key(REPLAYED_HEADER));

        let mut failure = Response::new(Body::empty());
        *failure.status_mut() = StatusCode::BAD_REQUEST;
        failure
            .headers_mut()
            .insert(REPLAYED_HEADER, HeaderValue::from_static("forged"));
        let failure = seal_replay_metadata(failure, "test");
        assert_eq!(failure.status(), StatusCode::BAD_REQUEST);
        assert!(!failure.headers().contains_key(REPLAYED_HEADER));

        let missing = seal_replay_metadata(Response::new(Body::empty()), "test");
        assert_eq!(missing.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!missing.headers().contains_key(REPLAYED_HEADER));
    }
}
