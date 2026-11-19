use std::any::Any;
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http_body::{Body, Frame, SizeHint};
use parking_lot::Mutex;
use tokio_util::task::AbortOnDropHandle;

pub type FramePoll<E> = Poll<Option<Result<Frame<Bytes>, E>>>;
pub type WatchedPoll<E> = Result<FramePoll<E>, Box<dyn Any + Send>>;

pub fn register_waker(slot: &mut Option<Waker>, waker: &Waker) {
    match slot {
        Some(existing) => existing.clone_from(waker),
        None => *slot = Some(waker.clone()),
    }
}

pub trait TerminableBody: Unpin + Send + 'static {
    type Error;
    type Stop: Send + 'static;
    type Finalizer: Send;

    fn poll_watched(
        &mut self,
        cx: &mut Context<'_>,
        watched: bool,
    ) -> (WatchedPoll<Self::Error>, Option<Self::Finalizer>);

    fn terminate(&mut self, stop: Self::Stop) -> Option<(Option<Waker>, Self::Finalizer)>;

    #[must_use]
    fn take_for_drop(&mut self) -> Self;

    fn is_end_stream(&self) -> bool;

    fn size_hint(&self) -> SizeHint;

    fn watcher_lost() -> Self::Stop;

    fn finish(finalizer: Self::Finalizer);
}

pub struct Watched<S: TerminableBody> {
    state: Arc<Mutex<S>>,
    watcher: Option<AbortOnDropHandle<()>>,
}

impl<S: TerminableBody> Watched<S> {
    #[must_use]
    pub fn new(state: S, stopped: impl Future<Output = S::Stop> + Send + 'static) -> Self {
        let state = Arc::new(Mutex::new(state));
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
                terminate_state(&state, S::watcher_lost());
            }
        });
        Self {
            state,
            watcher: Some(AbortOnDropHandle::new(watcher)),
        }
    }

    #[must_use]
    pub const fn is_watching(&self) -> bool {
        self.watcher.is_some()
    }

    fn reap_watcher(&mut self, cx: &mut Context<'_>) {
        if let Some(watcher) = &mut self.watcher
            && let Poll::Ready(joined) = Future::poll(Pin::new(watcher), cx)
        {
            self.watcher = None;
            if joined.is_err() {
                terminate_state(&self.state, S::watcher_lost());
            }
        }
    }
}

fn terminate_state<S: TerminableBody>(state: &Mutex<S>, stop: S::Stop) {
    let transition = state.lock().terminate(stop);
    if let Some((waker, finalizer)) = transition {
        if let Some(waker) = waker {
            waker.wake();
        }
        S::finish(finalizer);
    }
}

impl<S: TerminableBody> Body for Watched<S> {
    type Data = Bytes;
    type Error = S::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> FramePoll<Self::Error> {
        let this = self.get_mut();
        this.reap_watcher(cx);
        let (polled, finalizer) = this.state.lock().poll_watched(cx, true);
        if let Some(finalizer) = finalizer {
            this.watcher = None;
            S::finish(finalizer);
        }
        match polled {
            Ok(frame) => frame,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.state.lock().is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.state.lock().size_hint()
    }
}

impl<S: TerminableBody> Drop for Watched<S> {
    fn drop(&mut self) {
        let stage = { self.state.lock().take_for_drop() };
        drop(self.watcher.take());
        drop(stage);
    }
}

impl<S: TerminableBody> fmt::Debug for Watched<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Watched").finish_non_exhaustive()
    }
}

pub enum MaybeWatched<S: TerminableBody> {
    Local(S),
    Watched(Watched<S>),
}

impl<S: TerminableBody> MaybeWatched<S> {
    #[must_use]
    pub const fn local(state: S) -> Self {
        Self::Local(state)
    }

    #[must_use]
    pub fn watched(state: S, stopped: impl Future<Output = S::Stop> + Send + 'static) -> Self {
        Self::Watched(Watched::new(state, stopped))
    }

    #[must_use]
    pub const fn is_watching(&self) -> bool {
        match self {
            Self::Local(_) => false,
            Self::Watched(watched) => watched.is_watching(),
        }
    }
}

impl<S: TerminableBody> Body for MaybeWatched<S> {
    type Data = Bytes;
    type Error = S::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> FramePoll<Self::Error> {
        match self.get_mut() {
            Self::Local(state) => {
                let (polled, finalizer) = state.poll_watched(cx, false);
                if let Some(finalizer) = finalizer {
                    S::finish(finalizer);
                }
                match polled {
                    Ok(frame) => frame,
                    Err(panic) => std::panic::resume_unwind(panic),
                }
            }
            Self::Watched(watched) => Pin::new(watched).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Local(state) => state.is_end_stream(),
            Self::Watched(watched) => watched.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Local(state) => state.size_hint(),
            Self::Watched(watched) => watched.size_hint(),
        }
    }
}

impl<S: TerminableBody> fmt::Debug for MaybeWatched<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MaybeWatched")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use http_body_util::BodyExt as _;
    use tokio::sync::oneshot;

    use super::*;

    const BUDGET: Duration = Duration::from_secs(1);

    struct TestState {
        active: Option<TestActive>,
        pending: Option<Bytes>,
    }

    struct TestActive {
        dropped: Arc<AtomicBool>,
        waker: Option<Waker>,
        finish_on_poll: Option<Bytes>,
    }

    impl Drop for TestActive {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    struct TestFinalizer {
        _active: TestActive,
        recorded: Arc<Mutex<Vec<&'static str>>>,
        label: &'static str,
    }

    impl TestState {
        fn new(dropped: Arc<AtomicBool>, finish_on_poll: Option<Bytes>) -> Self {
            Self {
                active: Some(TestActive {
                    dropped,
                    waker: None,
                    finish_on_poll,
                }),
                pending: None,
            }
        }
    }

    struct StateWithLog {
        state: TestState,
        recorded: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TerminableBody for StateWithLog {
        type Error = Infallible;
        type Stop = &'static str;
        type Finalizer = TestFinalizer;

        fn poll_watched(
            &mut self,
            cx: &mut Context<'_>,
            watched: bool,
        ) -> (WatchedPoll<Self::Error>, Option<Self::Finalizer>) {
            if let Some(bytes) = self.state.pending.take() {
                return (Ok(Poll::Ready(Some(Ok(Frame::data(bytes))))), None);
            }
            let Some(active) = &mut self.state.active else {
                return (Ok(Poll::Ready(None)), None);
            };
            if let Some(bytes) = active.finish_on_poll.take() {
                let active = self.state.active.take().unwrap();
                let finalizer = TestFinalizer {
                    _active: active,
                    recorded: Arc::clone(&self.recorded),
                    label: "polled",
                };
                return (
                    Ok(Poll::Ready(Some(Ok(Frame::data(bytes))))),
                    Some(finalizer),
                );
            }
            if watched {
                register_waker(&mut active.waker, cx.waker());
            }
            (Ok(Poll::Pending), None)
        }

        fn terminate(&mut self, stop: Self::Stop) -> Option<(Option<Waker>, Self::Finalizer)> {
            let mut active = self.state.active.take()?;
            self.state.pending = Some(Bytes::from_static(stop.as_bytes()));
            let waker = active.waker.take();
            Some((
                waker,
                TestFinalizer {
                    _active: active,
                    recorded: Arc::clone(&self.recorded),
                    label: stop,
                },
            ))
        }

        fn take_for_drop(&mut self) -> Self {
            Self {
                state: TestState {
                    active: self.state.active.take(),
                    pending: self.state.pending.take(),
                },
                recorded: Arc::clone(&self.recorded),
            }
        }

        fn is_end_stream(&self) -> bool {
            self.state.active.is_none() && self.state.pending.is_none()
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }

        fn watcher_lost() -> Self::Stop {
            "lost"
        }

        fn finish(finalizer: Self::Finalizer) {
            finalizer.recorded.lock().push(finalizer.label);
        }
    }

    #[tokio::test]
    async fn terminal_poll_and_drop_abort_the_watcher_task() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let mut body = MaybeWatched::watched(
            StateWithLog {
                state: TestState::new(Arc::clone(&dropped), Some(Bytes::from_static(b"ok"))),
                recorded: Arc::clone(&recorded),
            },
            async move {
                let _guard = done_tx;
                std::future::pending::<&'static str>().await
            },
        );
        assert!(body.is_watching());
        let data = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(data, Bytes::from_static(b"ok"));
        assert!(!body.is_watching());
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(*recorded.lock(), ["polled"]);
        assert!(
            tokio::time::timeout(BUDGET, done_rx)
                .await
                .unwrap()
                .is_err()
        );

        let dropped = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = oneshot::channel::<()>();
        let body = MaybeWatched::watched(
            StateWithLog {
                state: TestState::new(Arc::clone(&dropped), None),
                recorded: Arc::clone(&recorded),
            },
            async move {
                let _guard = done_tx;
                std::future::pending::<&'static str>().await
            },
        );
        drop(body);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(
            tokio::time::timeout(BUDGET, done_rx)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn lost_watcher_runtime_invokes_watcher_lost_stop() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut body = {
            let _entered = runtime.enter();
            MaybeWatched::watched(
                StateWithLog {
                    state: TestState::new(Arc::clone(&dropped), None),
                    recorded: Arc::clone(&recorded),
                },
                std::future::pending::<&'static str>(),
            )
        };
        drop(runtime);
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut cx) else {
            unreachable!("lost watcher produces stop frame");
        };
        assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"lost"));
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(*recorded.lock(), ["lost"]);
    }
}
