use std::any::Any;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use parking_lot::Mutex;
use tokio_util::task::AbortOnDropHandle;

pub type FramePoll<E> = Poll<Option<Result<Frame<Bytes>, E>>>;
type WatchedPoll<E> = Result<FramePoll<E>, Box<dyn Any + Send>>;

pub fn register_waker(slot: &mut Option<Waker>, waker: &Waker) {
    match slot {
        Some(existing) => existing.clone_from(waker),
        None => *slot = Some(waker.clone()),
    }
}

pub enum PanicOutcome<Stop, Outcome> {
    Stop(Stop),
    Unwind(Outcome),
}

impl<Stop, Outcome> fmt::Debug for PanicOutcome<Stop, Outcome> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stop(_) => formatter.write_str("PanicOutcome::Stop(..)"),
            Self::Unwind(_) => formatter.write_str("PanicOutcome::Unwind(..)"),
        }
    }
}

pub trait StreamHooks<E>: Unpin + Send + 'static {
    type Stop: Send + 'static;
    type Outcome: Send;
    type Guards: Send;

    fn stopped(&self) -> Option<Self::Stop>;

    fn in_scope<R>(&self, poll: impl FnOnce() -> R) -> R;

    fn on_panic(&self) -> PanicOutcome<Self::Stop, Self::Outcome>;

    fn write_stop(&mut self, stop: Self::Stop, trailers: &mut HeaderMap) -> Self::Outcome;

    fn terminal_outcome(&self, polled: &FramePoll<E>) -> Option<Self::Outcome>;

    fn empty_outcome(&self) -> Self::Outcome;

    fn watcher_lost() -> Self::Stop;

    fn emit(self, outcome: Self::Outcome) -> Self::Guards;
}

struct Active<B, H> {
    body: B,
    hooks: H,
    waker: Option<Waker>,
}

enum Envelope<B, H> {
    Active(Active<B, H>),
    PendingTrailers(HeaderMap),
    Closed,
}

struct Finalizer<B, H, O> {
    body: B,
    hooks: H,
    outcome: O,
}

type StreamFinalizer<B, H> = Finalizer<B, H, <H as StreamHooks<<B as Body>::Error>>::Outcome>;

impl<B, H> Finalizer<B, H, H::Outcome>
where
    B: Body<Data = Bytes>,
    H: StreamHooks<B::Error>,
{
    fn run(self) {
        let Self {
            body,
            hooks,
            outcome,
        } = self;
        let guards = hooks.emit(outcome);
        drop(body);
        drop(guards);
    }
}

impl<B, H> Envelope<B, H> {
    fn is_end_stream(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

impl<B, H> Envelope<B, H>
where
    B: Body<Data = Bytes>,
{
    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Active(active) => active.body.size_hint(),
            Self::PendingTrailers(_) | Self::Closed => SizeHint::with_exact(0),
        }
    }

    fn pop_terminal(&mut self) -> FramePoll<B::Error> {
        match std::mem::replace(self, Self::Closed) {
            Self::PendingTrailers(trailers) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            other => {
                *self = other;
                Poll::Ready(None)
            }
        }
    }
}

impl<B, H> Envelope<B, H>
where
    B: Body<Data = Bytes>,
    H: StreamHooks<B::Error>,
{
    fn stopped(&self) -> Option<H::Stop> {
        let Self::Active(active) = self else {
            return None;
        };
        active.hooks.stopped()
    }

    fn transition(&mut self, next: Self, outcome: H::Outcome) -> Option<StreamFinalizer<B, H>> {
        match std::mem::replace(self, next) {
            Self::Active(Active { body, hooks, .. }) => Some(Finalizer {
                body,
                hooks,
                outcome,
            }),
            other => {
                *self = other;
                None
            }
        }
    }

    fn stage_stop(&mut self, stop: H::Stop) -> Option<StreamFinalizer<B, H>> {
        let Self::Active(active) = self else {
            return None;
        };
        let mut trailers = HeaderMap::new();
        let outcome = active.hooks.write_stop(stop, &mut trailers);
        self.transition(Self::PendingTrailers(trailers), outcome)
    }

    fn close_empty(&mut self) -> Option<StreamFinalizer<B, H>> {
        let Self::Active(active) = self else {
            return None;
        };
        let outcome = active.hooks.empty_outcome();
        self.transition(Self::Closed, outcome)
    }

    fn cut_off(&mut self, stop: H::Stop) -> (FramePoll<B::Error>, Option<StreamFinalizer<B, H>>) {
        let finalizer = self.stage_stop(stop);
        (self.pop_terminal(), finalizer)
    }

    fn terminate(&mut self, stop: H::Stop) -> Option<(Option<Waker>, StreamFinalizer<B, H>)> {
        let Self::Active(active) = self else {
            return None;
        };
        let waker = active.waker.take();
        let finalizer = self.stage_stop(stop)?;
        Some((waker, finalizer))
    }
}

impl<B, H> Envelope<B, H>
where
    B: Body<Data = Bytes> + Unpin,
    H: StreamHooks<B::Error>,
{
    fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
        watched: bool,
    ) -> (WatchedPoll<B::Error>, Option<StreamFinalizer<B, H>>) {
        let Self::Active(active) = self else {
            return (Ok(self.pop_terminal()), None);
        };
        if let Some(stop) = active.hooks.stopped() {
            let (frame, finalizer) = self.cut_off(stop);
            return (Ok(frame), finalizer);
        }
        if watched {
            register_waker(&mut active.waker, cx.waker());
        }
        let polled = active.hooks.in_scope(|| {
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                Pin::new(&mut active.body).poll_frame(cx)
            }))
        });
        match polled {
            Err(panic) => match active.hooks.on_panic() {
                PanicOutcome::Stop(stop) => {
                    let (frame, finalizer) = self.cut_off(stop);
                    (Ok(frame), finalizer)
                }
                PanicOutcome::Unwind(outcome) => {
                    let finalizer = self.transition(Self::Closed, outcome);
                    (Err(panic), finalizer)
                }
            },
            Ok(polled) => {
                if let Some(stop) = active.hooks.stopped() {
                    let (frame, finalizer) = self.cut_off(stop);
                    (Ok(frame), finalizer)
                } else {
                    let outcome = active.hooks.terminal_outcome(&polled);
                    let finalizer =
                        outcome.and_then(|outcome| self.transition(Self::Closed, outcome));
                    (Ok(polled), finalizer)
                }
            }
        }
    }
}

struct Watched<B, H> {
    state: Arc<Mutex<Envelope<B, H>>>,
    watcher: Option<AbortOnDropHandle<()>>,
}

impl<B, H> Watched<B, H>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
    H: StreamHooks<B::Error>,
{
    fn new(
        envelope: Envelope<B, H>,
        stopped: impl Future<Output = H::Stop> + Send + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(envelope));
        let weak = Arc::downgrade(&state);
        let watcher = tokio::spawn(async move {
            let result = AssertUnwindSafe(async {
                let stop = stopped.await;
                if let Some(state) = weak.upgrade() {
                    terminate_state(&state, stop);
                }
            })
            .catch_unwind()
            .await;
            if result.is_err()
                && let Some(state) = weak.upgrade()
            {
                terminate_state(&state, H::watcher_lost());
            }
        });
        Self {
            state,
            watcher: Some(AbortOnDropHandle::new(watcher)),
        }
    }

    fn reap_watcher(&mut self, cx: &mut Context<'_>) {
        if let Some(watcher) = &mut self.watcher
            && let Poll::Ready(joined) = Future::poll(Pin::new(watcher), cx)
        {
            self.watcher = None;
            if joined.is_err() {
                terminate_state(&self.state, H::watcher_lost());
            }
        }
    }
}

impl<B, H> Drop for Watched<B, H> {
    fn drop(&mut self) {
        let stage = {
            let mut state = self.state.lock();
            std::mem::replace(&mut *state, Envelope::Closed)
        };
        drop(self.watcher.take());
        drop(stage);
    }
}

fn terminate_state<B, H>(state: &Mutex<Envelope<B, H>>, stop: H::Stop)
where
    B: Body<Data = Bytes>,
    H: StreamHooks<B::Error>,
{
    let transition = state.lock().terminate(stop);
    if let Some((waker, finalizer)) = transition {
        if let Some(waker) = waker {
            waker.wake();
        }
        finalizer.run();
    }
}

enum State<B, H> {
    Local(Envelope<B, H>),
    Watched(Watched<B, H>),
}

pub struct WatchedBody<B, H>(State<B, H>);

impl<B, H> WatchedBody<B, H> {
    #[must_use]
    pub const fn is_watching(&self) -> bool {
        match &self.0 {
            State::Local(_) => false,
            State::Watched(watched) => watched.watcher.is_some(),
        }
    }
}

impl<B, H> WatchedBody<B, H>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
    H: StreamHooks<B::Error>,
{
    #[must_use]
    pub fn start<F>(
        body: B,
        hooks: H,
        initial: Option<H::Outcome>,
        watch: impl FnOnce(&H) -> Option<F>,
    ) -> Option<Self>
    where
        F: Future<Output = H::Stop> + Send + 'static,
    {
        let ended = body.is_end_stream();
        let mut envelope = Envelope::Active(Active {
            body,
            hooks,
            waker: None,
        });
        let finalizer = if let Some(outcome) = initial {
            envelope.transition(Envelope::Closed, outcome)
        } else if let Some(stop) = envelope.stopped() {
            envelope.stage_stop(stop)
        } else if ended {
            envelope.close_empty()
        } else {
            None
        };
        if let Some(finalizer) = finalizer {
            finalizer.run();
        }
        let stopped = match &envelope {
            Envelope::Active(active) => watch(&active.hooks),
            Envelope::PendingTrailers(_) => None,
            Envelope::Closed => return None,
        };
        Some(Self(match stopped {
            Some(stopped) => State::Watched(Watched::new(envelope, stopped)),
            None => State::Local(envelope),
        }))
    }
}

impl<B, H> Body for WatchedBody<B, H>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
    H: StreamHooks<B::Error>,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> FramePoll<Self::Error> {
        let (polled, finalizer) = match &mut self.get_mut().0 {
            State::Local(envelope) => envelope.poll_next(cx, false),
            State::Watched(watched) => {
                watched.reap_watcher(cx);
                let (polled, finalizer) = watched.state.lock().poll_next(cx, true);
                if finalizer.is_some() {
                    watched.watcher = None;
                }
                (polled, finalizer)
            }
        };
        if let Some(finalizer) = finalizer {
            finalizer.run();
        }
        match polled {
            Ok(frame) => frame,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.0 {
            State::Local(envelope) => envelope.is_end_stream(),
            State::Watched(watched) => watched.state.lock().is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            State::Local(envelope) => envelope.size_hint(),
            State::Watched(watched) => watched.state.lock().size_hint(),
        }
    }
}

impl<B, H> fmt::Debug for WatchedBody<B, H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WatchedBody")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use http::HeaderValue;
    use http_body_util::BodyExt as _;
    use tokio::sync::oneshot;

    use super::*;

    const BUDGET: Duration = Duration::from_secs(1);

    struct ProbeBody {
        frame: Option<Frame<Bytes>>,
        dropped: Arc<AtomicBool>,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ProbeBody {
        fn new(
            frame: Option<Frame<Bytes>>,
            dropped: Arc<AtomicBool>,
            events: Arc<Mutex<Vec<&'static str>>>,
        ) -> Self {
            Self {
                frame,
                dropped,
                events,
            }
        }
    }

    impl Drop for ProbeBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
            self.events.lock().push("body_dropped");
        }
    }

    impl Body for ProbeBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> FramePoll<Self::Error> {
            self.frame
                .take()
                .map_or(Poll::Pending, |frame| Poll::Ready(Some(Ok(frame))))
        }
    }

    struct GuardLog {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Drop for GuardLog {
        fn drop(&mut self) {
            self.events.lock().push("guards_dropped");
        }
    }

    struct TestHooks {
        events: Arc<Mutex<Vec<&'static str>>>,
        guards: GuardLog,
        stopped: Option<&'static str>,
    }

    impl TestHooks {
        fn new(events: Arc<Mutex<Vec<&'static str>>>, stopped: Option<&'static str>) -> Self {
            Self {
                guards: GuardLog {
                    events: Arc::clone(&events),
                },
                events,
                stopped,
            }
        }
    }

    impl StreamHooks<Infallible> for TestHooks {
        type Stop = &'static str;
        type Outcome = &'static str;
        type Guards = GuardLog;

        fn stopped(&self) -> Option<Self::Stop> {
            self.stopped
        }

        fn in_scope<R>(&self, poll: impl FnOnce() -> R) -> R {
            poll()
        }

        fn on_panic(&self) -> PanicOutcome<Self::Stop, Self::Outcome> {
            PanicOutcome::Stop("panic")
        }

        fn write_stop(&mut self, stop: Self::Stop, trailers: &mut HeaderMap) -> Self::Outcome {
            trailers.insert("x-stop", HeaderValue::from_static(stop));
            stop
        }

        fn terminal_outcome(&self, polled: &FramePoll<Infallible>) -> Option<Self::Outcome> {
            match polled {
                Poll::Ready(Some(Ok(frame))) if frame.is_trailers() => Some("polled"),
                Poll::Ready(None) => Some("empty"),
                _ => None,
            }
        }

        fn empty_outcome(&self) -> Self::Outcome {
            "empty"
        }

        fn watcher_lost() -> Self::Stop {
            "lost"
        }

        fn emit(self, outcome: Self::Outcome) -> Self::Guards {
            self.events.lock().push(outcome);
            self.guards
        }
    }

    #[tokio::test]
    async fn terminal_poll_and_drop_abort_the_watcher_task() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let mut trailers = HeaderMap::new();
        trailers.insert("x-ok", HeaderValue::from_static("1"));
        let mut body = WatchedBody::start(
            ProbeBody::new(
                Some(Frame::trailers(trailers)),
                Arc::clone(&dropped),
                Arc::clone(&events),
            ),
            TestHooks::new(Arc::clone(&events), None),
            None,
            |_| {
                Some(async move {
                    let _guard = done_tx;
                    std::future::pending::<&'static str>().await
                })
            },
        )
        .unwrap();
        assert!(body.is_watching());
        let frame = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        assert_eq!(frame["x-ok"], "1");
        assert!(!body.is_watching());
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(*events.lock(), ["polled", "body_dropped", "guards_dropped"]);
        assert!(
            tokio::time::timeout(BUDGET, done_rx)
                .await
                .unwrap()
                .is_err()
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let body = WatchedBody::start(
            ProbeBody::new(None, Arc::clone(&dropped), Arc::clone(&events)),
            TestHooks::new(Arc::clone(&events), None),
            None,
            |_| {
                Some(async move {
                    let _guard = done_tx;
                    std::future::pending::<&'static str>().await
                })
            },
        )
        .unwrap();
        drop(body);
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(*events.lock(), ["body_dropped", "guards_dropped"]);
        assert!(
            tokio::time::timeout(BUDGET, done_rx)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn lost_watcher_runtime_invokes_watcher_lost_stop() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut body = {
            let _entered = runtime.enter();
            WatchedBody::start(
                ProbeBody::new(None, Arc::clone(&dropped), Arc::clone(&events)),
                TestHooks::new(Arc::clone(&events), None),
                None,
                |_| Some(std::future::pending::<&'static str>()),
            )
            .unwrap()
        };
        drop(runtime);
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut cx) else {
            unreachable!("lost watcher produces stop frame");
        };
        assert_eq!(frame.into_trailers().unwrap()["x-stop"], "lost");
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(*events.lock(), ["lost", "body_dropped", "guards_dropped"]);
    }

    #[tokio::test]
    async fn start_handles_initial_and_stopped_outcomes_without_a_watcher() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let watch_called = Arc::new(AtomicBool::new(false));
        let body = WatchedBody::start(
            ProbeBody::new(None, Arc::clone(&dropped), Arc::clone(&events)),
            TestHooks::new(Arc::clone(&events), None),
            Some("initial"),
            |_| {
                watch_called.store(true, Ordering::SeqCst);
                Some(std::future::pending::<&'static str>())
            },
        );
        assert!(body.is_none());
        assert!(!watch_called.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            *events.lock(),
            ["initial", "body_dropped", "guards_dropped"]
        );

        let events = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut body = WatchedBody::start(
            ProbeBody::new(None, Arc::clone(&dropped), Arc::clone(&events)),
            TestHooks::new(Arc::clone(&events), Some("expired")),
            None,
            |_| {
                watch_called.store(true, Ordering::SeqCst);
                Some(std::future::pending::<&'static str>())
            },
        )
        .unwrap();
        assert!(!body.is_watching());
        assert!(!watch_called.load(Ordering::SeqCst));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(
            *events.lock(),
            ["expired", "body_dropped", "guards_dropped"]
        );
        let trailers = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        assert_eq!(trailers["x-stop"], "expired");
        assert!(body.frame().await.is_none());
    }
}
