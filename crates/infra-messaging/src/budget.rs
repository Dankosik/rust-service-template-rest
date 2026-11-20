use std::time::Duration;

use operation_context::{Deadline, OperationContext, Stopped};

use crate::error::MessagingError;

pub const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);

pub(crate) async fn bounded_operation<T>(
    context: &OperationContext,
    operation: impl Future<Output = Result<T, MessagingError>>,
) -> Result<T, MessagingError> {
    let deadline = context.effective_deadline(Deadline::after(BROKER_OPERATION_BUDGET));
    let budget = deadline.remaining();
    if budget.is_zero() {
        return Err(MessagingError::TimedOut { budget });
    }
    let bounded = OperationContext::new(Some(deadline), context.cancellation().clone());
    tokio::select! {
        biased;
        reason = bounded.wait_stopped() => Err(match reason {
            Stopped::Deadline => MessagingError::TimedOut { budget },
            Stopped::Cancelled => MessagingError::Cancelled,
        }),
        result = operation => result,
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

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
}
