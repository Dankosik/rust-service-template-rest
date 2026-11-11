//! One call's budget: the SDK timeout and retry policy for a read or a
//! mutation, and the poll that stops when the operation context does.

use std::time::Duration;

use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use operation_context::{OperationContext, Stopped};

use crate::ObjectStorageError;
use crate::error::{Call, Failure, FailureKind, classify};
use crate::observe::OperationGuard;

/// Standard retry for reads. A put or delete makes one attempt: the SDK
/// keeps only the last attempt's reply, so after a retry a refusal could
/// hide an earlier attempt that applied the mutation, and a create-only
/// retry could meet this call's own lost success as `AlreadyExists`.
const MAX_ATTEMPTS: u32 = 3;
/// Cap on one jittered retry delay, so three attempts fit an interactive
/// `operation_timeout`. The SDK default of 20 s would outlast it.
const MAX_BACKOFF: Duration = Duration::from_secs(1);
/// TCP and TLS connect bound per attempt; the SDK default of the pinned
/// behavior version, stated so an SDK bump cannot move it. The attempt bound
/// caps it when that is shorter.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3100);
/// A read attempt is bounded at `operation_timeout / READ_ATTEMPT_SHARE`, so
/// one attempt that hangs before its response headers leaves room for a
/// retry and its backoff. The SDK sets no attempt bound itself.
const READ_ATTEMPT_SHARE: u32 = 2;

/// The one timeout and retry policy for a read or mutation call.
pub(crate) fn call_policy(
    call: Call,
    remaining: Duration,
    ceiling: Duration,
) -> (TimeoutConfig, RetryConfig) {
    let attempt = match call {
        Call::Read => remaining.min(ceiling / READ_ATTEMPT_SHARE),
        Call::Mutation | Call::CreateOnly => remaining,
    };
    let timeout = TimeoutConfig::builder()
        .operation_timeout(remaining)
        .operation_attempt_timeout(attempt)
        .connect_timeout(CONNECT_TIMEOUT.min(attempt))
        .build();
    let retry = match call {
        Call::Read => RetryConfig::standard()
            .with_max_attempts(MAX_ATTEMPTS)
            .with_max_backoff(MAX_BACKOFF),
        Call::Mutation | Call::CreateOnly => RetryConfig::standard().with_max_attempts(1),
    };
    (timeout, retry)
}

/// Poll only while a context is live. A definitive mutation result from a
/// poll that began live remains internally authoritative even when that poll
/// crosses the cooperative cutoff. Read callers perform their strict postcheck
/// before transferring or returning newly observed data.
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

/// The context stopped [`within`] before the future finished.
pub(crate) struct Interrupted {
    pub(crate) stopped: Stopped,
    /// The future was polled at least once, so a request may have been sent.
    polled: bool,
}

impl Interrupted {
    /// Before the first poll nothing was sent; after it, the reply is lost.
    pub(crate) fn fail(self, guard: &mut OperationGuard, call: Call) -> ObjectStorageError {
        let mut failure = Failure::stopped(self.stopped);
        if self.polled {
            failure.error = classify(call, FailureKind::Lost);
        }
        guard.fail(&failure)
    }
}
