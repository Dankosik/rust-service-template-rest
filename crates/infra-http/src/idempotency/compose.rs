use std::sync::Arc;

use axum::RequestExt;
use axum::body::{Body, Bytes};
use axum::extract::{FromRef, Request, State};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use axum::routing::on;
use compact_str::CompactString;
use http_body_util::Collected;
use infra_idempotency_store::{ScopeKey, Store};
use serde::de::DeserializeOwned;
use utoipa::{IntoResponses as _, OpenApi as _};

use super::declaration;
use super::execute::{
    Attempt, FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Outcome, RouteState,
};
use super::identity::{self, KEY_GRAMMAR, MAX_DECODED_KEY_BYTES};
use super::openapi::IdempotencyComponents;
use super::{CommandInput, KEY_HEADER, KEY_HEADER_NAME, ReplaySuccess as _};
use crate::context::{RequestContext, collect_body};
use crate::extract::{Json, Path, Query};
use crate::problem::{
    Code, Problem, body_too_large, body_unreadable, caused_by_length_limit,
    sanitized_internal_error,
};
use crate::{
    DelegationCredential, FinalizeError, HttpContract, Principal, VerifiedDelegation,
    VerifiedPrincipal,
};

const INVALID_KEY_DETAIL: &str = "Idempotency-Key is missing or invalid";
const INVALID_KEY_PARAM_NAME: &str = const_format::formatcp!("header.{KEY_HEADER}");
const INVALID_KEY_PARAM_REASON: &str = const_format::formatcp!(
    "Supply exactly one Idempotency-Key field. {KEY_GRAMMAR} The decoded value must contain 1 to {MAX_DECODED_KEY_BYTES} bytes."
);

mod sealed {
    use crate::{DelegationCredential, Principal, Problem};

    use super::{CommandInput, FreshCommand};

    #[derive(Debug)]
    pub enum ViaPart {}

    #[derive(Debug)]
    pub enum ViaCommand {}

    #[derive(Debug)]
    pub struct ExtractContext<'a, S> {
        pub(super) principal: &'a Principal,
        pub(super) delegation: Option<&'a DelegationCredential>,
        pub(super) state: &'a S,
    }

    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot be extracted as an idempotent command part",
        note = "supported extractors are `State<T>`, `Principal`, `VerifiedPrincipal`, `VerifiedDelegation`, `Option<DelegationCredential>`, `Path<T>`, `Query<T>`, `Json<T>`, and `CommandInput` (last argument only)"
    )]
    pub trait FromCommandPart<S>: Sized {
        fn extract_part(
            input: &mut CommandInput,
            cx: &ExtractContext<'_, S>,
        ) -> impl Future<Output = Result<Self, Problem>> + Send;
    }

    #[diagnostic::on_unimplemented(
        message = "`{Self}` cannot be extracted by an idempotent command handler",
        note = "supported extractors are `State<T>`, `Principal`, `VerifiedPrincipal`, `VerifiedDelegation`, `Option<DelegationCredential>`, `Path<T>`, `Query<T>`, `Json<T>`, and `CommandInput` (last argument only)"
    )]
    pub trait FromCommand<S, M>: Sized {
        fn extract(
            input: CommandInput,
            cx: &ExtractContext<'_, S>,
        ) -> impl Future<Output = Result<Self, Problem>> + Send;
    }

    #[diagnostic::on_unimplemented(message = "`{Self}` is not a valid idempotent command handler")]
    pub trait CommandHandler<Args, S>: Clone + Send + Sync + 'static {
        type Command: FreshCommand + 'static;

        fn call(
            self,
            input: CommandInput,
            cx: &ExtractContext<'_, S>,
        ) -> impl Future<Output = Result<Self::Command, Problem>> + Send;
    }
}

#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be extracted as an idempotent command part",
    note = "supported extractors are `State<T>`, `Principal`, `VerifiedPrincipal`, `VerifiedDelegation`, `Option<DelegationCredential>`, `Path<T>`, `Query<T>`, `Json<T>`, and `CommandInput` (last argument only)"
)]
pub trait FromCommandPart<S>: sealed::FromCommandPart<S> {}

impl<S, T: sealed::FromCommandPart<S>> FromCommandPart<S> for T {}

#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be extracted by an idempotent command handler",
    note = "supported extractors are `State<T>`, `Principal`, `VerifiedPrincipal`, `VerifiedDelegation`, `Option<DelegationCredential>`, `Path<T>`, `Query<T>`, `Json<T>`, and `CommandInput` (last argument only)"
)]
pub trait FromCommand<S, M = sealed::ViaCommand>: sealed::FromCommand<S, M> {}

impl<S, M, T: sealed::FromCommand<S, M>> FromCommand<S, M> for T {}

#[diagnostic::on_unimplemented(message = "`{Self}` is not a valid idempotent command handler")]
pub trait CommandHandler<Args, S>: sealed::CommandHandler<Args, S> {}

impl<Args, S, H: sealed::CommandHandler<Args, S>> CommandHandler<Args, S> for H {}

impl<S, Sub> sealed::FromCommandPart<S> for State<Sub>
where
    S: Send + Sync,
    Sub: FromRef<S> + Send,
{
    fn extract_part(
        _input: &mut CommandInput,
        cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(Ok(Self(Sub::from_ref(cx.state))))
    }
}

impl<S: Send + Sync> sealed::FromCommandPart<S> for Principal {
    fn extract_part(
        _input: &mut CommandInput,
        cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(Ok(cx.principal.clone()))
    }
}

impl<S: Send + Sync> sealed::FromCommandPart<S> for VerifiedPrincipal {
    fn extract_part(
        _input: &mut CommandInput,
        cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(Ok(Self(cx.principal.clone())))
    }
}

impl<S: Send + Sync> sealed::FromCommandPart<S> for VerifiedDelegation {
    fn extract_part(
        _input: &mut CommandInput,
        cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(
            cx.delegation
                .cloned()
                .map(Self)
                .ok_or_else(crate::context::missing_extension_problem::<Self>),
        )
    }
}

impl<S: Send + Sync> sealed::FromCommandPart<S> for Option<DelegationCredential> {
    fn extract_part(
        _input: &mut CommandInput,
        cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(Ok(cx.delegation.cloned()))
    }
}

impl<S, T> sealed::FromCommandPart<S> for Path<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    async fn extract_part(
        input: &mut CommandInput,
        _cx: &sealed::ExtractContext<'_, S>,
    ) -> Result<Self, Problem> {
        input.path().await.map(Self)
    }
}

impl<S, T> sealed::FromCommandPart<S> for Query<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    fn extract_part(
        input: &mut CommandInput,
        _cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(input.query().map(Self))
    }
}

impl<S, T> sealed::FromCommandPart<S> for Json<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    fn extract_part(
        input: &mut CommandInput,
        _cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(input.json().map(Self))
    }
}

impl<S: Send + Sync> sealed::FromCommand<S, sealed::ViaCommand> for CommandInput {
    fn extract(
        input: Self,
        _cx: &sealed::ExtractContext<'_, S>,
    ) -> impl Future<Output = Result<Self, Problem>> + Send {
        std::future::ready(Ok(input))
    }
}

impl<S, T> sealed::FromCommand<S, sealed::ViaPart> for T
where
    S: Send + Sync,
    T: sealed::FromCommandPart<S>,
{
    async fn extract(
        mut input: CommandInput,
        cx: &sealed::ExtractContext<'_, S>,
    ) -> Result<Self, Problem> {
        Self::extract_part(&mut input, cx).await
    }
}

impl<F, Fut, C, S> sealed::CommandHandler<(), S> for F
where
    F: FnOnce() -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Result<C, Problem>> + Send,
    C: FreshCommand + 'static,
    S: Clone + Send + Sync + 'static,
{
    type Command = C;

    async fn call(
        self,
        _input: CommandInput,
        _cx: &sealed::ExtractContext<'_, S>,
    ) -> Result<Self::Command, Problem> {
        self().await
    }
}

macro_rules! impl_command_handler {
    ([$($ty:ident),*], $last:ident) => {
        #[allow(
            non_snake_case,
            unused_mut,
            reason = "generated extractor bindings follow tuple type parameter names"
        )]
        impl<F, Fut, C, S, M, $($ty,)* $last> sealed::CommandHandler<(M, $($ty,)* $last), S> for F
        where
            F: FnOnce($($ty,)* $last,) -> Fut + Clone + Send + Sync + 'static,
            Fut: Future<Output = Result<C, Problem>> + Send,
            C: FreshCommand + 'static,
            S: Clone + Send + Sync + 'static,
            $($ty: FromCommandPart<S> + Send,)*
            $last: FromCommand<S, M> + Send,
        {
            type Command = C;

            async fn call(
                self,
                mut input: CommandInput,
                cx: &sealed::ExtractContext<'_, S>,
            ) -> Result<Self::Command, Problem> {
                $(let $ty = $ty::extract_part(&mut input, cx).await?;)*
                let $last = $last::extract(input, cx).await?;
                self($($ty,)* $last,).await
            }
        }
    };
}

impl_command_handler!([], T1);
impl_command_handler!([T1], T2);
impl_command_handler!([T1, T2], T3);
impl_command_handler!([T1, T2, T3], T4);
impl_command_handler!([T1, T2, T3, T4], T5);
impl_command_handler!([T1, T2, T3, T4, T5], T6);
impl_command_handler!([T1, T2, T3, T4, T5, T6], T7);
impl_command_handler!([T1, T2, T3, T4, T5, T6, T7], T8);

pub fn route<S, Args, H>(
    mut document: utoipa::openapi::OpenApi,
    prepare: H,
) -> Result<HttpContract<S>, FinalizeError>
where
    S: Clone + Send + Sync + 'static,
    Args: 'static,
    H: CommandHandler<Args, S>,
{
    let operation = declaration::prepare(
        &mut document,
        <H::Command as FreshCommand>::Success::responses(),
        <H::Command as FreshCommand>::Success::schemas(),
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
    let handler = move |State(state): State<S>, mut request: Request| async move {
        let delegation = request.extensions_mut().remove::<DelegationCredential>();
        let (input, principal, attempt) = match capture_attempt(route_state, request).await {
            Ok(captured) => captured,
            Err(response) => return response,
        };
        let cx = sealed::ExtractContext {
            principal: &principal,
            delegation: delegation.as_ref(),
            state: &state,
        };
        match prepare.call(input, &cx).await {
            Ok(command) => attempt.run(command).await,
            Err(problem) => {
                attempt.route.record(Outcome::NotStored);
                problem.into_response()
            }
        }
    };
    let route = (
        Vec::new(),
        std::mem::take(&mut document.paths),
        on(method, handler),
    );
    HttpContract::from_idempotent(document, route)?.with_components(
        IdempotencyComponents::openapi()
            .components
            .unwrap_or_default(),
    )
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the middleware returns"
)]
async fn capture_attempt(
    route: Arc<RouteState>,
    mut request: Request,
) -> Result<(CommandInput, Principal, Attempt), Response> {
    let store = request
        .extensions_mut()
        .remove::<Store>()
        .ok_or_else(|| wiring_failure(&route.operation_id, "store_missing"))?;
    let request = request.with_limited_body();
    let (mut parts, body) = request.into_parts();
    let deadline = parts
        .extensions
        .get::<RequestContext>()
        .and_then(RequestContext::deadline)
        .ok_or_else(|| wiring_failure(&route.operation_id, "deadline_missing"))?;
    let principal = parts
        .extensions
        .remove::<Principal>()
        .ok_or_else(|| wiring_failure(&route.operation_id, "principal_missing"))?;
    let caller = identity::caller_identity(principal.issuer(), principal.identity());
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
    let collected = buffer_body(parts.extensions.get::<RequestContext>(), body)
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
    use std::convert::Infallible;

    use serde::Deserialize;

    use super::*;
    use crate::idempotency::{CreatedText, Rollback, Tx};

    #[derive(Clone)]
    struct AppState {
        sub: SubState,
    }

    #[derive(Clone)]
    struct SubState;

    impl FromRef<AppState> for SubState {
        fn from_ref(state: &AppState) -> Self {
            state.sub.clone()
        }
    }

    #[derive(Deserialize)]
    struct Filter {
        mode: String,
    }

    #[derive(Deserialize)]
    struct Payload {
        name: String,
    }

    struct DummyCommand;

    impl FreshCommand for DummyCommand {
        type Success = CreatedText;
        type Terminal = Infallible;

        fn execute(
            self,
            _: &mut Tx<'_>,
        ) -> impl Future<Output = Result<Self::Success, Rollback>> + Send {
            std::future::poll_fn(|_| unreachable!("composition fixture"))
        }
    }

    fn post_document() -> utoipa::openapi::OpenApi {
        serde_json::from_value(serde_json::json!({
            "openapi": "3.1.0",
            "info": {"title": "fixture", "version": "1"},
            "externalDocs": {"url": "https://docs.example/retry"},
            "paths": {
                "/items/{id}": {
                    "post": {
                        "operationId": "createItem",
                        "responses": {"201": {"description": "created"}}
                    }
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn route_accepts_axum_style_command_extractors_in_any_supported_combination() {
        async fn zero() -> Result<DummyCommand, Problem> {
            Ok(DummyCommand)
        }
        #[expect(
            clippy::too_many_arguments,
            reason = "exercises the maximum 8-extractor command handler arity"
        )]
        async fn all(
            State(sub): State<SubState>,
            principal: Principal,
            VerifiedPrincipal(verified): VerifiedPrincipal,
            delegation: Option<DelegationCredential>,
            Path(id): Path<u32>,
            Query(Filter { mode }): Query<Filter>,
            Json(Payload { name }): Json<Payload>,
            input: CommandInput,
        ) -> Result<DummyCommand, Problem> {
            let _ = (sub, principal, verified, delegation, id, mode, name, input);
            Ok(DummyCommand)
        }
        async fn state_last(State(sub): State<SubState>) -> Result<DummyCommand, Problem> {
            let _ = sub;
            Ok(DummyCommand)
        }
        async fn principal_last(principal: Principal) -> Result<DummyCommand, Problem> {
            let _ = principal;
            Ok(DummyCommand)
        }
        async fn verified_principal_last(
            VerifiedPrincipal(principal): VerifiedPrincipal,
        ) -> Result<DummyCommand, Problem> {
            let _ = principal;
            Ok(DummyCommand)
        }
        async fn verified_delegation_last(
            VerifiedDelegation(delegation): VerifiedDelegation,
        ) -> Result<DummyCommand, Problem> {
            let _ = delegation;
            Ok(DummyCommand)
        }
        async fn delegation_last(
            delegation: Option<DelegationCredential>,
        ) -> Result<DummyCommand, Problem> {
            let _ = delegation;
            Ok(DummyCommand)
        }
        async fn path_last(Path(id): Path<u32>) -> Result<DummyCommand, Problem> {
            let _ = id;
            Ok(DummyCommand)
        }
        async fn query_last(Query(filter): Query<Filter>) -> Result<DummyCommand, Problem> {
            let _ = filter.mode;
            Ok(DummyCommand)
        }
        async fn json_last(Json(payload): Json<Payload>) -> Result<DummyCommand, Problem> {
            let _ = payload.name;
            Ok(DummyCommand)
        }

        let _: HttpContract<AppState> = route(post_document(), zero).unwrap();
        let _: HttpContract<AppState> = route(post_document(), all).unwrap();
        let _: HttpContract<AppState> = route(post_document(), state_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), principal_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), verified_principal_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), verified_delegation_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), delegation_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), path_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), query_last).unwrap();
        let _: HttpContract<AppState> = route(post_document(), json_last).unwrap();
    }
}
