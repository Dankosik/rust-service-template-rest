//! Bounds on broker round trips.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::error::MessagingError;

/// Bounds every broker round trip, inside any longer caller deadline.
pub const BROKER_OPERATION_BUDGET: Duration = Duration::from_secs(5);

/// The earlier of `deadline` and one [`BROKER_OPERATION_BUDGET`] from now.
pub(crate) fn within_operation_budget(deadline: Instant) -> Instant {
    deadline.min(Instant::now() + BROKER_OPERATION_BUDGET)
}

/// Runs one broker operation within [`within_operation_budget`] of `deadline`,
/// unless `cancel` fires first.
pub(crate) async fn bounded_operation<T>(
    deadline: Instant,
    cancel: &CancellationToken,
    operation: impl Future<Output = Result<T, MessagingError>>,
) -> Result<T, MessagingError> {
    let deadline = within_operation_budget(deadline);
    let budget = deadline.saturating_duration_since(Instant::now());
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(MessagingError::Cancelled),
        () = tokio::time::sleep_until(deadline) => Err(MessagingError::TimedOut { budget }),
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
            Instant::now() + caller,
            &CancellationToken::new(),
            std::future::pending::<Result<(), MessagingError>>(),
        )
        .await;
        assert!(
            matches!(result, Err(MessagingError::TimedOut { budget }) if budget == caller),
            "{result:?}"
        );
    }
}
