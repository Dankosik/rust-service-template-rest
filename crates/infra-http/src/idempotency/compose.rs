use std::convert::Infallible;
use std::num::NonZeroUsize;
use std::ops::ControlFlow;
use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, Collected, Full};
use infra_idempotency_store::Store;
use utoipa::OpenApi as _;

use super::declaration::{self, CompositionError};
use super::execute::{
    Attempt, AttemptOutcome, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, Provenance,
};
use super::identity::{self, KEY_GRAMMAR, MAX_DECODED_KEY_BYTES};
use super::openapi::IdempotencyComponents;
use super::{KEY_HEADER, REPLAYED_HEADER, REPLAYED_VALUE, caused_by_length_limit};
use crate::authn::VerifiedPrincipal;
use crate::context::{RequestContext, RequestDeadline, read_body};
use crate::problem::{Code, Problem, body_too_large, body_unreadable, sanitized_internal_error};
use utoipa_axum::router::UtoipaMethodRouter;

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";

#[derive(Debug)]
pub struct Composer {
    store: Option<Store>,
    operations: usize,
}

#[derive(Debug)]
pub enum Activation {
    Inactive,
    /// The store must be checked and maintained before readiness admission.
    Active {
        store: Store,
        operations: NonZeroUsize,
    },
    /// Idempotent operations are served by a composer without a store, which
    /// startup must refuse.
    Unconfigured {
        operations: NonZeroUsize,
    },
}

impl Composer {
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

    /// Key handling stays inside final authentication, which finalization requires
    /// so capture receives a verified caller. Declare every header that can change
    /// the intended effect as an inline request parameter so its values
    /// participate in retry matching.
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
            "Idempotency capture refusals, handler refusals, and execution outcomes."
        );
        let (schemas, mut paths, method_router) = routes;
        let operation = declaration::prepare(&mut paths)?;
        let state = RouteState {
            store: self.store.clone(),
            operation_id: Arc::from(operation.id),
            fingerprint_headers: Arc::from(operation.fingerprint_headers),
        };
        self.operations += 1;
        Ok((
            schemas,
            paths,
            method_router.route_layer(middleware::from_fn_with_state(state, run_attempt)),
        ))
    }

    /// The target document must already include the `ProblemComponents`
    /// registration from [`crate::router()`]: the generated family references
    /// the shared [`Problem`] and [`crate::problem::InvalidParam`] schemas and
    /// the shared 413/500 responses.
    #[must_use]
    pub fn components(&self) -> utoipa::openapi::OpenApi {
        IdempotencyComponents::openapi()
    }

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

#[derive(Clone)]
struct RouteState {
    /// `None` under an inert composer, whose routes answer a wiring failure.
    store: Option<Store>,
    operation_id: Arc<str>,
    fingerprint_headers: Arc<[HeaderName]>,
}

/// Capture the attempt after final contract authentication and before
/// ordinary extraction, run the handler, then seal replay metadata.
async fn run_attempt(State(state): State<RouteState>, request: Request, next: Next) -> Response {
    let operation_id = Arc::clone(&state.operation_id);
    match capture_attempt(state, request).await {
        ControlFlow::Continue((request, outcome)) => {
            let response = next.run(request).await;
            if !response.status().is_success() {
                outcome.record(Outcome::NotStored);
            }
            seal_replay_metadata(response, &operation_id)
        }
        ControlFlow::Break(refusal) => refusal,
    }
}

async fn capture_attempt(
    state: RouteState,
    request: Request,
) -> ControlFlow<Response, (Request, AttemptOutcome)> {
    let RouteState {
        store,
        operation_id,
        fingerprint_headers,
    } = state;
    let Some(store) = store else {
        return ControlFlow::Break(wiring_failure(&operation_id, "store_missing"));
    };
    let request = request.with_limited_body();
    let (mut parts, body) = request.into_parts();
    let Some(deadline) = parts.extensions.get::<RequestDeadline>().copied() else {
        return ControlFlow::Break(wiring_failure(&operation_id, "deadline_missing"));
    };
    let Some(principal) = VerifiedPrincipal::from_extensions(&parts.extensions) else {
        return ControlFlow::Break(wiring_failure(&operation_id, "principal_missing"));
    };
    let Some(caller) = identity::caller_identity(
        principal.issuer(),
        principal.subject(),
        principal.client_id(),
    ) else {
        return ControlFlow::Break(wiring_failure(&operation_id, "caller_missing"));
    };
    let Some(scope) = identity::decode_key(
        parts
            .headers
            .get_all(KEY_HEADER)
            .iter()
            .map(HeaderValue::as_bytes),
    )
    .map(|key| identity::scope_key(&caller, &key)) else {
        Outcome::InvalidKey.record();
        return ControlFlow::Break(invalid_key());
    };
    let context = parts.extensions.get::<RequestContext>();
    let collected = match read_body(context, buffer_body(body)).await {
        Ok(collected) => collected,
        Err(refusal) => {
            Outcome::NotStored.record();
            return ControlFlow::Break(refusal);
        }
    };
    let trailers = collected.trailers().cloned();
    let body = collected.to_bytes();
    let uri = parts
        .extensions
        .get::<OriginalUri>()
        .map_or(&parts.uri, |original| &original.0);
    let fingerprint = identity::request_fingerprint(
        &parts.method,
        uri,
        &parts.headers,
        &fingerprint_headers,
        &body,
        trailers.as_ref(),
    );
    let outcome = AttemptOutcome::default();
    parts.extensions.insert(Attempt {
        store,
        scope,
        caller,
        fingerprint,
        operation_id,
        deadline,
        outcome: outcome.clone(),
    });
    ControlFlow::Continue((
        Request::from_parts(parts, restore_body(body, trailers)),
        outcome,
    ))
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the middleware returns"
)]
async fn buffer_body(body: Body) -> Result<Collected<Bytes>, Response> {
    body.collect().await.map_err(|error| {
        let problem = if caused_by_length_limit(&error) {
            body_too_large()
        } else {
            body_unreadable()
        };
        problem.into_response()
    })
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
                "must be one Idempotency-Key field; {KEY_GRAMMAR} The decoded value must contain 1 to {MAX_DECODED_KEY_BYTES} bytes"
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
        let replay = (StatusCode::CREATED, [(REPLAYED_HEADER, "forged")]).into_response();
        let replay = super::super::execute::mark_provenance(replay, Provenance::Replayed);
        let replay = seal_replay_metadata(replay, "test");
        assert_eq!(
            replay.headers().get(REPLAYED_HEADER),
            Some(&HeaderValue::from_static("true"))
        );
        assert_eq!(replay.headers().get_all(REPLAYED_HEADER).iter().count(), 1);

        let executed = (StatusCode::CREATED, [(REPLAYED_HEADER, "forged")]).into_response();
        let executed = super::super::execute::mark_provenance(executed, Provenance::Executed);
        let executed = seal_replay_metadata(executed, "test");
        assert!(!executed.headers().contains_key(REPLAYED_HEADER));

        let failure = (StatusCode::BAD_REQUEST, [(REPLAYED_HEADER, "forged")]).into_response();
        let failure = seal_replay_metadata(failure, "test");
        assert_eq!(failure.status(), StatusCode::BAD_REQUEST);
        assert!(!failure.headers().contains_key(REPLAYED_HEADER));

        let missing = seal_replay_metadata(Response::new(Body::empty()), "test");
        assert_eq!(missing.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!missing.headers().contains_key(REPLAYED_HEADER));
    }
}
