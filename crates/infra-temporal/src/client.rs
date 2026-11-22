use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use compact_str::CompactString;
use operation_context::OperationContext;
use temporalio_client::{
    Client, RetryOptions, RpcOptions, WorkflowCancelOptions, WorkflowDescribeOptions,
    WorkflowExecuteUpdateOptions, WorkflowExecutionDescription, WorkflowExecutionInfo,
    WorkflowGetResultOptions, WorkflowIdConflictPolicy, WorkflowIdReusePolicy,
    WorkflowQueryOptions, WorkflowSignalOptions, WorkflowStartOptions, WorkflowTerminateOptions,
    WorkflowUpdateWithStartOptions,
};
use temporalio_common::search_attributes::SearchAttributes;
use temporalio_common::{
    HasWorkflowDefinition, MemoValues, QueryDefinition, RetryPolicy, SignalDefinition,
    UpdateDefinition,
};

use crate::error::{
    MutationError, MutationFailure, ReadError, StartFailure, StartWorkflowError, UnknownOutcome,
    UpdateFailure, UpdateWorkflowError, WorkflowResultError, bounded_stop_to_stopped,
    map_describe_error, map_mutation_error, map_query_error, map_result_error, map_start_error,
    map_update_error, map_update_with_start_error,
};
use crate::ids::{NonBlankName, RequestKey, RunId, UpdateId, WorkflowId};

const LONG_POLL_RPC_SLICE: Duration = Duration::from_secs(70);
const DEFAULT_RETRY_INITIAL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_RETRY_MAX_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartIdPolicy {
    RejectDuplicate,
    AllowDuplicateFailedOnly,
    AllowDuplicate {
        running_conflict: RunningConflictPolicy,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningConflictPolicy {
    Fail,
    UseExisting,
    TerminateExisting,
}

impl StartIdPolicy {
    const fn into_sdk_policies(self) -> (WorkflowIdReusePolicy, WorkflowIdConflictPolicy) {
        match self {
            Self::RejectDuplicate => (
                WorkflowIdReusePolicy::RejectDuplicate,
                WorkflowIdConflictPolicy::Fail,
            ),
            Self::AllowDuplicateFailedOnly => (
                WorkflowIdReusePolicy::AllowDuplicateFailedOnly,
                WorkflowIdConflictPolicy::Fail,
            ),
            Self::AllowDuplicate { running_conflict } => (
                WorkflowIdReusePolicy::AllowDuplicate,
                match running_conflict {
                    RunningConflictPolicy::Fail => WorkflowIdConflictPolicy::Fail,
                    RunningConflictPolicy::UseExisting => WorkflowIdConflictPolicy::UseExisting,
                    RunningConflictPolicy::TerminateExisting => {
                        WorkflowIdConflictPolicy::TerminateExisting
                    }
                },
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationDedupe {
    Key(RequestKey),
    Unkeyed,
}

pub struct StartRequest<W: HasWorkflowDefinition> {
    workflow: W,
    task_queue: NonBlankName,
    workflow_id: WorkflowId,
    id_policy: StartIdPolicy,
    input: W::Input,
    execution_timeout: Option<Duration>,
    run_timeout: Option<Duration>,
    task_timeout: Option<Duration>,
    retry_policy: Option<RetryPolicy>,
    memo: Option<BTreeMap<String, serde_json::Value>>,
    search_attributes: Option<SearchAttributes>,
    static_summary: Option<String>,
    static_details: Option<String>,
}

impl<W: HasWorkflowDefinition> fmt::Debug for StartRequest<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartRequest")
            .field("workflow_type", &self.workflow.name())
            .field("task_queue", &self.task_queue)
            .field("workflow_id", &self.workflow_id)
            .field("id_policy", &self.id_policy)
            .field("execution_timeout", &self.execution_timeout)
            .field("run_timeout", &self.run_timeout)
            .field("task_timeout", &self.task_timeout)
            .field("has_retry_policy", &self.retry_policy.is_some())
            .field("has_memo", &self.memo.is_some())
            .field("has_search_attributes", &self.search_attributes.is_some())
            .finish_non_exhaustive()
    }
}

impl<W: HasWorkflowDefinition> StartRequest<W> {
    #[must_use]
    pub fn new(
        workflow: W,
        task_queue: NonBlankName,
        workflow_id: WorkflowId,
        id_policy: StartIdPolicy,
        input: W::Input,
    ) -> Self {
        Self {
            workflow,
            task_queue,
            workflow_id,
            id_policy,
            input,
            execution_timeout: None,
            run_timeout: None,
            task_timeout: None,
            retry_policy: None,
            memo: None,
            search_attributes: None,
            static_summary: None,
            static_details: None,
        }
    }

    #[must_use]
    pub const fn execution_timeout(mut self, d: Duration) -> Self {
        self.execution_timeout = Some(d);
        self
    }

    #[must_use]
    pub const fn run_timeout(mut self, d: Duration) -> Self {
        self.run_timeout = Some(d);
        self
    }

    #[must_use]
    pub const fn task_timeout(mut self, d: Duration) -> Self {
        self.task_timeout = Some(d);
        self
    }

    #[must_use]
    pub fn retry_policy(mut self, p: RetryPolicy) -> Self {
        self.retry_policy = Some(p);
        self
    }

    #[must_use]
    pub fn memo(mut self, m: BTreeMap<String, serde_json::Value>) -> Self {
        self.memo = Some(m);
        self
    }

    #[must_use]
    pub fn search_attributes(mut self, sa: SearchAttributes) -> Self {
        self.search_attributes = Some(sa);
        self
    }

    #[must_use]
    pub fn static_summary(mut self, s: impl Into<String>) -> Self {
        self.static_summary = Some(s.into());
        self
    }

    #[must_use]
    pub fn static_details(mut self, d: impl Into<String>) -> Self {
        self.static_details = Some(d.into());
        self
    }

    fn into_sdk_parts(self, rpc_options: RpcOptions) -> (W, W::Input, WorkflowStartOptions) {
        let (id_reuse_policy, id_conflict_policy) = self.id_policy.into_sdk_policies();
        let memo = self.memo.map(to_memo_values);
        let options =
            WorkflowStartOptions::new(self.task_queue.into_inner(), self.workflow_id.into_inner())
                .id_reuse_policy(id_reuse_policy)
                .id_conflict_policy(id_conflict_policy)
                .maybe_execution_timeout(self.execution_timeout)
                .maybe_run_timeout(self.run_timeout)
                .maybe_task_timeout(self.task_timeout)
                .maybe_retry_policy(self.retry_policy)
                .maybe_memo(memo)
                .maybe_search_attributes(self.search_attributes)
                .maybe_static_summary(self.static_summary)
                .maybe_static_details(self.static_details)
                .rpc_options(rpc_options)
                .build();
        (self.workflow, self.input, options)
    }
}

pub struct UpdateWithStartRequest<W: HasWorkflowDefinition, U: UpdateDefinition<Workflow = W::Run>>
{
    start: StartRequest<W>,
    update: U,
    update_id: UpdateId,
    input: U::Input,
}

impl<W: HasWorkflowDefinition, U: UpdateDefinition<Workflow = W::Run>> fmt::Debug
    for UpdateWithStartRequest<W, U>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateWithStartRequest")
            .field("start", &self.start)
            .field("update_name", &self.update.name())
            .field("update_id", &self.update_id)
            .finish_non_exhaustive()
    }
}

impl<W: HasWorkflowDefinition, U: UpdateDefinition<Workflow = W::Run>>
    UpdateWithStartRequest<W, U>
{
    #[must_use]
    pub fn new(start: StartRequest<W>, update: U, update_id: UpdateId, input: U::Input) -> Self {
        Self {
            start,
            update,
            update_id,
            input,
        }
    }
}

fn to_memo_values(memo: BTreeMap<String, serde_json::Value>) -> MemoValues {
    let mut values = MemoValues::new();
    for (key, value) in memo {
        values.insert(key, value);
    }
    values
}

fn bounded_rpc_options(remaining: Duration, retry_allowed: bool) -> RpcOptions {
    let retry_options = if retry_allowed {
        RetryOptions::builder()
            .initial_interval(remaining.min(DEFAULT_RETRY_INITIAL_INTERVAL))
            .max_interval(remaining.min(DEFAULT_RETRY_MAX_INTERVAL))
            .max_elapsed_time(Some(remaining))
            .build()
    } else {
        RetryOptions::no_retries()
    };
    RpcOptions::builder()
        .timeout(remaining)
        .retry_options(retry_options)
        .build()
}

fn long_poll_rpc_options(remaining: Option<Duration>) -> RpcOptions {
    let Some(remaining) = remaining else {
        return RpcOptions::default();
    };
    let retry_options = RetryOptions::builder()
        .initial_interval(remaining.min(DEFAULT_RETRY_INITIAL_INTERVAL))
        .max_interval(remaining.min(DEFAULT_RETRY_MAX_INTERVAL))
        .max_elapsed_time(Some(remaining))
        .build();
    RpcOptions::builder()
        .timeout(remaining.min(LONG_POLL_RPC_SLICE))
        .retry_options(retry_options)
        .build()
}

#[derive(Clone)]
pub struct TemporalClient {
    raw: Client,
    rpc_timeout: Duration,
}

impl fmt::Debug for TemporalClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalClient")
            .field("namespace", &self.raw.options().namespace)
            .field("rpc_timeout", &self.rpc_timeout)
            .finish()
    }
}

impl TemporalClient {
    pub(crate) fn new(raw: Client, rpc_timeout: Duration) -> Self {
        Self { raw, rpc_timeout }
    }

    pub async fn start_workflow<W: HasWorkflowDefinition>(
        &self,
        op: &OperationContext,
        req: StartRequest<W>,
    ) -> Result<WorkflowHandle<W>, StartWorkflowError>
    where
        W::Input: Send,
    {
        op.check()
            .map_err(|stopped| StartWorkflowError::NotApplied(StartFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(StartWorkflowError::NotApplied(StartFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let rpc_options = bounded_rpc_options(bounded.remaining(), true);
        let (workflow, input, options) = req.into_sdk_parts(rpc_options);
        let future = self.raw.start_workflow(workflow, input, options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(StartWorkflowError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => match result {
                Ok(handle) => Ok(WorkflowHandle::from_sdk(handle, self.rpc_timeout)),
                Err(error) => Err(map_start_error(&error)),
            }
        }
    }

    pub async fn signal_with_start<
        W: HasWorkflowDefinition,
        S: SignalDefinition<Workflow = W::Run>,
    >(
        &self,
        op: &OperationContext,
        req: StartRequest<W>,
        signal: S,
        input: S::Input,
    ) -> Result<WorkflowHandle<W>, StartWorkflowError>
    where
        W::Input: Send,
        S::Input: Send,
    {
        op.check()
            .map_err(|stopped| StartWorkflowError::NotApplied(StartFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(StartWorkflowError::NotApplied(StartFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let rpc_options = bounded_rpc_options(bounded.remaining(), false);
        let (workflow, workflow_input, options) = req.into_sdk_parts(rpc_options);
        let future =
            self.raw
                .signal_with_start_workflow(workflow, workflow_input, signal, input, options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(StartWorkflowError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => match result {
                Ok(handle) => Ok(WorkflowHandle::from_sdk(handle, self.rpc_timeout)),
                Err(error) => Err(map_start_error(&error)),
            }
        }
    }

    pub async fn execute_update_with_start<
        W: HasWorkflowDefinition,
        U: UpdateDefinition<Workflow = W::Run>,
    >(
        &self,
        op: &OperationContext,
        req: UpdateWithStartRequest<W, U>,
    ) -> Result<U::Output, UpdateWorkflowError>
    where
        W::Input: Send,
        U::Input: Send,
    {
        op.check()
            .map_err(|stopped| UpdateWorkflowError::NotApplied(UpdateFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(UpdateWorkflowError::NotApplied(UpdateFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let rpc_options = bounded_rpc_options(bounded.remaining(), true);
        let (id_reuse_policy, id_conflict_policy) = req.start.id_policy.into_sdk_policies();
        let memo = req.start.memo.map(to_memo_values);
        let options = WorkflowUpdateWithStartOptions::new(
            req.start.task_queue.into_inner(),
            req.start.workflow_id.into_inner(),
            id_conflict_policy,
        )
        .id_reuse_policy(id_reuse_policy)
        .maybe_execution_timeout(req.start.execution_timeout)
        .maybe_run_timeout(req.start.run_timeout)
        .maybe_task_timeout(req.start.task_timeout)
        .maybe_retry_policy(req.start.retry_policy)
        .maybe_memo(memo)
        .maybe_search_attributes(req.start.search_attributes)
        .maybe_static_summary(req.start.static_summary)
        .maybe_static_details(req.start.static_details)
        .update_id(req.update_id.into_inner())
        .rpc_options(rpc_options)
        .build();
        let future = self.raw.execute_update_with_start_workflow(
            req.start.workflow,
            req.start.input,
            req.update,
            req.input,
            options,
        );
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(UpdateWorkflowError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => result.map_err(|error| map_update_with_start_error(&error)),
        }
    }

    #[must_use]
    pub fn workflow_handle<W: HasWorkflowDefinition>(
        &self,
        workflow_id: WorkflowId,
        run_id: Option<RunId>,
    ) -> WorkflowHandle<W> {
        let inner = match run_id {
            None => self.raw.get_workflow_handle::<W>(workflow_id.into_inner()),
            Some(run_id) => temporalio_client::WorkflowHandle::new(
                self.raw.clone(),
                WorkflowExecutionInfo::builder()
                    .namespace(self.raw.options().namespace.clone())
                    .workflow_id(workflow_id.into_inner())
                    .run_id(run_id.into_inner())
                    .build(),
            ),
        };
        WorkflowHandle::from_sdk(inner, self.rpc_timeout)
    }
}

pub struct WorkflowHandle<W: HasWorkflowDefinition> {
    inner: temporalio_client::WorkflowHandle<Client, W>,
    rpc_timeout: Duration,
}

impl<W: HasWorkflowDefinition> Clone for WorkflowHandle<W> {
    fn clone(&self) -> Self {
        Self {
            inner: temporalio_client::WorkflowHandle::new(
                self.inner.client().clone(),
                self.inner.info().clone(),
            ),
            rpc_timeout: self.rpc_timeout,
        }
    }
}

impl<W: HasWorkflowDefinition> fmt::Debug for WorkflowHandle<W> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowHandle")
            .field("workflow_id", &self.workflow_id())
            .field("run_id", &self.run_id())
            .finish()
    }
}

impl<W: HasWorkflowDefinition> WorkflowHandle<W> {
    pub(crate) fn from_sdk(
        inner: temporalio_client::WorkflowHandle<Client, W>,
        rpc_timeout: Duration,
    ) -> Self {
        Self { inner, rpc_timeout }
    }

    #[must_use]
    pub fn workflow_id(&self) -> &str {
        self.inner.info().workflow_id.as_str()
    }

    #[must_use]
    pub fn run_id(&self) -> Option<&str> {
        self.inner.run_id()
    }

    pub async fn signal<S: SignalDefinition<Workflow = W::Run>>(
        &self,
        op: &OperationContext,
        signal: S,
        input: S::Input,
        dedupe: MutationDedupe,
    ) -> Result<(), MutationError>
    where
        S::Input: Send,
    {
        op.check()
            .map_err(|stopped| MutationError::NotApplied(MutationFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(MutationError::NotApplied(MutationFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let (request_id, retry_allowed) = match dedupe {
            MutationDedupe::Key(key) => (Some(key.into_inner()), true),
            MutationDedupe::Unkeyed => (None, false),
        };
        let options = WorkflowSignalOptions::builder()
            .maybe_request_id(request_id)
            .rpc_options(bounded_rpc_options(bounded.remaining(), retry_allowed))
            .build();
        let future = self.inner.signal(signal, input, options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(MutationError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => result.map_err(|error| map_mutation_error(&error)),
        }
    }

    pub async fn query<Q: QueryDefinition<Workflow = W::Run>>(
        &self,
        op: &OperationContext,
        query: Q,
        input: Q::Input,
    ) -> Result<Q::Output, ReadError>
    where
        Q::Input: Send,
    {
        op.check().map_err(ReadError::Stopped)?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(ReadError::Stopped(bounded_stop_to_stopped(stop)));
        }
        let options = WorkflowQueryOptions::builder()
            .rpc_options(bounded_rpc_options(bounded.remaining(), true))
            .build();
        let future = self.inner.query(query, input, options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(ReadError::Stopped(bounded_stop_to_stopped(stop))),
            result = future => result.map_err(|error| map_query_error(&error)),
        }
    }

    pub async fn execute_update<U: UpdateDefinition<Workflow = W::Run>>(
        &self,
        op: &OperationContext,
        update: U,
        update_id: UpdateId,
        input: U::Input,
    ) -> Result<U::Output, UpdateWorkflowError>
    where
        U::Input: Send,
    {
        op.check()
            .map_err(|stopped| UpdateWorkflowError::NotApplied(UpdateFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(UpdateWorkflowError::NotApplied(UpdateFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let options = WorkflowExecuteUpdateOptions::builder()
            .update_id(update_id.into_inner())
            .rpc_options(bounded_rpc_options(bounded.remaining(), true))
            .build();
        let future = self.inner.execute_update(update, input, options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(UpdateWorkflowError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => result.map_err(|error| map_update_error(&error)),
        }
    }

    pub async fn cancel(
        &self,
        op: &OperationContext,
        dedupe: MutationDedupe,
        reason: Option<CompactString>,
    ) -> Result<(), MutationError> {
        op.check()
            .map_err(|stopped| MutationError::NotApplied(MutationFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(MutationError::NotApplied(MutationFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let (request_id, retry_allowed) = match dedupe {
            MutationDedupe::Key(key) => (Some(key.into_inner()), true),
            MutationDedupe::Unkeyed => (None, false),
        };
        let options = WorkflowCancelOptions::builder()
            .reason(reason.map_or_else(String::new, String::from))
            .maybe_request_id(request_id)
            .rpc_options(bounded_rpc_options(bounded.remaining(), retry_allowed))
            .build();
        let future = self.inner.cancel(options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(MutationError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => result.map_err(|error| map_mutation_error(&error)),
        }
    }

    pub async fn terminate(
        &self,
        op: &OperationContext,
        reason: Option<CompactString>,
    ) -> Result<(), MutationError> {
        op.check()
            .map_err(|stopped| MutationError::NotApplied(MutationFailure::Stopped(stopped)))?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(MutationError::NotApplied(MutationFailure::Stopped(
                bounded_stop_to_stopped(stop),
            )));
        }
        let options = WorkflowTerminateOptions::builder()
            .reason(reason.map_or_else(String::new, String::from))
            .rpc_options(bounded_rpc_options(bounded.remaining(), false))
            .build();
        let future = self.inner.terminate(options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(MutationError::OutcomeUnknown(
                UnknownOutcome::Stopped(bounded_stop_to_stopped(stop)),
            )),
            result = future => result.map_err(|error| map_mutation_error(&error)),
        }
    }

    pub async fn describe(
        &self,
        op: &OperationContext,
    ) -> Result<WorkflowExecutionDescription, ReadError> {
        op.check().map_err(ReadError::Stopped)?;
        let bounded = op.bounded(self.rpc_timeout);
        if let Err(stop) = bounded.check() {
            return Err(ReadError::Stopped(bounded_stop_to_stopped(stop)));
        }
        let options = WorkflowDescribeOptions::builder()
            .rpc_options(bounded_rpc_options(bounded.remaining(), true))
            .build();
        let future = self.inner.describe(options);
        tokio::select! {
            biased;
            stop = bounded.wait_stopped() => Err(ReadError::Stopped(bounded_stop_to_stopped(stop))),
            result = future => result.map_err(|error| map_describe_error(&error)),
        }
    }

    pub async fn get_result(
        &self,
        op: &OperationContext,
    ) -> Result<W::Output, WorkflowResultError> {
        op.check().map_err(WorkflowResultError::Stopped)?;
        let options = WorkflowGetResultOptions::builder()
            .rpc_options(long_poll_rpc_options(op.remaining()))
            .build();
        match op.run(self.inner.get_result(options)).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(map_result_error(&error)),
            Err(stopped) => Err(WorkflowResultError::Stopped(stopped)),
        }
    }
}
