use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::stream::StreamExt;
use operation_context::Deadline;
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use crate::connection::Retirement;
use crate::error::{ErrorType, error_type};

pub(crate) trait AbandonedFuture: Send {
    fn poll_requires_retirement(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool>;
}

impl<T, F> AbandonedFuture for F
where
    F: Future<Output = redis::RedisResult<T>> + Send,
{
    fn poll_requires_retirement(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<bool> {
        self.poll(cx)
            .map(|result| reply_requires_retirement(&result))
    }
}

pub(crate) struct AbandonedReply {
    reservation: OwnedSemaphorePermit,
    reply: Pin<Box<dyn AbandonedFuture>>,
    retired: Retirement,
    wire_deadline: Deadline,
}

impl AbandonedReply {
    async fn wait(mut self) {
        let _reservation = self.reservation;
        let should_retire = tokio::select! {
            biased;
            retire = std::future::poll_fn(|cx| self.reply.as_mut().poll_requires_retirement(cx)) => {
                retire
            }
            () = self.wire_deadline.wait() => true,
        };
        if should_retire {
            self.retired.retire();
        }
    }
}

struct InFlight<F> {
    reply: Pin<Box<F>>,
    reservation: OwnedSemaphorePermit,
}

pub(crate) struct Exchange<'a, F: AbandonedFuture + 'static> {
    retired: &'a Retirement,
    abandoned: &'a mpsc::UnboundedSender<AbandonedReply>,
    in_flight: Option<InFlight<F>>,
    wire_deadline: Deadline,
}

impl<'a, F, T> Exchange<'a, F>
where
    F: Future<Output = redis::RedisResult<T>> + Send + 'static,
{
    pub(crate) fn new(
        retired: &'a Retirement,
        abandoned: &'a mpsc::UnboundedSender<AbandonedReply>,
        reservation: OwnedSemaphorePermit,
        reply: F,
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

impl<F, T> Future for Exchange<'_, F>
where
    F: Future<Output = redis::RedisResult<T>> + Send + 'static,
{
    type Output = redis::RedisResult<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let Some(mut in_flight) = self.in_flight.take() else {
            return Poll::Pending;
        };
        match in_flight.reply.as_mut().poll(cx) {
            Poll::Ready(result) => {
                if reply_requires_retirement(&result) {
                    self.retired.retire();
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

impl<F: AbandonedFuture + 'static> Drop for Exchange<'_, F> {
    fn drop(&mut self) {
        let Some(InFlight { reply, reservation }) = self.in_flight.take() else {
            return;
        };
        let _ = self.abandoned.send(AbandonedReply {
            reservation,
            reply,
            retired: self.retired.clone(),
            wire_deadline: self.wire_deadline,
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

pub(crate) async fn send_accepted(
    connection: &mut redis::aio::MultiplexedConnection,
    command: &redis::Cmd,
    accepts: fn(redis::Value) -> bool,
) -> redis::RedisResult<()> {
    let reply = tokio::task::unconstrained(connection.send_packed_command(command)).await;
    accepts(reply.and_then(reply_data)?)
        .ok_or_else(|| (redis::ErrorKind::Parse, "unexpected cache reply").into())
}
