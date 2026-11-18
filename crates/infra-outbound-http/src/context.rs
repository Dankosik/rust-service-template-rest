use operation_context::{Deadline, OperationContext, Stopped};
use smallvec::SmallVec;

#[derive(Clone, Debug)]
pub struct RequestContext {
    operation: OperationContext,
    requests: SmallVec<[OperationContext; 1]>,
}

impl From<OperationContext> for RequestContext {
    fn from(operation: OperationContext) -> Self {
        Self {
            operation,
            requests: SmallVec::new(),
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
        request: Option<OperationContext>,
    ) -> Self {
        if let Some(request) = request {
            if let Some(deadline) = request.deadline() {
                ceiling = ceiling.earlier(deadline);
            }
            self.requests.push(request);
        }
        self.operation = self.operation.child_until(ceiling);
        self
    }

    #[must_use]
    pub fn child_until(&self, ceiling: Deadline) -> Self {
        Self {
            operation: self.operation.child_until(ceiling),
            requests: self.requests.clone(),
        }
    }

    #[must_use]
    pub const fn deadline(&self) -> Option<Deadline> {
        self.operation.deadline()
    }

    #[must_use]
    pub fn stopped(&self) -> Option<Stopped> {
        self.operation
            .stopped()
            .or_else(|| self.requests.iter().find_map(OperationContext::stopped))
    }

    pub fn check(&self) -> Result<(), Stopped> {
        self.stopped().map_or(Ok(()), Err)
    }

    pub async fn wait_stopped(&self) -> Stopped {
        let requests = async {
            match self.requests.as_slice() {
                [] => std::future::pending().await,
                [request] => request.wait_stopped().await,
                requests => {
                    futures_util::future::select_all(
                        requests
                            .iter()
                            .map(|request| Box::pin(request.wait_stopped())),
                    )
                    .await
                    .0
                }
            }
        };
        tokio::select! {
            biased;
            stopped = self.operation.wait_stopped() => stopped,
            stopped = requests => stopped,
        }
    }
}
