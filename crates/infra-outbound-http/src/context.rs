use operation_context::{Deadline, OperationContext, Stopped};
use smallvec::SmallVec;

#[derive(Clone, Debug)]
pub struct RequestContext {
    operation: OperationContext,
    request_contexts: SmallVec<[OperationContext; 1]>,
}

impl From<OperationContext> for RequestContext {
    fn from(operation: OperationContext) -> Self {
        Self {
            operation,
            request_contexts: SmallVec::new(),
        }
    }
}

impl From<&OperationContext> for RequestContext {
    fn from(operation: &OperationContext) -> Self {
        operation.clone().into()
    }
}

impl RequestContext {
    pub(super) fn for_request(
        mut self,
        mut ceiling: Deadline,
        request_context: Option<OperationContext>,
    ) -> Self {
        if let Some(request_context) = request_context {
            ceiling = request_context.effective_deadline(ceiling);
            self.request_contexts.push(request_context);
        }
        self.operation = self.operation.child_until(ceiling);
        self
    }

    #[must_use]
    pub fn child_until(&self, ceiling: Deadline) -> Self {
        Self {
            operation: self.operation.child_until(ceiling),
            request_contexts: self.request_contexts.clone(),
        }
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Deadline> {
        self.operation.deadline()
    }

    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        self.operation.stopped().or_else(|| {
            self.request_contexts
                .iter()
                .find_map(OperationContext::stopped)
        })
    }

    pub fn check(&self) -> Result<(), Stopped> {
        self.stopped().map_or(Ok(()), Err)
    }

    pub async fn wait_stopped(&self) -> Stopped {
        let request_stopped = async {
            match self.request_contexts.as_slice() {
                [] => std::future::pending().await,
                [context] => context.wait_stopped().await,
                contexts => {
                    futures_util::future::select_all(
                        contexts
                            .iter()
                            .map(|context| Box::pin(context.wait_stopped())),
                    )
                    .await
                    .0
                }
            }
        };
        tokio::select! {
            biased;
            stopped = self.operation.wait_stopped() => stopped,
            stopped = request_stopped => stopped,
        }
    }
}
