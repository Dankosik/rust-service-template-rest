use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use operation_context::{OperationContext, Stopped};
use parking_lot::Mutex;
use tokio_util::sync::DropGuard;
use tokio_util::task::AbortOnDropHandle;

use crate::observe::Call;
use crate::protocol::{header_status_code, http_status_code};

pub(crate) type StatusHook = Box<dyn FnOnce(tonic::Code) + Send>;
type FramePoll = Poll<Option<Result<Frame<Bytes>, tonic::Status>>>;

#[derive(Default)]
#[expect(dead_code, reason = "guards release resources on drop")]
pub(crate) struct ClientGuards {
    pub(crate) upload: Option<UploadGuard>,
    pub(crate) cancel_on_drop: Option<DropGuard>,
}

pub(crate) struct ClientCustody {
    pub(crate) lifetime: OperationContext,
    pub(crate) watch_stop: bool,
    pub(crate) guards: ClientGuards,
    pub(crate) on_status: Option<StatusHook>,
}

pub(crate) enum ClientObservation {
    Transport(Call),
    Decoded {
        span: tracing::Span,
        observer: tonic::client::ResponseStatusObserver,
    },
}

impl ClientObservation {
    fn span(&self) -> &tracing::Span {
        match self {
            Self::Transport(call) => call.span(),
            Self::Decoded { span, .. } => span,
        }
    }

    fn record_local_stop(&self, status: &tonic::Status) {
        if let Self::Decoded { observer, .. } = self {
            observer.stop(status);
        }
    }
}

pub(crate) fn client_deadline() -> tonic::Status {
    tonic::Status::deadline_exceeded("request deadline exceeded")
}

pub(crate) fn client_stopped(reason: Stopped) -> tonic::Status {
    match reason {
        Stopped::Deadline => client_deadline(),
        Stopped::Cancelled => tonic::Status::cancelled("request cancelled"),
    }
}

fn watcher_lost_status() -> tonic::Status {
    tonic::Status::internal("internal error")
}

#[derive(Clone, Copy)]
struct ClientOutcome {
    metric_code: tonic::Code,
    wire_code: Option<tonic::Code>,
}

impl ClientOutcome {
    const fn local(code: tonic::Code) -> Self {
        Self {
            metric_code: code,
            wire_code: None,
        }
    }

    fn from_headers(observation: &ClientObservation, headers: &HeaderMap) -> Option<Self> {
        let wire_code = header_status_code(headers)?;
        let metric_code = if matches!(observation, ClientObservation::Transport(_))
            && (headers.contains_key(tonic::Status::GRPC_MESSAGE)
                || headers.contains_key(tonic::Status::GRPC_STATUS_DETAILS))
        {
            tonic::Status::from_header_map(headers).map_or(wire_code, |status| status.code())
        } else {
            wire_code
        };
        Some(Self {
            metric_code,
            wire_code: Some(wire_code),
        })
    }
}

fn store_waker(slot: &mut Option<Waker>, waker: &Waker) {
    match slot {
        Some(stored) => stored.clone_from(waker),
        None => *slot = Some(waker.clone()),
    }
}

struct ActiveStream<B> {
    body: B,
    observation: ClientObservation,
    _guards: ClientGuards,
    lifetime: OperationContext,
    waker: Option<Waker>,
    on_status: Option<StatusHook>,
}

enum StreamPhase<B> {
    Open(ActiveStream<B>),
    Trailers(HeaderMap),
    Done,
}

struct StreamCompletion<B> {
    active: ActiveStream<B>,
    outcome: ClientOutcome,
}

impl<B> StreamCompletion<B> {
    fn finish(self) {
        match self.active.observation {
            ClientObservation::Transport(call) => call.finish(self.outcome.metric_code),
            ClientObservation::Decoded { observer, .. } => observer.finish_stop(),
        }
        if let (Some(code), Some(on_status)) = (self.outcome.wire_code, self.active.on_status) {
            on_status(code);
        }
    }
}

struct StreamState<B> {
    phase: StreamPhase<B>,
    missing_status: ClientOutcome,
}

impl<B> StreamState<B> {
    fn stopped_status(&self) -> Option<tonic::Status> {
        let StreamPhase::Open(active) = &self.phase else {
            return None;
        };
        active.lifetime.stopped().map(client_stopped)
    }

    fn transition(
        &mut self,
        next: StreamPhase<B>,
        outcome: ClientOutcome,
    ) -> Option<StreamCompletion<B>> {
        match std::mem::replace(&mut self.phase, next) {
            StreamPhase::Open(active) => Some(StreamCompletion { active, outcome }),
            terminal => {
                self.phase = terminal;
                None
            }
        }
    }

    fn complete_stopped(&mut self, status: &tonic::Status) -> Option<StreamCompletion<B>> {
        if let StreamPhase::Open(active) = &self.phase {
            active.observation.record_local_stop(status);
        }
        let mut trailers = HeaderMap::new();
        let _ = status.add_header(&mut trailers);
        self.transition(
            StreamPhase::Trailers(trailers),
            ClientOutcome::local(status.code()),
        )
    }

    fn end_stopped(&mut self, status: &tonic::Status) -> (FramePoll, Option<StreamCompletion<B>>) {
        let completion = self.complete_stopped(status);
        (self.take_terminal(), completion)
    }

    fn complete_polled(&mut self, polled: &FramePoll) -> Option<StreamCompletion<B>> {
        let StreamPhase::Open(active) = &self.phase else {
            return None;
        };
        let outcome = match polled {
            Poll::Ready(Some(Ok(frame))) => {
                let trailers = frame.trailers_ref()?;
                ClientOutcome::from_headers(&active.observation, trailers)
                    .unwrap_or(self.missing_status)
            }
            Poll::Ready(Some(Err(error))) => ClientOutcome::local(error.code()),
            Poll::Ready(None) => self.missing_status,
            Poll::Pending => return None,
        };
        self.transition(StreamPhase::Done, outcome)
    }

    fn take_terminal(&mut self) -> FramePoll {
        match std::mem::replace(&mut self.phase, StreamPhase::Done) {
            StreamPhase::Trailers(trailers) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            other => {
                self.phase = other;
                Poll::Ready(None)
            }
        }
    }
}

struct ClientResponseBody<B> {
    state: Arc<Mutex<StreamState<B>>>,
    watcher: Option<AbortOnDropHandle<()>>,
    #[cfg(test)]
    watcher_done: Option<tokio::sync::oneshot::Receiver<()>>,
}

pub(crate) fn attach_client_response<B>(
    mut response: http::Response<B>,
    observation: ClientObservation,
    custody: ClientCustody,
) -> http::Response<impl Body<Data = Bytes, Error = tonic::Status> + Send + 'static>
where
    B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
{
    let initial = if let Some(reason) = custody.lifetime.stopped() {
        let status = client_stopped(reason);
        observation.record_local_stop(&status);
        for name in [
            tonic::Status::GRPC_STATUS,
            tonic::Status::GRPC_MESSAGE,
            tonic::Status::GRPC_STATUS_DETAILS,
        ] {
            response.headers_mut().remove(name);
        }
        let _ = status.add_header(response.headers_mut());
        Some(ClientOutcome::local(status.code()))
    } else {
        ClientOutcome::from_headers(&observation, response.headers())
    };
    let missing_status = http_status_code(response.status()).map_or(
        ClientOutcome::local(tonic::Code::Unknown),
        |code| ClientOutcome {
            metric_code: code,
            wire_code: Some(code),
        },
    );
    response
        .map(|body| ClientResponseBody::start(body, observation, custody, initial, missing_status))
}

impl<B> ClientResponseBody<B>
where
    B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
{
    fn start(
        body: B,
        observation: ClientObservation,
        custody: ClientCustody,
        initial: Option<ClientOutcome>,
        missing_status: ClientOutcome,
    ) -> Self {
        let ended = body.is_end_stream();
        let watch_stop = custody.watch_stop;
        let mut state = StreamState {
            phase: StreamPhase::Open(ActiveStream {
                body,
                observation,
                _guards: custody.guards,
                lifetime: custody.lifetime,
                waker: None,
                on_status: custody.on_status,
            }),
            missing_status,
        };
        let completion = if let Some(outcome) = initial {
            state.transition(StreamPhase::Done, outcome)
        } else if let Some(status) = state.stopped_status() {
            state.complete_stopped(&status)
        } else if ended {
            state.transition(StreamPhase::Done, missing_status)
        } else {
            None
        };
        if let Some(completion) = completion {
            completion.finish();
        }
        let watched = if watch_stop {
            match &state.phase {
                StreamPhase::Open(active) => Some(active.lifetime.clone()),
                StreamPhase::Trailers(_) | StreamPhase::Done => None,
            }
        } else {
            None
        };
        let mut response_body = Self {
            state: Arc::new(Mutex::new(state)),
            watcher: None,
            #[cfg(test)]
            watcher_done: None,
        };
        if let Some(lifetime) = watched {
            response_body.spawn_watcher(lifetime);
        }
        response_body
    }

    fn spawn_watcher(&mut self, lifetime: OperationContext) {
        let weak = Arc::downgrade(&self.state);
        #[cfg(test)]
        let (done, receiver) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        {
            self.watcher_done = Some(receiver);
        }
        self.watcher = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            #[cfg(test)]
            let _done = done;
            let result = AssertUnwindSafe(async {
                let reason = lifetime.wait_stopped().await;
                if let Some(state) = weak.upgrade() {
                    terminate_stream(&state, &client_stopped(reason));
                }
            })
            .catch_unwind()
            .await;
            if result.is_err()
                && let Some(state) = weak.upgrade()
            {
                terminate_stream(&state, &watcher_lost_status());
            }
        })));
    }

    fn reap_watcher(&mut self, context: &mut Context<'_>) {
        if let Some(watcher) = &mut self.watcher
            && let Poll::Ready(result) = Future::poll(Pin::new(watcher), context)
        {
            self.watcher = None;
            if result.is_err() {
                terminate_stream(&self.state, &watcher_lost_status());
            }
        }
    }
}

fn terminate_stream<B>(state: &Mutex<StreamState<B>>, status: &tonic::Status) {
    let completion = state.lock().complete_stopped(status);
    if let Some(mut completion) = completion {
        if let Some(waker) = completion.active.waker.take() {
            waker.wake();
        }
        completion.finish();
    }
}

impl<B> Body for ClientResponseBody<B>
where
    B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
{
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(self: Pin<&mut Self>, context: &mut Context<'_>) -> FramePoll {
        let this = self.get_mut();
        this.reap_watcher(context);
        let mut state = this.state.lock();
        let mut caught_panic = None;
        let (polled, completion) = if let Some(status) = state.stopped_status() {
            state.end_stopped(&status)
        } else {
            let StreamPhase::Open(active) = &mut state.phase else {
                return state.take_terminal();
            };
            store_waker(&mut active.waker, context.waker());
            let polled = {
                let _entered = active.observation.span().enter();
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    Pin::new(&mut active.body).poll_frame(context)
                }))
            };
            match polled {
                Ok(polled) => {
                    if let Some(status) = state.stopped_status() {
                        state.end_stopped(&status)
                    } else {
                        let completion = state.complete_polled(&polled);
                        (polled, completion)
                    }
                }
                Err(panic) => {
                    caught_panic = Some(panic);
                    let completion = state.transition(
                        StreamPhase::Done,
                        ClientOutcome::local(tonic::Code::Cancelled),
                    );
                    (Poll::Ready(None), completion)
                }
            }
        };
        drop(state);
        if let Some(completion) = completion {
            this.watcher = None;
            completion.finish();
        }
        if let Some(panic) = caught_panic {
            std::panic::resume_unwind(panic);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        let state = self.state.lock();
        matches!(state.phase, StreamPhase::Done)
    }

    fn size_hint(&self) -> SizeHint {
        let state = self.state.lock();
        match &state.phase {
            StreamPhase::Open(active) => active.body.size_hint(),
            StreamPhase::Trailers(_) | StreamPhase::Done => SizeHint::with_exact(0),
        }
    }
}

impl<B> Drop for ClientResponseBody<B> {
    fn drop(&mut self) {
        let phase = std::mem::replace(&mut self.state.lock().phase, StreamPhase::Done);
        drop(self.watcher.take());
        drop(phase);
    }
}

enum UploadPhase {
    Open(tonic::body::Body),
    Aborted,
    Done,
}

struct UploadState {
    phase: UploadPhase,
    waker: Option<Waker>,
}

pub(crate) struct UploadGuard(Arc<Mutex<UploadState>>);
struct Upload(Arc<Mutex<UploadState>>);

pub(crate) fn upload(body: tonic::body::Body) -> (tonic::body::Body, Option<UploadGuard>) {
    if body.is_end_stream() {
        return (body, None);
    }
    let state = Arc::new(Mutex::new(UploadState {
        phase: UploadPhase::Open(body),
        waker: None,
    }));
    (
        tonic::body::Body::new(Upload(Arc::clone(&state))),
        Some(UploadGuard(state)),
    )
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        let (released, waker) = {
            let mut state = self.0.lock();
            match &state.phase {
                UploadPhase::Open(body) => {
                    let next = if body.is_end_stream() {
                        UploadPhase::Done
                    } else {
                        UploadPhase::Aborted
                    };
                    let waker = if matches!(next, UploadPhase::Aborted) {
                        state.waker.take()
                    } else {
                        None
                    };
                    (Some(std::mem::replace(&mut state.phase, next)), waker)
                }
                UploadPhase::Aborted | UploadPhase::Done => (None, None),
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        drop(released);
    }
}

impl Body for Upload {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let mut guard = self.0.lock();
        let state = &mut *guard;
        let polled = match &mut state.phase {
            UploadPhase::Open(body) => {
                store_waker(&mut state.waker, context.waker());
                std::panic::catch_unwind(AssertUnwindSafe(|| Pin::new(body).poll_frame(context)))
            }
            UploadPhase::Aborted => {
                state.phase = UploadPhase::Done;
                state.waker = None;
                return Poll::Ready(Some(Err(tonic::Status::cancelled("call cancelled"))));
            }
            UploadPhase::Done => return Poll::Ready(None),
        };
        let finished = matches!(polled, Ok(Poll::Ready(None | Some(Err(_)))) | Err(_))
            || matches!(&state.phase, UploadPhase::Open(body) if body.is_end_stream());
        let released = if finished {
            state.waker = None;
            Some(std::mem::replace(&mut state.phase, UploadPhase::Done))
        } else {
            None
        };
        drop(guard);
        drop(released);
        match polled {
            Ok(polled) => polled,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn is_end_stream(&self) -> bool {
        let state = self.0.lock();
        match &state.phase {
            UploadPhase::Open(body) => body.is_end_stream(),
            UploadPhase::Aborted => false,
            UploadPhase::Done => true,
        }
    }

    fn size_hint(&self) -> SizeHint {
        let state = self.0.lock();
        match &state.phase {
            UploadPhase::Open(body) => body.size_hint(),
            UploadPhase::Aborted | UploadPhase::Done => SizeHint::with_exact(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use http_body_util::BodyExt as _;
    use operation_context::Deadline;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::*;

    const TEST_BUDGET: Duration = Duration::from_secs(1);
    const TEST_PANIC: &str = "stream source panic";

    struct PendingFrames {
        queued: VecDeque<Frame<Bytes>>,
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl PendingFrames {
        fn stalled() -> Self {
            Self {
                queued: VecDeque::new(),
                polls: Arc::default(),
                dropped: Arc::default(),
            }
        }
    }

    impl Drop for PendingFrames {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl Body for PendingFrames {
        type Data = Bytes;
        type Error = tonic::Status;

        fn poll_frame(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> FramePoll {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.queued
                .pop_front()
                .map_or(Poll::Pending, |frame| Poll::Ready(Some(Ok(frame))))
        }
    }

    struct UnwindingBody(Arc<AtomicBool>);

    impl Drop for UnwindingBody {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Body for UnwindingBody {
        type Data = Bytes;
        type Error = tonic::Status;

        fn poll_frame(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> FramePoll {
            std::panic::resume_unwind(Box::new(TEST_PANIC))
        }
    }

    fn sample_call() -> Call {
        let mut request = http::Request::builder()
            .uri("/test.v1.StreamService/Watch")
            .body(())
            .unwrap();
        request
            .extensions_mut()
            .insert(tonic::GrpcMethod::new("test.v1.StreamService", "Watch"));
        let destination =
            crate::observe::Destination::new(&"http://127.0.0.1:50051".parse().unwrap());
        Call::start(crate::observe::Series::client(), &request, &destination)
    }

    #[test]
    fn empty_upload_skips_guard_allocation() {
        let (body, guard) = upload(tonic::body::Body::empty());
        assert!(guard.is_none());
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn aborted_upload_yields_cancelled_error_before_end_of_stream() {
        let source = PendingFrames::stalled();
        let dropped = Arc::clone(&source.dropped);
        let (mut outgoing, guard) = upload(tonic::body::Body::new(source));
        assert!(guard.is_some());
        assert!(!outgoing.is_end_stream());

        drop(guard);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(
            !outgoing.is_end_stream(),
            "aborted upload must stay pollable so hyper sends RST_STREAM"
        );

        let first = outgoing.frame().await.expect("cancelled error frame");
        assert_eq!(first.unwrap_err().code(), tonic::Code::Cancelled);
        assert!(outgoing.is_end_stream());
        assert!(outgoing.frame().await.is_none());
    }

    #[tokio::test]
    async fn upload_panic_releases_source_and_resumes_unwind() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (mut outgoing, guard) =
            upload(tonic::body::Body::new(UnwindingBody(Arc::clone(&dropped))));
        let mut cx = Context::from_waker(Waker::noop());
        let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
            Pin::new(&mut outgoing).poll_frame(&mut cx)
        }))
        .unwrap_err();
        assert_eq!(caught.downcast_ref::<&str>(), Some(&TEST_PANIC));
        assert!(dropped.load(Ordering::SeqCst));
        assert!(outgoing.is_end_stream());
        assert!(outgoing.frame().await.is_none());
        drop(guard);
    }

    #[tokio::test]
    async fn unbounded_opening_only_response_does_not_spawn_watcher() {
        let open_without_watch = ClientResponseBody::start(
            PendingFrames::stalled(),
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(None, CancellationToken::new()),
                watch_stop: false,
                guards: ClientGuards::default(),
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        );
        assert!(open_without_watch.watcher.is_none());

        let open_with_watch = ClientResponseBody::start(
            PendingFrames::stalled(),
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(None, CancellationToken::new()),
                watch_stop: true,
                guards: ClientGuards::default(),
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        );
        assert!(open_with_watch.watcher.is_some());

        let already_initial = ClientResponseBody::start(
            PendingFrames::stalled(),
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(None, CancellationToken::new()),
                watch_stop: true,
                guards: ClientGuards::default(),
                on_status: None,
            },
            Some(ClientOutcome::local(tonic::Code::Ok)),
            ClientOutcome::local(tonic::Code::Unknown),
        );
        assert!(already_initial.watcher.is_none());
    }

    #[tokio::test]
    async fn parent_cancellation_releases_an_unbounded_unpolled_response() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let source = PendingFrames::stalled();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let mut body = ClientResponseBody::start(
            source,
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(None, child.clone()),
                watch_stop: true,
                guards: ClientGuards {
                    upload: None,
                    cancel_on_drop: Some(child.drop_guard()),
                },
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        );
        let watcher = body.watcher.take().unwrap();
        parent.cancel();
        tokio::time::timeout(TEST_BUDGET, watcher)
            .await
            .unwrap()
            .unwrap();

        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        let trailers = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        assert_eq!(
            tonic::Status::from_header_map(&trailers).unwrap().code(),
            tonic::Code::Cancelled
        );
        assert!(body.frame().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn response_drop_cancels_unpolled_upload_and_completes_watcher_abort() {
        let cancellation = CancellationToken::new();
        let source = PendingFrames::stalled();
        let dropped = Arc::clone(&source.dropped);
        let upload_source = PendingFrames::stalled();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload_guard) = upload(tonic::body::Body::new(upload_source));
        let mut body = ClientResponseBody::start(
            source,
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(
                    Some(Deadline::new(Instant::now(), TEST_BUDGET)),
                    cancellation.clone(),
                ),
                watch_stop: true,
                guards: ClientGuards {
                    upload: upload_guard,
                    cancel_on_drop: Some(cancellation.clone().drop_guard()),
                },
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        );
        let done = body.watcher_done.take().unwrap();
        drop(body);

        assert!(
            cancellation.is_cancelled(),
            "response drop cancels propagated work"
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert!(upload_dropped.load(Ordering::SeqCst));
        let abort_frame = outgoing.frame().await.expect("upload error frame");
        assert_eq!(abort_frame.unwrap_err().code(), tonic::Code::Cancelled);
        assert!(outgoing.frame().await.is_none());
        assert!(
            tokio::time::timeout(TEST_BUDGET, done)
                .await
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expiry_releases_unpolled_stream_and_aborts_upload() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);

        let response_source = PendingFrames::stalled();
        let response_dropped = Arc::clone(&response_source.dropped);
        let response_polls = Arc::clone(&response_source.polls);

        let upload_source = PendingFrames::stalled();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload_guard) = upload(tonic::body::Body::new(upload_source));

        let lifetime = OperationContext::new(
            Some(Deadline::new(Instant::now(), TEST_BUDGET)),
            CancellationToken::new(),
        );
        let mut body = ClientResponseBody::start(
            response_source,
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime,
                watch_stop: true,
                guards: ClientGuards {
                    upload: upload_guard,
                    cancel_on_drop: None,
                },
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        );
        let watcher = body.watcher.take().unwrap();
        tokio::time::advance(TEST_BUDGET).await;
        tokio::time::timeout(TEST_BUDGET, watcher)
            .await
            .unwrap()
            .unwrap();

        assert!(response_dropped.load(Ordering::SeqCst));
        assert!(upload_dropped.load(Ordering::SeqCst));
        assert_eq!(response_polls.load(Ordering::SeqCst), 0);

        let abort_frame = outgoing.frame().await.expect("upload error frame");
        assert_eq!(abort_frame.unwrap_err().code(), tonic::Code::Cancelled);
        assert!(outgoing.frame().await.is_none());

        let trailers = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        assert_eq!(
            tonic::Status::from_header_map(&trailers).unwrap().code(),
            tonic::Code::DeadlineExceeded
        );
        assert!(body.frame().await.is_none());
        drop(body);

        let rendered = handle.render();
        assert!(
            rendered.contains("grpc_code=\"DeadlineExceeded\"} 1"),
            "{rendered}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn client_response_panic_records_cancelled_and_resumes_unwind() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let observer = tonic::client::ResponseStatusObserver::new({
            let recorded = Arc::clone(&recorded);
            move |code| recorded.lock().push(code)
        });
        let wire_codes = Arc::new(Mutex::new(Vec::new()));
        let on_status: StatusHook = Box::new({
            let wire_codes = Arc::clone(&wire_codes);
            move |code| wire_codes.lock().push(code)
        });
        let dropped = Arc::new(AtomicBool::new(false));
        let lifetime = OperationContext::new(
            Some(Deadline::new(Instant::now(), TEST_BUDGET)),
            CancellationToken::new(),
        );
        let mut body = ClientResponseBody::start(
            UnwindingBody(Arc::clone(&dropped)),
            ClientObservation::Decoded {
                span: tracing::Span::none(),
                observer,
            },
            ClientCustody {
                lifetime,
                watch_stop: true,
                guards: ClientGuards::default(),
                on_status: Some(on_status),
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        );
        let watcher_done = body.watcher_done.take().unwrap();
        let mut cx = Context::from_waker(Waker::noop());
        let caught =
            std::panic::catch_unwind(AssertUnwindSafe(|| Pin::new(&mut body).poll_frame(&mut cx)))
                .unwrap_err();
        assert_eq!(caught.downcast_ref::<&str>(), Some(&TEST_PANIC));
        assert_eq!(*recorded.lock(), [tonic::Code::Cancelled]);
        assert!(wire_codes.lock().is_empty());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(body.is_end_stream());
        assert!(
            tokio::time::timeout(TEST_BUDGET, watcher_done)
                .await
                .unwrap()
                .is_err()
        );
    }
}
