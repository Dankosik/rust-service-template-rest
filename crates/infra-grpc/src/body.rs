use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use tonic::Status;

use crate::call::CallState;

/// The response-body lifetime owner.  The actual tonic body is kept in the
/// call state so deadline, shutdown, and peer drop can take and drop it before
/// releasing admission.  This wrapper never buffers a business stream.
pub(crate) struct GuardedBody {
    state: Arc<CallState>,
    emitted_terminal: bool,
}

impl GuardedBody {
    pub(crate) fn new(
        state: Arc<CallState>,
        body: tonic::body::Body,
        initial_status: Option<tonic::Code>,
    ) -> Self {
        state.install_response_body(body);
        if let Some(code) = initial_status {
            state.finish_with_code(code);
        }
        Self {
            state,
            emitted_terminal: initial_status.is_some(),
        }
    }

    fn terminal_frame(&mut self, status: Status) -> Poll<Option<Result<Frame<Bytes>, Status>>> {
        if self.emitted_terminal {
            return Poll::Ready(None);
        }
        self.emitted_terminal = true;
        Poll::Ready(Some(Ok(Frame::trailers(status_trailers(status)))))
    }
}

impl Body for GuardedBody {
    type Data = Bytes;
    type Error = Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.emitted_terminal {
            return Poll::Ready(None);
        }
        self.state.register_body_waker(context.waker());
        if let Some(status) = self.state.validation_failure() {
            let status = self.state.finish_with_status(status);
            return self.terminal_frame(status);
        }
        if let Some(status) = self.state.terminal_status() {
            return self.terminal_frame(status);
        }

        let polled = crate::call::recover_poll(|| {
            CallState::scope_poll(Arc::clone(&self.state), || {
                self.state
                    .with_response_body(|body| Pin::new(body).poll_frame(context))
                    .unwrap_or(Poll::Ready(None))
            })
        });
        if let Some(status) = self.state.validation_failure() {
            let status = self.state.finish_with_status(status);
            return self.terminal_frame(status);
        }
        if let Some(status) = self.state.terminal_status() {
            return self.terminal_frame(status);
        }
        match polled {
            Err(()) => {
                let status = crate::status::panic_status();
                let status = self.state.finish_with_status(status);
                self.terminal_frame(status)
            }
            Ok(Poll::Ready(Some(Ok(frame)))) => {
                if let Some(trailers) = frame.trailers_ref() {
                    let code =
                        crate::observe::status_code(trailers).unwrap_or(tonic::Code::Unknown);
                    let status = self.state.finish_with_code(code);
                    if status.code() != code {
                        return self.terminal_frame(status);
                    }
                    self.emitted_terminal = true;
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Ok(Poll::Ready(Some(Err(status)))) => {
                let status = self.state.finish_with_status(status);
                self.terminal_frame(status)
            }
            Ok(Poll::Ready(None)) => {
                let status = self
                    .state
                    .finish_with_status(Status::unknown("missing response status"));
                self.terminal_frame(status)
            }
            Ok(Poll::Pending) => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.emitted_terminal
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

impl Drop for GuardedBody {
    fn drop(&mut self) {
        self.state
            .finish_with_status(Status::cancelled("request cancelled"));
    }
}

fn status_trailers(status: Status) -> HeaderMap {
    let (parts, ()) = status.into_http::<()>().into_parts();
    parts.headers
}
