use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::future::BoxFuture;
use futures_util::stream::StreamExt;
use operation_context::Deadline;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio_util::sync::CancellationToken;

use crate::error::{ErrorType, error_type};

pub(crate) struct AbandonedReply {
    reservation: OwnedSemaphorePermit,
    reply: BoxFuture<'static, ()>,
}

impl AbandonedReply {
    async fn wait(self) {
        let _reservation = self.reservation;
        self.reply.await;
    }
}

type QueryFuture<T> = BoxFuture<'static, redis::RedisResult<T>>;

struct InFlight<T> {
    reply: QueryFuture<T>,
    reservation: OwnedSemaphorePermit,
}

pub(crate) struct Exchange<'a, T: Send + 'static> {
    retired: &'a CancellationToken,
    abandoned: &'a mpsc::UnboundedSender<AbandonedReply>,
    in_flight: Option<InFlight<T>>,
    wire_deadline: Deadline,
}

impl<'a, T: Send + 'static> Exchange<'a, T> {
    pub(crate) fn new(
        retired: &'a CancellationToken,
        abandoned: &'a mpsc::UnboundedSender<AbandonedReply>,
        reservation: OwnedSemaphorePermit,
        reply: impl Future<Output = redis::RedisResult<T>> + Send + 'static,
        wire_deadline: Deadline,
    ) -> Self {
        Self {
            retired,
            abandoned,
            in_flight: Some(InFlight {
                reply: Box::pin(reply),
                reservation,
            }),
            wire_deadline,
        }
    }
}

impl<T: Send + 'static> Future for Exchange<'_, T> {
    type Output = redis::RedisResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(mut in_flight) = self.in_flight.take() else {
            return Poll::Pending;
        };
        match in_flight.reply.as_mut().poll(cx) {
            Poll::Ready(result) => {
                if reply_requires_retirement(&result) {
                    self.retired.cancel();
                }
                Poll::Ready(result)
            }
            Poll::Pending => {
                self.in_flight = Some(in_flight);
                Poll::Pending
            }
        }
    }
}

impl<T: Send + 'static> Drop for Exchange<'_, T> {
    fn drop(&mut self) {
        let Some(InFlight { reply, reservation }) = self.in_flight.take() else {
            return;
        };
        let wire_deadline = self.wire_deadline;
        let retired = self.retired.clone();
        let _ = self.abandoned.send(AbandonedReply {
            reservation,
            reply: Box::pin(async move {
                tokio::select! {
                    biased;
                    result = reply => {
                        if reply_requires_retirement(&result) {
                            retired.cancel();
                        }
                    }
                    () = wire_deadline.wait() => {
                        retired.cancel();
                    }
                }
            }),
        });
    }
}

fn reply_requires_retirement<T>(reply: &redis::RedisResult<T>) -> bool {
    reply.as_ref().is_err_and(|error| {
        error_type(error) != ErrorType::ServerErrorReply || error.code() == Some("READONLY")
    })
}

pub(crate) async fn drain_abandoned_replies(mut replies: mpsc::UnboundedReceiver<AbandonedReply>) {
    futures_util::stream::poll_fn(|cx| replies.poll_recv(cx))
        .for_each_concurrent(None, AbandonedReply::wait)
        .await;
}

pub(crate) fn reply_data(mut reply: redis::Value) -> redis::RedisResult<redis::Value> {
    while let redis::Value::Attribute { data, .. } = reply {
        reply = *data;
    }
    match reply {
        redis::Value::ServerError(error) => Err(error.into()),
        data => Ok(data),
    }
}
