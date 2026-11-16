use std::time::Duration;

use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use operation_context::{OperationContext, Stopped};

use crate::ObjectStorageError;
use crate::error::{Call, Failure};
use crate::observe::OperationGuard;

const MAX_ATTEMPTS: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3100);
const READ_ATTEMPT_SHARE: u32 = 2;

pub(crate) fn call_policy(
    call: Call,
    remaining: Duration,
    ceiling: Duration,
) -> (TimeoutConfig, RetryConfig) {
    let (attempt_timeout, retry_config) = match call {
        Call::Read => (
            remaining.min(ceiling / READ_ATTEMPT_SHARE),
            RetryConfig::standard()
                .with_max_attempts(MAX_ATTEMPTS)
                .with_max_backoff(MAX_BACKOFF),
        ),
        Call::UnconditionalMutation | Call::CreateOnly => {
            (remaining, RetryConfig::standard().with_max_attempts(1))
        }
    };
    let timeout = TimeoutConfig::builder()
        .operation_timeout(remaining)
        .operation_attempt_timeout(attempt_timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(attempt_timeout))
        .build();
    (timeout, retry_config)
}

pub(crate) async fn within<T>(
    context: &OperationContext,
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

pub(crate) struct Interrupted {
    pub(crate) stopped: Stopped,
    polled: bool,
}

impl Interrupted {
    pub(crate) fn fail(self, guard: &mut OperationGuard, call: Call) -> ObjectStorageError {
        let failure = if self.polled {
            Failure::stopped_after_poll(call, self.stopped)
        } else {
            Failure::stopped(self.stopped)
        };
        guard.fail(&failure)
    }
}
