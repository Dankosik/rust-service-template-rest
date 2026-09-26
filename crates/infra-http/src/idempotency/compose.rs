//! Composition and authenticated request capture for idempotent route carriers.
//!
//! Key handling and request capture are the only policy this module owns. The
//! final contract layer authenticates first and inserts a sealed principal
//! before this carrier runs.

use std::convert::Infallible;
use std::error::Error as StdError;
use std::num::NonZeroUsize;
use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, OriginalUri, Request, State};
use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, Full, LengthLimitError};
use infra_idempotency_store::Store;
use utoipa::OpenApi as _;

use super::declaration::{self, CompositionError, Rule};
use super::execute::{Attempt, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, Provenance, sanitized};
use super::identity;
use super::openapi::{IdempotencyComponents, KEY_HEADER, REPLAYED_HEADER};
use crate::authn::VerifiedPrincipal;
use crate::contract::RegisteredRoutes;
use crate::harden::RequestDeadline;
use crate::problem::{Code, Problem};
use crate::request_id;

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const INVALID_KEY_REASON: &str =
    "must be one Idempotency-Key field of 1 to 255 decoded visible-ASCII bytes";
const BODY_READ_DETAIL: &str = "request body could not be read";
const BODY_LIMIT_DETAIL: &str = "request body exceeds the configured limit";

/// The generated contract and runtime composition owner for idempotent route
/// carriers.
#[derive(Debug)]
pub struct Composer {
    store: Store,
    operations: usize,
}

/// Whether composition served any idempotent operation.
#[derive(Debug)]
pub enum Activation {
    /// No idempotent operation needs a store or maintenance task.
    Inactive,
    /// The store must be checked and maintained before readiness admission.
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
            operations: 0,
        }
    }

    /// A composer over an inert store for rendering the document and tests.
    /// It performs no I/O.
    #[must_use]
    pub fn inert() -> Self {
        Self::new(Store::inert())
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
        mut routes: RegisteredRoutes<S>,
    ) -> Result<RegisteredRoutes<S>, CompositionError>
    where
        S: Clone + Send + Sync + 'static,
    {
        metrics::describe_counter!(
            HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
            metrics::Unit::Count,
            "Outcomes of requests to idempotent operations, by outcome."
        );
        let operation = routes
            .documented_paths_mut()
            .ok_or_else(|| CompositionError::new("registered route", Rule::Shape))
            .and_then(declaration::prepare);
        let operation = operation?;
        let keys = KeyLayer {
            store: self.store.clone(),
            operation: Arc::from(operation),
        };
        self.operations += 1;
        Ok(routes.map_method_routers(|method_router| {
            let keys = keys.clone();
            method_router.route_layer(middleware::from_fn_with_state(keys, handle_key))
        }))
    }

    /// Register the generated idempotency Problem components for the contract
    /// document merge.
    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "response components enter only through the composer-owned contract path"
    )]
    pub fn components(&self) -> utoipa::openapi::OpenApi {
        IdempotencyComponents::openapi()
    }

    /// Select activation from successfully composed routes alone.
    #[must_use]
    pub fn finish(self) -> Activation {
        match NonZeroUsize::new(self.operations) {
            Some(operations) => Activation::Active {
                store: self.store,
                operations,
            },
            None => Activation::Inactive,
        }
    }
}

#[derive(Clone)]
struct KeyLayer {
    store: Store,
    operation: Arc<str>,
}

/// Capture identity after final contract authentication and before ordinary
/// extraction.
async fn handle_key(State(keys): State<KeyLayer>, request: Request, next: Next) -> Response {
    let request_id = request_id::request_id(request.extensions());
    let request = request.with_limited_body();
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
    let Some(caller) = identity::caller_identity(
        principal.issuer(),
        principal.subject(),
        principal.client_id(),
    ) else {
        return wiring_failure(&keys.operation, "caller_missing", request_id);
    };
    let Some(key) = identity::valid_key(
        parts
            .headers
            .get_all(KEY_HEADER)
            .iter()
            .map(axum::http::HeaderValue::as_bytes),
    ) else {
        Outcome::InvalidKey.record();
        return invalid_key(request_id);
    };
    let Some(scope) = identity::scope_key(&caller, &key) else {
        return wiring_failure(&keys.operation, "scope_unencodable", request_id);
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
            return if caused_by_length_limit(&error) {
                payload_too_large(request_id)
            } else {
                unreadable_body(request_id)
            };
        }
    };
    let trailers = collected.trailers().cloned();
    let body = collected.to_bytes();
    let Some(fingerprint) = identity::request_digest(&parts.method, &uri, &content_types, &body)
    else {
        return wiring_failure(&keys.operation, "fingerprint_unencodable", request_id);
    };
    let operation = Arc::clone(&keys.operation);
    parts.extensions.insert(Attempt {
        store: keys.store,
        scope,
        caller,
        fingerprint,
        operation: keys.operation,
        deadline,
        request_id: request_id.clone(),
    });
    let response = next
        .run(Request::from_parts(parts, restore_body(body, trailers)))
        .await;
    normalize_response(response, &operation, request_id)
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

fn caused_by_length_limit(error: &axum::Error) -> bool {
    let mut current: &(dyn StdError + 'static) = error;
    loop {
        if current.is::<LengthLimitError>() {
            return true;
        }
        let Some(source) = current.source() else {
            return false;
        };
        current = source;
    }
}

fn invalid_key(request_id: Option<String>) -> Response {
    Problem::new(Code::BadRequest)
        .detail(INVALID_KEY_DETAIL)
        .invalid_param(format!("header.{KEY_HEADER}"), INVALID_KEY_REASON)
        .request_id(request_id)
        .into_response()
}

fn payload_too_large(request_id: Option<String>) -> Response {
    Problem::new(Code::RequestEntityTooLarge)
        .detail(BODY_LIMIT_DETAIL)
        .request_id(request_id)
        .into_response()
}

fn unreadable_body(request_id: Option<String>) -> Response {
    Problem::new(Code::BadRequest)
        .detail(BODY_READ_DETAIL)
        .request_id(request_id)
        .into_response()
}

fn normalize_response(
    mut response: Response,
    operation: &str,
    request_id: Option<String>,
) -> Response {
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
                .insert(REPLAYED_HEADER, HeaderValue::from_static("true"));
            response
        }
        None => wiring_failure(operation, "seam_unused", request_id),
    }
}

fn wiring_failure(operation: &str, failure: &'static str, request_id: Option<String>) -> Response {
    tracing::error!(operation, failure, "http_idempotency_wiring_failed");
    sanitized(request_id)
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

    #[tokio::test]
    async fn invalid_key_is_sanitized_and_never_echoes_input() {
        let body = invalid_key(Some("req-1".to_owned()))
            .into_body()
            .collect()
            .await
            .expect("problem")
            .to_bytes();
        let problem: serde_json::Value = serde_json::from_slice(&body).expect("problem JSON");
        assert_eq!(problem["code"], "bad_request");
        assert!(problem.to_string().contains(INVALID_KEY_REASON));
        assert!(!problem.to_string().contains("submitted-secret"));
    }

    #[test]
    fn response_normalization_seals_replay_metadata_after_the_handler() {
        let mut replay = Response::new(Body::empty());
        *replay.status_mut() = StatusCode::CREATED;
        replay
            .headers_mut()
            .insert(REPLAYED_HEADER, HeaderValue::from_static("forged"));
        let replay = super::super::execute::mark_provenance(replay, Provenance::Replayed);
        let replay = normalize_response(replay, "test", None);
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
        let executed = normalize_response(executed, "test", None);
        assert!(!executed.headers().contains_key(REPLAYED_HEADER));

        let mut failure = Response::new(Body::empty());
        *failure.status_mut() = StatusCode::BAD_REQUEST;
        failure
            .headers_mut()
            .insert(REPLAYED_HEADER, HeaderValue::from_static("forged"));
        let failure = normalize_response(failure, "test", None);
        assert_eq!(failure.status(), StatusCode::BAD_REQUEST);
        assert!(!failure.headers().contains_key(REPLAYED_HEADER));

        let missing = normalize_response(Response::new(Body::empty()), "test", None);
        assert_eq!(missing.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!missing.headers().contains_key(REPLAYED_HEADER));
    }
}
