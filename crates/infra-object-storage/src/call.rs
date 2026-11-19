use std::time::Duration;

use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::time::Instant;

use crate::error::Call;

const MAX_ATTEMPTS: u32 = 3;
const MAX_BACKOFF: Duration = Duration::from_secs(1);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(3100);
const READ_ATTEMPT_DIVISOR: u32 = 2;

pub(crate) fn call_policy(
    call: Call,
    remaining: Duration,
    ceiling: Duration,
) -> (TimeoutConfig, RetryConfig) {
    let (attempt_timeout, retry_config) = match call {
        Call::Read => (
            remaining.min(ceiling / READ_ATTEMPT_DIVISOR),
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
    context: &CallContext,
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
    context: &CallContext,
    future: impl std::future::Future<Output = T>,
) -> Result<(T, Option<CallStop>), Interrupted> {
    within(context, async {
        let reply = future.await;
        (reply, context.stopped())
    })
    .await
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Interrupted {
    pub(crate) stopped: CallStop,
    pub(crate) polled: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CallStop {
    Parent(Stopped),
    Timeout,
}

#[derive(Clone, Debug)]
pub(crate) struct CallContext {
    context: OperationContext,
    parent_deadline: bool,
}

impl CallContext {
    pub(crate) fn new(parent: &OperationContext, timeout: Duration) -> Self {
        let now = Instant::now();
        let (deadline, parent_deadline) = match parent.deadline() {
            Some(deadline) if deadline.remaining_at(now) <= timeout => (deadline, true),
            _ => (Deadline::new(now, timeout), false),
        };
        Self {
            context: OperationContext::new(Some(deadline), parent.cancellation().child_token()),
            parent_deadline,
        }
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.context.remaining()
    }

    pub(crate) fn stopped(&self) -> Option<CallStop> {
        self.context.stopped().map(|stopped| self.classify(stopped))
    }

    pub(crate) async fn wait_stopped(&self) -> CallStop {
        self.classify(self.context.wait_stopped().await)
    }

    fn classify(&self, stopped: Stopped) -> CallStop {
        match stopped {
            Stopped::Deadline if !self.parent_deadline => CallStop::Timeout,
            _ => CallStop::Parent(stopped),
        }
    }
}
