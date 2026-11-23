use std::cell::RefCell;
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use opentelemetry::Context as OtelContext;
use opentelemetry::trace::TraceContextExt as _;
use operation_context::{Deadline, OperationContext};
use temporalio_common::data_converters::PayloadConverter;
use temporalio_common::protos::temporal::api::common::v1::Payload;
use temporalio_sdk::activities::{ActivityContext, ActivityError, ActivityInfo};
use temporalio_sdk::interceptors::{
    ActivityInboundInterceptor, ExecuteActivityInput, ExecuteActivityOutput, Next as ActivityNext,
};
use temporalio_sdk::workflow_interceptors::{
    CancellableWorkflowOutboundFuture, ContinueAsNewInput, ContinueAsNewResult,
    ExecuteWorkflowInput, ExecuteWorkflowResult, HandleQueryInput, HandleQueryResult,
    HandleSignalInput, HandleSignalResult, HandleUpdateInput, HandleUpdateResult,
    InitializeWorkflowInput, InitializeWorkflowOutput, ScheduleActivityInput,
    ScheduleActivityResult, ScheduleLocalActivityInput, SignalWorkflowInput, SignalWorkflowResult,
    StartChildWorkflowInput, StartChildWorkflowResult, SyncWorkflowInterceptorContext,
    ValidateUpdateInput, ValidateUpdateResult, WorkflowInterceptor, WorkflowInterceptorConstructor,
    WorkflowInterceptorContext, WorkflowInterceptorFuture, WorkflowNext,
};
use temporalio_sdk::{WorkflowContextKey, WorkflowContextView};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::trace::{
    TRACER_DATA_HEADER, extract_context_from_headers, inject_current_context_into_headers,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Preempted;

pub async fn run_with_preemption<T, F: Future<Output = T>>(
    force_abort: &CancellationToken,
    fut: F,
) -> Result<T, Preempted> {
    tokio::select! {
        biased;
        () = force_abort.cancelled() => Err(Preempted),
        output = fut => Ok(output),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ActivityAttemptKey {
    task_token: Vec<u8>,
    namespace: String,
    task_queue: String,
    workflow_id: Option<String>,
    workflow_run_id: Option<String>,
    activity_id: String,
    activity_run_id: Option<String>,
    attempt: u32,
    is_local: bool,
}

impl ActivityAttemptKey {
    fn from_info(info: &ActivityInfo) -> Self {
        Self {
            task_token: info.task_token.clone(),
            namespace: info.namespace.clone(),
            task_queue: info.task_queue.clone(),
            workflow_id: info.workflow_id.clone(),
            workflow_run_id: info.workflow_run_id.clone(),
            activity_id: info.activity_id.clone(),
            activity_run_id: info.activity_run_id.clone(),
            attempt: info.attempt,
            is_local: info.is_local,
        }
    }
}

struct BridgeState {
    closed: bool,
    handle: Option<JoinHandle<()>>,
}

struct ActivityOperationScope {
    exit: CancellationToken,
    attempt_cancel: CancellationToken,
    runtime: Option<Handle>,
    bridge_started: AtomicBool,
    bridge: Mutex<BridgeState>,
    #[cfg(test)]
    bridge_spawns: AtomicUsize,
}

impl ActivityOperationScope {
    fn new(force_abort: &CancellationToken, runtime: Option<Handle>) -> Self {
        Self {
            exit: CancellationToken::new(),
            attempt_cancel: force_abort.child_token(),
            runtime,
            bridge_started: AtomicBool::new(false),
            bridge: Mutex::new(BridgeState {
                closed: false,
                handle: None,
            }),
            #[cfg(test)]
            bridge_spawns: AtomicUsize::new(0),
        }
    }

    fn ensure_cancel_bridge(&self, ctx: &ActivityContext) {
        if self.bridge_started.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.bridge.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed || state.handle.is_some() {
            return;
        }
        let Some(runtime) = self.runtime.clone().or_else(|| Handle::try_current().ok()) else {
            return;
        };
        let exit = self.exit.clone();
        let attempt_cancel = self.attempt_cancel.clone();
        let ctx_clone = ctx.clone();
        let handle = runtime.spawn(async move {
            tokio::select! {
                biased;
                () = exit.cancelled() => {}
                () = attempt_cancel.cancelled() => {}
                () = ctx_clone.cancelled() => {
                    attempt_cancel.cancel();
                }
            }
        });
        state.handle = Some(handle);
        self.bridge_started.store(true, Ordering::Release);
        #[cfg(test)]
        self.bridge_spawns.fetch_add(1, Ordering::AcqRel);
    }

    fn close_and_take_bridge(&self) -> Option<JoinHandle<()>> {
        let mut state = self.bridge.lock().unwrap_or_else(PoisonError::into_inner);
        state.closed = true;
        self.exit.cancel();
        state.handle.take()
    }

    fn abort_sync(&self) {
        if let Some(handle) = self.close_and_take_bridge() {
            handle.abort();
        }
    }

    async fn drain(&self) {
        if let Some(handle) = self.close_and_take_bridge() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

static ACTIVE_ACTIVITY_SCOPES: LazyLock<
    Mutex<HashMap<ActivityAttemptKey, Vec<Arc<ActivityOperationScope>>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn register_active_activity_scope(key: ActivityAttemptKey, scope: Arc<ActivityOperationScope>) {
    let mut scopes = ACTIVE_ACTIVITY_SCOPES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    scopes.entry(key).or_default().push(scope);
}

fn unregister_active_activity_scope(key: &ActivityAttemptKey, scope: &Arc<ActivityOperationScope>) {
    let mut scopes = ACTIVE_ACTIVITY_SCOPES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(entries) = scopes.get_mut(key) {
        entries.retain(|entry| !Arc::ptr_eq(entry, scope));
        if entries.is_empty() {
            scopes.remove(key);
        }
    }
}

fn lookup_active_activity_scope(info: &ActivityInfo) -> Option<Arc<ActivityOperationScope>> {
    let key = ActivityAttemptKey::from_info(info);
    let scopes = ACTIVE_ACTIVITY_SCOPES
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    scopes.get(&key).and_then(|entries| entries.last()).cloned()
}

tokio::task_local! {
    static ACTIVITY_OPERATION_SCOPE: Arc<ActivityOperationScope>;
}

#[cfg(test)]
pub(crate) fn active_scope_bridge_spawns(ctx: &ActivityContext) -> Option<usize> {
    ACTIVITY_OPERATION_SCOPE
        .try_with(Arc::clone)
        .ok()
        .or_else(|| lookup_active_activity_scope(ctx.info()))
        .map(|scope| scope.bridge_spawns.load(Ordering::Acquire))
}

#[cfg(test)]
pub(crate) fn has_active_activity_scope(info: &ActivityInfo) -> bool {
    lookup_active_activity_scope(info).is_some()
}

pub(crate) fn deadline_from_system_time(system_deadline: SystemTime) -> Deadline {
    let remaining = system_deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    Deadline::after(remaining)
}

#[must_use]
pub fn activity_operation_context(ctx: &ActivityContext) -> OperationContext {
    let deadline = ctx.info().deadline.map(deadline_from_system_time);
    let scope = ACTIVITY_OPERATION_SCOPE
        .try_with(Arc::clone)
        .ok()
        .or_else(|| lookup_active_activity_scope(ctx.info()));

    let Some(scope) = scope else {
        let token = CancellationToken::new();
        if ctx.is_cancelled() {
            token.cancel();
        }
        return OperationContext::new(deadline, token);
    };

    if ctx.is_cancelled() {
        scope.attempt_cancel.cancel();
        return OperationContext::new(deadline, scope.attempt_cancel.child_token());
    }

    scope.ensure_cancel_bridge(ctx);
    OperationContext::new(deadline, scope.attempt_cancel.child_token())
}

pub(crate) struct ActivityOperationScopeInterceptor {
    force_abort: CancellationToken,
}

impl ActivityOperationScopeInterceptor {
    pub(crate) const fn new(force_abort: CancellationToken) -> Self {
        Self { force_abort }
    }
}

impl ActivityInboundInterceptor for ActivityOperationScopeInterceptor {
    fn execute_activity<'a>(
        &'a self,
        input: ExecuteActivityInput,
        next: ActivityNext<'a, ExecuteActivityInput, ExecuteActivityOutput<'a>>,
    ) -> ExecuteActivityOutput<'a> {
        let key = ActivityAttemptKey::from_info(input.activity_info());
        let scope = Arc::new(ActivityOperationScope::new(
            &self.force_abort,
            Handle::try_current().ok(),
        ));
        register_active_activity_scope(key.clone(), Arc::clone(&scope));
        let guard = std::mem::DropGuard::new((key.clone(), Arc::clone(&scope)), |(key, scope)| {
            unregister_active_activity_scope(&key, &scope);
            scope.abort_sync();
        });
        Box::pin(async move {
            let result = ACTIVITY_OPERATION_SCOPE
                .scope(Arc::clone(&scope), next.run(input))
                .await;
            unregister_active_activity_scope(&key, &scope);
            scope.drain().await;
            let _ = std::mem::DropGuard::dismiss(guard);
            result
        })
    }
}

pub(crate) struct ForceShutdownActivityInterceptor {
    force_abort: CancellationToken,
}

impl ForceShutdownActivityInterceptor {
    pub(crate) const fn new(force_abort: CancellationToken) -> Self {
        Self { force_abort }
    }
}

impl ActivityInboundInterceptor for ForceShutdownActivityInterceptor {
    fn execute_activity<'a>(
        &'a self,
        input: ExecuteActivityInput,
        next: ActivityNext<'a, ExecuteActivityInput, ExecuteActivityOutput<'a>>,
    ) -> ExecuteActivityOutput<'a> {
        let force_abort = self.force_abort.clone();
        Box::pin(async move {
            match run_with_preemption(&force_abort, next.run(input)).await {
                Ok(result) => result,
                Err(Preempted) => Err(ActivityError::WillCompleteAsync),
            }
        })
    }
}

pub(crate) struct TraceActivityInterceptor {
    payload_converter: PayloadConverter,
}

impl TraceActivityInterceptor {
    pub(crate) const fn new(payload_converter: PayloadConverter) -> Self {
        Self { payload_converter }
    }
}

impl ActivityInboundInterceptor for TraceActivityInterceptor {
    fn execute_activity<'a>(
        &'a self,
        input: ExecuteActivityInput,
        next: ActivityNext<'a, ExecuteActivityInput, ExecuteActivityOutput<'a>>,
    ) -> ExecuteActivityOutput<'a> {
        let info = input.activity_info();
        let span = tracing::info_span!(
            "temporal_run_activity",
            otel.kind = "server",
            temporal.activity_type = %info.activity_type,
            temporal.task_queue = %info.task_queue,
            temporal.attempt = info.attempt,
        );
        let parent = extract_context_from_headers(input.headers(), &self.payload_converter);
        attach_parent(&span, &parent);
        Box::pin(next.run(input).instrument(span))
    }
}

pub(crate) fn trace_workflow_interceptor_constructor(
    payload_converter: PayloadConverter,
) -> WorkflowInterceptorConstructor {
    WorkflowInterceptorConstructor::new(move |_| {
        TraceWorkflowInterceptor::new(payload_converter.clone())
    })
}

struct ActiveInboundTraceSpan;

impl WorkflowContextKey for ActiveInboundTraceSpan {
    type Value = ();
}

struct TraceWorkflowInterceptor {
    payload_converter: PayloadConverter,
    workflow_parent: RefCell<Option<OtelContext>>,
}

impl TraceWorkflowInterceptor {
    fn new(payload_converter: PayloadConverter) -> Self {
        Self {
            payload_converter,
            workflow_parent: RefCell::new(None),
        }
    }

    fn parent_for(&self, headers: &HashMap<String, Payload>) -> OtelContext {
        if headers.contains_key(TRACER_DATA_HEADER) {
            let extracted = extract_context_from_headers(headers, &self.payload_converter);
            if extracted.span().span_context().is_valid() {
                return extracted;
            }
        }
        self.workflow_parent.borrow().clone().unwrap_or_default()
    }

    fn store_workflow_parent(&self, headers: &HashMap<String, Payload>) -> OtelContext {
        let parent = self.parent_for(headers);
        if parent.span().span_context().is_valid() {
            *self.workflow_parent.borrow_mut() = Some(parent.clone());
        }
        parent
    }

    fn attach_fallback_parent_if_detached(&self, span: &tracing::Span, has_inbound_span: bool) {
        if !has_inbound_span && let Some(parent) = self.workflow_parent.borrow().as_ref() {
            attach_parent(span, parent);
        }
    }

    fn inject_span_headers(&self, span: &tracing::Span, headers: &mut HashMap<String, Payload>) {
        span.in_scope(|| {
            let _ = inject_current_context_into_headers(headers, &self.payload_converter);
        });
    }
}

fn attach_parent(span: &tracing::Span, context: &OtelContext) {
    if context.span().span_context().is_valid() {
        let _ = span.set_parent(context.clone());
    }
}

fn attach_link(span: &tracing::Span, context: &OtelContext) {
    let span_ref = context.span();
    let span_context = span_ref.span_context();
    if span_context.is_valid() {
        span.add_link(span_context.clone());
    }
}

impl WorkflowInterceptor for TraceWorkflowInterceptor {
    fn initialize_workflow(
        &self,
        _ctx: WorkflowContextView,
        input: InitializeWorkflowInput,
        next: WorkflowNext<'_, InitializeWorkflowInput, InitializeWorkflowOutput>,
    ) -> InitializeWorkflowOutput {
        let _ = self.store_workflow_parent(input.headers());
        next.run(input)
    }

    fn execute<'a>(
        &'a self,
        ctx: WorkflowInterceptorContext,
        input: ExecuteWorkflowInput,
        next: WorkflowNext<
            'a,
            ExecuteWorkflowInput,
            WorkflowInterceptorFuture<'a, ExecuteWorkflowResult>,
        >,
    ) -> WorkflowInterceptorFuture<'a, ExecuteWorkflowResult> {
        let parent = self.store_workflow_parent(input.headers());
        if ctx.is_replaying() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_run_workflow",
            otel.kind = "consumer",
            temporal.workflow_type = %ctx.workflow_type(),
            temporal.task_queue = %ctx.task_queue(),
        );
        attach_link(&span, &parent);
        WorkflowInterceptorFuture::new(
            ctx.with_context_value::<ActiveInboundTraceSpan, _>(
                (),
                next.run(input).instrument(span),
            ),
        )
    }

    fn handle_signal<'a>(
        &'a self,
        ctx: WorkflowInterceptorContext,
        input: HandleSignalInput,
        next: WorkflowNext<
            'a,
            HandleSignalInput,
            WorkflowInterceptorFuture<'a, HandleSignalResult>,
        >,
    ) -> WorkflowInterceptorFuture<'a, HandleSignalResult> {
        if ctx.is_replaying() {
            return next.run(input);
        }
        let parent = self.parent_for(input.headers());
        let span = tracing::info_span!(
            "temporal_handle_signal",
            otel.kind = "consumer",
            temporal.signal_name = %input.name(),
        );
        attach_link(&span, &parent);
        WorkflowInterceptorFuture::new(
            ctx.with_context_value::<ActiveInboundTraceSpan, _>(
                (),
                next.run(input).instrument(span),
            ),
        )
    }

    fn handle_update<'a>(
        &'a self,
        ctx: WorkflowInterceptorContext,
        input: HandleUpdateInput,
        next: WorkflowNext<
            'a,
            HandleUpdateInput,
            WorkflowInterceptorFuture<'a, HandleUpdateResult>,
        >,
    ) -> WorkflowInterceptorFuture<'a, HandleUpdateResult> {
        if ctx.is_replaying() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_handle_update",
            otel.kind = "server",
            temporal.update_name = %input.name(),
        );
        let direct_parent = extract_context_from_headers(input.headers(), &self.payload_converter);
        if direct_parent.span().span_context().is_valid() {
            attach_parent(&span, &direct_parent);
        } else if let Some(workflow_parent) = self.workflow_parent.borrow().as_ref() {
            attach_link(&span, workflow_parent);
        }
        WorkflowInterceptorFuture::new(
            ctx.with_context_value::<ActiveInboundTraceSpan, _>(
                (),
                next.run(input).instrument(span),
            ),
        )
    }

    fn handle_query(
        &self,
        ctx: SyncWorkflowInterceptorContext,
        input: HandleQueryInput,
        next: WorkflowNext<'_, HandleQueryInput, HandleQueryResult>,
    ) -> HandleQueryResult {
        if ctx.is_replaying_history_events() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_handle_query",
            otel.kind = "server",
            temporal.query_name = %input.name(),
        );
        let direct_parent = extract_context_from_headers(input.headers(), &self.payload_converter);
        if direct_parent.span().span_context().is_valid() {
            attach_parent(&span, &direct_parent);
        } else if let Some(workflow_parent) = self.workflow_parent.borrow().as_ref() {
            attach_link(&span, workflow_parent);
        }
        span.in_scope(|| {
            ctx.with_context_value::<ActiveInboundTraceSpan, _>((), || next.run(input))
        })
    }

    fn validate_update(
        &self,
        ctx: SyncWorkflowInterceptorContext,
        input: ValidateUpdateInput,
        next: WorkflowNext<'_, ValidateUpdateInput, ValidateUpdateResult>,
    ) -> ValidateUpdateResult {
        if ctx.is_replaying_history_events() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_validate_update",
            otel.kind = "server",
            temporal.update_name = %input.name(),
        );
        let direct_parent = extract_context_from_headers(input.headers(), &self.payload_converter);
        if direct_parent.span().span_context().is_valid() {
            attach_parent(&span, &direct_parent);
        } else if let Some(workflow_parent) = self.workflow_parent.borrow().as_ref() {
            attach_link(&span, workflow_parent);
        }
        span.in_scope(|| {
            ctx.with_context_value::<ActiveInboundTraceSpan, _>((), || next.run(input))
        })
    }

    fn schedule_activity(
        &self,
        ctx: WorkflowInterceptorContext,
        mut input: ScheduleActivityInput,
        next: WorkflowNext<
            'static,
            ScheduleActivityInput,
            CancellableWorkflowOutboundFuture<ScheduleActivityResult>,
        >,
    ) -> CancellableWorkflowOutboundFuture<ScheduleActivityResult> {
        if ctx.is_replaying() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_schedule_activity",
            otel.kind = "client",
            temporal.activity_type = %input.activity_type(),
        );
        let has_inbound_span = ctx.context_value::<ActiveInboundTraceSpan>().is_some();
        self.attach_fallback_parent_if_detached(&span, has_inbound_span);
        self.inject_span_headers(&span, input.headers_mut());
        #[expect(
            clippy::result_large_err,
            reason = "temporalio-sdk ScheduleActivityResult carries ActivityExecutionError by value"
        )]
        next.run(input).map(move |result| {
            drop(span);
            result
        })
    }

    fn schedule_local_activity(
        &self,
        ctx: WorkflowInterceptorContext,
        mut input: ScheduleLocalActivityInput,
        next: WorkflowNext<
            'static,
            ScheduleLocalActivityInput,
            CancellableWorkflowOutboundFuture<ScheduleActivityResult>,
        >,
    ) -> CancellableWorkflowOutboundFuture<ScheduleActivityResult> {
        if ctx.is_replaying() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_schedule_local_activity",
            otel.kind = "client",
            temporal.activity_type = %input.activity_type(),
        );
        let has_inbound_span = ctx.context_value::<ActiveInboundTraceSpan>().is_some();
        self.attach_fallback_parent_if_detached(&span, has_inbound_span);
        self.inject_span_headers(&span, input.headers_mut());
        #[expect(
            clippy::result_large_err,
            reason = "temporalio-sdk ScheduleActivityResult carries ActivityExecutionError by value"
        )]
        next.run(input).map(move |result| {
            drop(span);
            result
        })
    }

    fn start_child_workflow(
        &self,
        ctx: WorkflowInterceptorContext,
        mut input: StartChildWorkflowInput,
        next: WorkflowNext<
            'static,
            StartChildWorkflowInput,
            CancellableWorkflowOutboundFuture<StartChildWorkflowResult>,
        >,
    ) -> CancellableWorkflowOutboundFuture<StartChildWorkflowResult> {
        if ctx.is_replaying() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_start_child_workflow",
            otel.kind = "client",
            temporal.workflow_type = %input.workflow_type(),
        );
        let has_inbound_span = ctx.context_value::<ActiveInboundTraceSpan>().is_some();
        self.attach_fallback_parent_if_detached(&span, has_inbound_span);
        self.inject_span_headers(&span, input.headers_mut());
        next.run(input).map(move |result| {
            drop(span);
            result
        })
    }

    fn signal_workflow(
        &self,
        ctx: WorkflowInterceptorContext,
        mut input: SignalWorkflowInput,
        next: WorkflowNext<
            'static,
            SignalWorkflowInput,
            CancellableWorkflowOutboundFuture<SignalWorkflowResult>,
        >,
    ) -> CancellableWorkflowOutboundFuture<SignalWorkflowResult> {
        if ctx.is_replaying() {
            return next.run(input);
        }
        let span = tracing::info_span!(
            "temporal_signal_external_workflow",
            otel.kind = "client",
            temporal.signal_name = %input.signal_name(),
        );
        let has_inbound_span = ctx.context_value::<ActiveInboundTraceSpan>().is_some();
        self.attach_fallback_parent_if_detached(&span, has_inbound_span);
        self.inject_span_headers(&span, input.headers_mut());
        next.run(input).map(move |result| {
            drop(span);
            result
        })
    }

    fn continue_as_new(
        &self,
        ctx: SyncWorkflowInterceptorContext,
        mut input: ContinueAsNewInput,
        next: WorkflowNext<'static, ContinueAsNewInput, ContinueAsNewResult>,
    ) -> ContinueAsNewResult {
        if !ctx.is_replaying() {
            if ctx.context_value::<ActiveInboundTraceSpan>().is_some() {
                let _ = inject_current_context_into_headers(
                    input.headers_mut(),
                    &self.payload_converter,
                );
            } else {
                let span = tracing::info_span!("temporal_continue_as_new", otel.kind = "client");
                self.attach_fallback_parent_if_detached(&span, false);
                self.inject_span_headers(&span, input.headers_mut());
            }
        }
        next.run(input)
    }
}
