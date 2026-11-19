use operation_context::{Deadline, OperationContext, Stopped};
use smallvec::SmallVec;

#[derive(Clone, Debug)]
pub struct RequestContext {
    operation: OperationContext,
    extension_contexts: SmallVec<[OperationContext; 1]>,
}

impl From<OperationContext> for RequestContext {
    fn from(operation: OperationContext) -> Self {
        Self {
            operation,
            extension_contexts: SmallVec::new(),
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
        extension_context: Option<OperationContext>,
    ) -> Self {
        if let Some(extension_context) = extension_context {
            ceiling = extension_context.effective_deadline(ceiling);
            self.extension_contexts.push(extension_context);
        }
        self.operation = self.operation.child_until(ceiling);
        self
    }

    #[must_use]
    pub fn child_until(&self, ceiling: Deadline) -> Self {
        Self {
            operation: self.operation.child_until(ceiling),
            extension_contexts: self.extension_contexts.clone(),
        }
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Deadline> {
        self.operation.deadline()
    }

    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        self.operation.stopped().or_else(|| {
            self.extension_contexts
                .iter()
                .find_map(OperationContext::stopped)
        })
    }

    pub fn check(&self) -> Result<(), Stopped> {
        self.stopped().map_or(Ok(()), Err)
    }

    pub async fn wait_stopped(&self) -> Stopped {
        let extension_stopped = async {
            match self.extension_contexts.as_slice() {
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
            stopped = extension_stopped => stopped,
        }
    }
}
