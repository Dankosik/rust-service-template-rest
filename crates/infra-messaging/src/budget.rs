use std::time::Duration;

use operation_context::{Deadline, OperationContext, Stopped};
use tokio_util::sync::CancellationToken;

use crate::error::MessagingError;

pub const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(crate) struct BrokerBudget<'a> {
    deadline: Deadline,
    cancellation: &'a CancellationToken,
}

impl<'a> BrokerBudget<'a> {
    pub(crate) fn from_context(context: &'a OperationContext) -> Self {
        Self::new(
            context.effective_deadline(Deadline::after(BROKER_OPERATION_BUDGET)),
            context.cancellation(),
        )
    }

    pub(crate) const fn new(deadline: Deadline, cancellation: &'a CancellationToken) -> Self {
        Self {
            deadline,
            cancellation,
        }
    }

    pub(crate) const fn deadline(self) -> Deadline {
        self.deadline
    }

    pub(crate) fn remaining(self) -> Duration {
        self.deadline.remaining()
    }

    pub(crate) fn stopped(self) -> Option<Stopped> {
        if self.deadline.expired() {
            Some(Stopped::Deadline)
        } else if self.cancellation.is_cancelled() {
            Some(Stopped::Cancelled)
        } else {
            None
        }
    }

    pub(crate) async fn wait_stopped(self) -> Stopped {
        tokio::select! {
            biased;
            () = self.deadline.wait() => Stopped::Deadline,
            () = self.cancellation.cancelled() => Stopped::Cancelled,
        }
    }

    pub(crate) async fn run_until_stopped<T>(
        self,
        operation: impl Future<Output = T>,
    ) -> Result<T, Stopped> {
        tokio::select! {
            biased;
            reason = self.wait_stopped() => Err(reason),
            output = operation => Ok(output),
        }
    }
}

pub(crate) fn bounded_operation<T>(
    context: &OperationContext,
    operation: impl Future<Output = Result<T, MessagingError>>,
) -> impl Future<Output = Result<T, MessagingError>> {
    bounded_with(BrokerBudget::from_context(context), operation)
}

pub(crate) async fn bounded_with<T>(
    budget: BrokerBudget<'_>,
    operation: impl Future<Output = Result<T, MessagingError>>,
) -> Result<T, MessagingError> {
    let remaining = budget.remaining();
    if remaining.is_zero() {
        return Err(MessagingError::TimedOut { budget: remaining });
    }
    tokio::select! {
        biased;
        reason = budget.wait_stopped() => Err(match reason {
            Stopped::Deadline => MessagingError::TimedOut { budget: remaining },
            Stopped::Cancelled => MessagingError::Cancelled,
        }),
        result = operation => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_timed_out_operation_reports_the_shorter_caller_deadline() {
        let caller = Duration::from_millis(100);
        let result = bounded_operation(
            &OperationContext::with_timeout(caller),
            std::future::pending::<Result<(), MessagingError>>(),
        )
        .await;
        std::assert_matches!(result, Err(MessagingError::TimedOut { budget }) if budget == caller,
            "{result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_cancellation_reports_the_elapsed_operation_deadline() {
        let budget = Duration::from_millis(100);
        let cancel = CancellationToken::new();
        let context = OperationContext::new(Some(Deadline::after(budget)), cancel.clone());
        let mut operation = std::pin::pin!(bounded_operation(
            &context,
            std::future::pending::<Result<(), MessagingError>>(),
        ));
        assert!(futures_util::poll!(operation.as_mut()).is_pending());
        tokio::time::advance(budget).await;
        cancel.cancel();

        let result = operation.await;
        std::assert_matches!(result, Err(MessagingError::TimedOut { budget: elapsed }) if elapsed == budget);
    }

    #[tokio::test(start_paused = true)]
    async fn broker_budget_clamps_to_five_seconds_and_prefers_deadline_over_cancellation() {
        let cancel = CancellationToken::new();
        let long = OperationContext::new(
            Some(Deadline::after(Duration::from_secs(30))),
            cancel.clone(),
        );
        let clamped = BrokerBudget::from_context(&long);
        assert_eq!(clamped.remaining(), BROKER_OPERATION_BUDGET);
        assert_eq!(clamped.stopped(), None);

        cancel.cancel();
        assert_eq!(clamped.stopped(), Some(Stopped::Cancelled));
        assert_eq!(clamped.wait_stopped().await, Stopped::Cancelled);

        tokio::time::advance(BROKER_OPERATION_BUDGET).await;
        assert_eq!(clamped.stopped(), Some(Stopped::Deadline));
        assert_eq!(clamped.wait_stopped().await, Stopped::Deadline);
    }
}
