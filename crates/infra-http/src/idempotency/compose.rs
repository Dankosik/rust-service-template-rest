use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use axum::routing::on;
use compact_str::CompactString;
use http_body_util::Collected;
use infra_idempotency_store::{ScopeKey, Store};
use utoipa::{IntoResponses as _, OpenApi as _};

use super::declaration::{self, CompositionError};
use super::execute::{
    Attempt, FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, RouteState,
};
use super::identity::{self, KEY_GRAMMAR, MAX_DECODED_KEY_BYTES};
use super::openapi::IdempotencyComponents;
use super::{CommandInput, KEY_HEADER, KEY_HEADER_NAME, ReplaySuccess as _};
use crate::authn::VerifiedPrincipal;
use crate::context::{RequestContext, RequestDeadline, collect_body};
use crate::problem::{
    Code, Problem, body_too_large, body_unreadable, caused_by_length_limit,
    sanitized_internal_error,
};
use crate::{DelegationCredential, HttpContract};

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const INVALID_KEY_PARAM_NAME: &str = const_format::formatcp!("header.{KEY_HEADER}");
const INVALID_KEY_PARAM_REASON: &str = const_format::formatcp!(
    "Supply exactly one Idempotency-Key field. {KEY_GRAMMAR} The decoded value must contain 1 to {MAX_DECODED_KEY_BYTES} bytes."
);

pub fn route<S, C, F>(
    mut document: utoipa::openapi::OpenApi,
    prepare: impl Fn(CommandInput, VerifiedPrincipal, Option<DelegationCredential>, S) -> F
    + Clone
    + Send
    + Sync
    + 'static,
) -> Result<HttpContract<S>, CompositionError>
where
    S: Clone + Send + Sync + 'static,
    C: FreshCommand + 'static,
    F: Future<Output = Result<C, Box<Problem>>> + Send + 'static,
{
    let operation = declaration::prepare(
        &mut document,
        C::Success::responses(),
        C::Success::schemas(),
    )?;
    metrics::describe_counter!(
        HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
        metrics::Unit::Count,
        "Idempotency capture refusals, handler refusals, and execution outcomes."
    );
    let method = operation.method;
    let route_state = Arc::new(RouteState::new(
        operation.id,
        operation.fingerprint_headers,
        operation.rules_link,
    ));
    let handler_route = Arc::clone(&route_state);
    let handler = move |State(state): State<S>, mut request: Request| {
        let prepare = prepare.clone();
        async move {
            let delegation = request.extensions_mut().remove::<DelegationCredential>();
            let (input, principal, attempt) = match capture_attempt(handler_route, request).await {
                Ok(captured) => captured,
                Err(response) => return response,
            };
            match prepare(input, principal, delegation, state).await {
                Ok(command) => attempt.run(command).await,
                Err(problem) => {
                    attempt.route.record(Outcome::NotStored);
                    problem.into_response()
                }
            }
        }
    };
    let route = (
        Vec::new(),
        std::mem::take(&mut document.paths),
        on(method, handler),
    );
    HttpContract::from_idempotent(document, route)
        .and_then(|contract| {
            contract.with_components(
                IdempotencyComponents::openapi()
                    .components
                    .unwrap_or_default(),
            )
        })
        .map_err(|error| CompositionError::contract(&route_state.operation_id, error))
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the middleware returns"
)]
async fn capture_attempt(
    route: Arc<RouteState>,
    mut request: Request,
) -> Result<(CommandInput, VerifiedPrincipal, Attempt), Response> {
    let store = request
        .extensions_mut()
        .remove::<Store>()
        .ok_or_else(|| wiring_failure(&route.operation_id, "store_missing"))?;
    let request = request.with_limited_body();
    let (parts, body) = request.into_parts();
    let deadline = parts
        .extensions
        .get::<RequestDeadline>()
        .copied()
        .ok_or_else(|| wiring_failure(&route.operation_id, "deadline_missing"))?;
    let principal = VerifiedPrincipal::from_extensions(&parts.extensions)
        .ok_or_else(|| wiring_failure(&route.operation_id, "principal_missing"))?;
    let caller = identity::caller_identity(
        principal.issuer(),
        principal.subject(),
        principal.client_id(),
    )
    .ok_or_else(|| wiring_failure(&route.operation_id, "caller_missing"))?;
    let Some(scope) = identity::decode_key(
        parts
            .headers
            .get_all(&KEY_HEADER_NAME)
            .iter()
            .map(HeaderValue::as_bytes),
    )
    .map(|key| ScopeKey::new(caller, &key)) else {
        route.record(Outcome::InvalidKey);
        return Err(invalid_key(route.rules_link.clone()));
    };
    let context = parts.extensions.get::<RequestContext>();
    let collected = buffer_body(context, body)
        .await
        .inspect_err(|_| route.record(Outcome::NotStored))?;
    let (input, fingerprint) = CommandInput::capture(parts, &route.fingerprint_headers, collected);
    let attempt = Attempt {
        store,
        scope,
        fingerprint,
        route,
        deadline,
    };
    Ok((input, principal, attempt))
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the middleware returns"
)]
async fn buffer_body(
    context: Option<&RequestContext>,
    body: Body,
) -> Result<Collected<Bytes>, Response> {
    collect_body(context, body).await.map_err(|error| {
        let problem = if caused_by_length_limit(&error) {
            body_too_large()
        } else {
            body_unreadable()
        };
        problem.into_response()
    })
}

fn invalid_key(rules_link: HeaderValue) -> Response {
    let problem = Problem::new(Code::BadRequest)
        .detail_static(INVALID_KEY_DETAIL)
        .invalid_param(
            CompactString::const_new(INVALID_KEY_PARAM_NAME),
            CompactString::const_new(INVALID_KEY_PARAM_REASON),
        );
    ([(axum::http::header::LINK, rules_link)], problem).into_response()
}

pub(super) fn wiring_failure(operation_id: &str, failure: &'static str) -> Response {
    tracing::error!(
        operation = operation_id,
        failure,
        "http_idempotency_wiring_failed"
    );
    sanitized_internal_error()
}
