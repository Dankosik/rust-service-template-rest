use std::ops::ControlFlow;
use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::on;
use http_body_util::Collected;
use infra_idempotency_store::{ScopeKey, Store};
use utoipa::{IntoResponses as _, OpenApi as _};

use super::declaration::{self, CompositionError};
use super::execute::{Attempt, FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome};
use super::identity::{self, KEY_GRAMMAR, MAX_DECODED_KEY_BYTES};
use super::openapi::IdempotencyComponents;
use super::{CommandInput, KEY_HEADER, ReplaySuccess as _};
use crate::authn::VerifiedPrincipal;
use crate::context::{RequestContext, RequestDeadline, collect_body};
use crate::problem::{
    Code, Problem, body_too_large, body_unreadable, caused_by_length_limit,
    sanitized_internal_error,
};
use crate::{DelegationCredential, HttpContract};

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";

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
    let mut paths = std::mem::take(&mut document.paths);
    let operation = declaration::prepare(
        &mut paths,
        document.external_docs.take(),
        C::Success::responses(),
    )?;
    metrics::describe_counter!(
        HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
        metrics::Unit::Count,
        "Idempotency capture refusals, handler refusals, and execution outcomes."
    );
    let method = operation.method;
    let operation_id: Arc<str> = Arc::from(operation.id.as_str());
    let fingerprint_headers: Arc<[HeaderName]> = operation.fingerprint_headers.into();
    let rules_link = operation.rules_link;
    let handler =
        move |State(state): State<S>, mut request: Request| {
            let operation_id = operation_id.clone();
            let fingerprint_headers = fingerprint_headers.clone();
            let rules_link = rules_link.clone();
            let prepare = prepare.clone();
            async move {
                let delegation = request.extensions_mut().remove::<DelegationCredential>();
                let (input, principal, attempt) =
                    match capture_attempt(operation_id, &fingerprint_headers, rules_link, request)
                        .await
                    {
                        ControlFlow::Continue(captured) => captured,
                        ControlFlow::Break(response) => return response,
                    };
                match prepare(input, principal, delegation, state).await {
                    Ok(command) => attempt.run(command).await,
                    Err(problem) => {
                        Outcome::NotStored.record();
                        problem.into_response()
                    }
                }
            }
        };
    let route = (C::Success::schemas(), paths, on(method, handler));
    HttpContract::from_idempotent(document, route)
        .and_then(|contract| {
            contract.with_components(
                IdempotencyComponents::openapi()
                    .components
                    .unwrap_or_default(),
            )
        })
        .map_err(|error| CompositionError::contract(&operation.id, error))
}

async fn capture_attempt(
    operation_id: Arc<str>,
    fingerprint_headers: &[HeaderName],
    rules_link: HeaderValue,
    request: Request,
) -> ControlFlow<Response, (CommandInput, VerifiedPrincipal, Attempt)> {
    let Some(store) = request.extensions().get::<Store>().cloned() else {
        return ControlFlow::Break(wiring_failure(&operation_id, "store_missing"));
    };
    let request = request.with_limited_body();
    let (parts, body) = request.into_parts();
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
    .map(|key| ScopeKey::new(caller, &key)) else {
        Outcome::InvalidKey.record();
        return ControlFlow::Break(invalid_key(rules_link));
    };
    let context = parts.extensions.get::<RequestContext>();
    let collected = match buffer_body(context, body).await {
        Ok(collected) => collected,
        Err(refusal) => {
            Outcome::NotStored.record();
            return ControlFlow::Break(refusal);
        }
    };
    let (input, fingerprint) = CommandInput::capture(parts, fingerprint_headers, collected);
    let attempt = Attempt {
        store,
        scope,
        fingerprint,
        operation_id,
        rules_link,
        deadline,
    };
    ControlFlow::Continue((input, principal, attempt))
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
        .detail(INVALID_KEY_DETAIL)
        .invalid_param(
            format!("header.{KEY_HEADER}"),
            format!(
                "Supply exactly one Idempotency-Key field. {KEY_GRAMMAR} The decoded value must contain 1 to {MAX_DECODED_KEY_BYTES} bytes."
            ),
        );
    ([(axum::http::header::LINK, rules_link)], problem).into_response()
}

fn wiring_failure(operation_id: &str, failure: &'static str) -> Response {
    tracing::error!(
        operation = operation_id,
        failure,
        "http_idempotency_wiring_failed"
    );
    sanitized_internal_error()
}
