use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use http_body::{Body as HttpBody, Frame, SizeHint};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BodyEnd {
    Complete,
    Error,
}

pub(crate) fn on_body_end<G, F>(
    body: Body,
    guard: Option<G>,
    remaining: Option<u64>,
    finish: F,
) -> Body
where
    G: Send + Unpin + 'static,
    F: FnOnce(BodyEnd) + Send + Unpin + 'static,
{
    if body.is_end_stream() {
        drop(guard);
        let end = match remaining {
            None | Some(0) => BodyEnd::Complete,
            Some(_) => BodyEnd::Error,
        };
        finish(end);
        return body;
    }
    let finish = if let Some(end) = body_end(false, remaining) {
        finish(end);
        None
    } else {
        Some(finish)
    };
    if guard.is_none() && finish.is_none() {
        return body;
    }
    Body::new(GuardedBody {
        body,
        guard,
        remaining,
        finish,
    })
}

fn body_end(end_stream: bool, remaining: Option<u64>) -> Option<BodyEnd> {
    match (end_stream, remaining) {
        (_, Some(0)) | (true, None) => Some(BodyEnd::Complete),
        (true, Some(_)) => Some(BodyEnd::Error),
        (false, _) => None,
    }
}

struct GuardedBody<G, F> {
    body: Body,
    guard: Option<G>,
    remaining: Option<u64>,
    finish: Option<F>,
}

impl<G: Unpin, F: FnOnce(BodyEnd) + Unpin> HttpBody for GuardedBody<G, F> {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = &mut *self;
        let frame = Pin::new(&mut this.body).poll_frame(cx);
        if let Poll::Ready(ref frame) = frame {
            let (stream_end, overflowed) =
                advance_stream(&mut this.remaining, &this.body, frame.as_ref());
            if stream_end.is_some() {
                this.guard = None;
            }
            if this.finish.is_some()
                && let Some(end) = observed_end(stream_end, overflowed, this.remaining)
                && let Some(finish) = this.finish.take()
            {
                finish(end);
            }
        }
        frame
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

fn advance_stream(
    remaining: &mut Option<u64>,
    body: &Body,
    frame: Option<&Result<Frame<Bytes>, axum::Error>>,
) -> (Option<BodyEnd>, bool) {
    match frame {
        None => (Some(BodyEnd::Complete), false),
        Some(Err(_)) => (Some(BodyEnd::Error), false),
        Some(Ok(frame)) => {
            let mut overflowed = false;
            if let (Some(remaining), Some(data)) = (remaining.as_mut(), frame.data_ref()) {
                match remaining.checked_sub(data.len() as u64) {
                    Some(next) => *remaining = next,
                    None => overflowed = true,
                }
            }
            let stream_end =
                (frame.is_trailers() || body.is_end_stream()).then_some(BodyEnd::Complete);
            (stream_end, overflowed)
        }
    }
}

fn observed_end(
    stream_end: Option<BodyEnd>,
    overflowed: bool,
    remaining: Option<u64>,
) -> Option<BodyEnd> {
    if overflowed {
        return Some(BodyEnd::Error);
    }
    match stream_end {
        Some(BodyEnd::Error) => Some(BodyEnd::Error),
        Some(BodyEnd::Complete) => body_end(true, remaining),
        None => body_end(false, remaining),
    }
}

#[cfg(test)]
mod tests {
    use std::mem::DropGuard;
    use std::sync::{Arc, Mutex};

    use axum::http::HeaderMap;
    use http_body_util::BodyExt as _;

    use super::*;

    struct ScriptedBody {
        frames: std::collections::VecDeque<Result<Frame<Bytes>, std::io::Error>>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl HttpBody for ScriptedBody {
        type Data = Bytes;
        type Error = std::io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            Poll::Ready(self.frames.pop_front())
        }
    }

    impl Drop for ScriptedBody {
        fn drop(&mut self) {
            self.events.lock().unwrap().push("body_dropped");
        }
    }

    fn tracked(
        inner: Body,
        remaining: Option<u64>,
        events: &Arc<Mutex<Vec<&'static str>>>,
    ) -> Body {
        let guard = DropGuard::new(Arc::clone(events), |events| {
            events.lock().unwrap().push("guard_dropped");
        });
        let observer = DropGuard::new(Arc::clone(events), |events| {
            events.lock().unwrap().push("observer_abandoned");
        });
        on_body_end(inner, Some(guard), remaining, move |end| {
            let events = DropGuard::dismiss(observer);
            events.lock().unwrap().push(match end {
                BodyEnd::Complete => "observer_complete",
                BodyEnd::Error => "observer_error",
            });
        })
    }

    fn scripted(
        frames: impl IntoIterator<Item = Result<Frame<Bytes>, std::io::Error>>,
        events: &Arc<Mutex<Vec<&'static str>>>,
    ) -> Body {
        Body::new(ScriptedBody {
            frames: frames.into_iter().collect(),
            events: Arc::clone(events),
        })
    }

    #[tokio::test]
    async fn declared_length_finishes_observation_before_eof_and_keeps_drop_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut body = tracked(
            scripted([Ok(Frame::data(Bytes::from_static(b"ok")))], &events),
            Some(2),
            &events,
        );

        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), "ok");
        assert_eq!(*events.lock().unwrap(), ["observer_complete"]);

        drop(body);
        assert_eq!(
            *events.lock().unwrap(),
            ["observer_complete", "body_dropped", "guard_dropped"]
        );
    }

    #[tokio::test]
    async fn early_drop_runs_body_then_guard_then_observer() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let body = tracked(
            scripted([Ok(Frame::data(Bytes::from_static(b"ok")))], &events),
            Some(2),
            &events,
        );

        drop(body);
        assert_eq!(
            *events.lock().unwrap(),
            ["body_dropped", "guard_dropped", "observer_abandoned"]
        );
    }

    #[tokio::test]
    async fn trailers_and_errors_release_guard_before_observation_and_preserve_error_source() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut body = tracked(
            scripted([Ok(Frame::trailers(HeaderMap::new()))], &events),
            None,
            &events,
        );
        assert!(body.frame().await.unwrap().unwrap().is_trailers());
        assert_eq!(
            *events.lock().unwrap(),
            ["guard_dropped", "observer_complete"]
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let mut body = tracked(
            scripted([Err(std::io::Error::other("late boom"))], &events),
            Some(4),
            &events,
        );
        let err = body.frame().await.unwrap().unwrap_err();
        assert!(
            shared_rules::error_chain::contains::<std::io::Error>(&err),
            "late error source chain lost: {err:?}"
        );
        assert_eq!(*events.lock().unwrap(), ["guard_dropped", "observer_error"]);
    }

    #[tokio::test]
    async fn declared_length_mismatch_reports_error_while_preserving_guard_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut body = tracked(
            scripted([Ok(Frame::data(Bytes::from_static(b"extra")))], &events),
            Some(2),
            &events,
        );
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "extra"
        );
        assert_eq!(*events.lock().unwrap(), ["observer_error"]);
        drop(body);
        assert_eq!(
            *events.lock().unwrap(),
            ["observer_error", "body_dropped", "guard_dropped"]
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let mut body = tracked(Body::from("extra"), Some(2), &events);
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "extra"
        );
        assert_eq!(*events.lock().unwrap(), ["guard_dropped", "observer_error"]);
    }
}
