use std::time::Duration;

use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use operation_context::{BoundedContext, BoundedStop};

use crate::error::{Failure, OutcomeError};

const MAX_ATTEMPTS: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3100);
const READ_ATTEMPT_DIVISOR: u32 = 2;

#[derive(Clone, Copy)]
pub(crate) enum RetryPolicy {
    Read,
    SingleAttempt,
}

pub(crate) fn read_retry_config() -> RetryConfig {
    RetryConfig::standard()
        .with_max_attempts(MAX_ATTEMPTS)
        .with_max_backoff(MAX_BACKOFF)
}

pub(crate) fn single_attempt_retry_config() -> RetryConfig {
    RetryConfig::standard().with_max_attempts(1)
}

pub(crate) fn call_timeout(
    policy: RetryPolicy,
    remaining: Duration,
    ceiling: Duration,
) -> TimeoutConfig {
    let attempt_timeout = match policy {
        RetryPolicy::Read => remaining.min(ceiling / READ_ATTEMPT_DIVISOR),
        RetryPolicy::SingleAttempt => remaining,
    };
    TimeoutConfig::builder()
        .operation_timeout(remaining)
        .operation_attempt_timeout(attempt_timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(attempt_timeout))
        .build()
}

pub(crate) fn check_active<E: OutcomeError>(context: &BoundedContext) -> Result<(), Failure<E>> {
    context.check().map_err(Failure::not_applied_stop)
}

pub(crate) async fn within<T>(
    context: &BoundedContext,
    future: impl std::future::Future<Output = T>,
) -> Result<T, Interrupted> {
    use std::future::Future as _;

    let mut future = std::pin::pin!(future);
    let mut stopped = std::pin::pin!(context.wait_stopped());
    let mut polled = false;
    std::future::poll_fn(|cx| {
        if let Some(reason) = context.stopped() {
            return std::task::Poll::Ready(Err(Interrupted {
                stopped: reason,
                polled,
            }));
        }
        if let std::task::Poll::Ready(reason) = stopped.as_mut().poll(cx) {
            return std::task::Poll::Ready(Err(Interrupted {
                stopped: reason,
                polled,
            }));
        }
        polled = true;
        future.as_mut().poll(cx).map(Ok)
    })
    .await
}

pub(crate) async fn within_mutation<T>(
    context: &BoundedContext,
    future: impl std::future::Future<Output = T>,
) -> Result<(T, Option<BoundedStop>), Interrupted> {
    let mut future = std::pin::pin!(future);
    within(context, async {
        let reply = future.as_mut().await;
        (reply, context.stopped())
    })
    .await
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Interrupted {
    pub(crate) stopped: BoundedStop,
    pub(crate) polled: bool,
}
