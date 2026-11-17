use std::convert::Infallible;
use std::ops::ControlFlow;
use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{FromRequest, OriginalUri, Request, State};
use axum::http::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::on;
use http_body_util::{BodyExt, Collected, Full};
use infra_idempotency_store::Store;
use utoipa::OpenApi as _;

use super::KEY_HEADER;
use super::declaration::{self, CompositionError};
use super::execute::{Attempt, FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, refusal};
use super::identity::{self, KEY_GRAMMAR, MAX_DECODED_KEY_BYTES};
use super::openapi::IdempotencyComponents;
use crate::HttpContract;
use crate::authn::VerifiedPrincipal;
use crate::context::{RequestContext, RequestDeadline, collect_body};
use crate::problem::{
    Code, Problem, body_too_large, body_unreadable, caused_by_length_limit,
    sanitized_internal_error,
};
use utoipa_axum::router::OpenApiRouter;

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";

pub fn route<S, I, M, C, F>(
    mut document: utoipa::openapi::OpenApi,
    prepare: impl Fn(I) -> F + Clone + Send + Sync + 'static,
) -> Result<HttpContract<S>, CompositionError>
where
    S: Clone + Send + Sync + 'static,
    I: FromRequest<S, M> + Send + 'static,
    M: 'static,
    C: FreshCommand + 'static,
    F: Future<Output = Result<C, Box<Problem>>> + Send + 'static,
{
    let mut paths = std::mem::take(&mut document.paths);
    let operation = declaration::prepare(&mut paths)?;
    metrics::describe_counter!(
        HTTP_IDEMPOTENCY_OUTCOMES_METRIC,
        metrics::Unit::Count,
        "Idempotency capture refusals, handler refusals, and execution outcomes."
    );
    let method = operation.method;
    let operation_id: Arc<str> = operation.id.into();
    let fingerprint_headers: Arc<[HeaderName]> = operation.fingerprint_headers.into();
    let handler = move |State(state): State<S>, request: Request| {
        let operation_id = operation_id.clone();
        let fingerprint_headers = fingerprint_headers.clone();
        let prepare = prepare.clone();
        async move {
            let (request, attempt) =
                match capture_attempt(operation_id, &fingerprint_headers, request).await {
                    ControlFlow::Continue(captured) => captured,
                    ControlFlow::Break(response) => return response,
                };
            let input = match I::from_request(request, &state).await {
                Ok(input) => input,
                Err(rejected) => {
                    Outcome::NotStored.record();
                    return refusal(rejected.into_response());
                }
            };
            match prepare(input).await {
                Ok(command) => attempt.run(command).await,
                Err(problem) => {
                    Outcome::NotStored.record();
                    problem.into_response()
                }
            }
        }
    };
    document.merge(IdempotencyComponents::openapi());
    let routes =
        OpenApiRouter::with_openapi(document).routes((Vec::new(), paths, on(method, handler)));
    Ok(HttpContract::from_idempotent(routes))
}

async fn capture_attempt(
    operation_id: Arc<str>,
    fingerprint_headers: &[HeaderName],
    request: Request,
) -> ControlFlow<Response, (Request, Attempt)> {
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
    .map(|key| identity::scope_key(&caller, &key)) else {
        Outcome::InvalidKey.record();
        return ControlFlow::Break(invalid_key());
    };
    let context = parts.extensions.get::<RequestContext>();
    let collected = match buffer_body(context, body).await {
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
        fingerprint_headers,
        &body,
        trailers.as_ref(),
    );
    let attempt = Attempt {
        store,
        scope,
        caller,
        fingerprint,
        operation_id,
        deadline,
    };
    ControlFlow::Continue((
        Request::from_parts(parts, restore_body(body, trailers)),
        attempt,
    ))
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
    use axum::http::HeaderMap;

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
}
