use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use infra_grpc_core::watched::{
    FramePoll, MaybeWatched, TerminableBody, WatchedPoll, register_waker,
};
use operation_context::{OperationContext, Stopped};
use parking_lot::Mutex;
use tokio_util::sync::DropGuard;

use crate::observe::Call;
use crate::protocol::{STATUS_HEADERS, grpc_code_from_http_status, header_status_code};

pub(crate) type StatusHook = Box<dyn FnOnce(tonic::Code) + Send>;
type ClientResponseBody<B> = MaybeWatched<StreamPhase<B>>;

#[expect(dead_code, reason = "guards release resources on drop")]
pub(crate) struct ClientGuards {
    pub(crate) upload_guard: Option<UploadGuard>,
    pub(crate) cancel_on_drop: DropGuard,
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

pub(crate) fn client_deadline_status() -> tonic::Status {
    tonic::Status::deadline_exceeded("request deadline exceeded")
}

pub(crate) fn client_stopped_status(reason: Stopped) -> tonic::Status {
    match reason {
        Stopped::Deadline => client_deadline_status(),
        Stopped::Cancelled => tonic::Status::cancelled("request cancelled"),
    }
}

#[derive(Clone, Copy)]
struct ClientOutcome {
    metric_code: tonic::Code,
    status_hook_code: Option<tonic::Code>,
}

impl ClientOutcome {
    const fn local(code: tonic::Code) -> Self {
        Self {
            metric_code: code,
            status_hook_code: None,
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
            status_hook_code: Some(wire_code),
        })
    }
}

struct ActiveStream<B> {
    body: B,
    observation: ClientObservation,
    _guards: ClientGuards,
    lifetime: OperationContext,
    waker: Option<Waker>,
    on_status: Option<StatusHook>,
    missing_status: ClientOutcome,
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
        if let (Some(code), Some(on_status)) =
            (self.outcome.status_hook_code, self.active.on_status)
        {
            on_status(code);
        }
    }
}

impl<B> StreamPhase<B> {
    fn stopped_status(&self) -> Option<tonic::Status> {
        let Self::Open(active) = self else {
            return None;
        };
        active.lifetime.stopped().map(client_stopped_status)
    }

    fn transition(&mut self, next: Self, outcome: ClientOutcome) -> Option<StreamCompletion<B>> {
        match std::mem::replace(self, next) {
            Self::Open(active) => Some(StreamCompletion { active, outcome }),
            terminal => {
                *self = terminal;
                None
            }
        }
    }

    fn stage_stopped_trailers(&mut self, status: &tonic::Status) -> Option<StreamCompletion<B>> {
        let Self::Open(active) = self else {
            return None;
        };
        active.observation.record_local_stop(status);
        let mut trailers = HeaderMap::new();
        let _ = status.add_header(&mut trailers);
        self.transition(
            Self::Trailers(trailers),
            ClientOutcome::local(status.code()),
        )
    }

    fn poll_stopped(
        &mut self,
        status: &tonic::Status,
    ) -> (FramePoll<tonic::Status>, Option<StreamCompletion<B>>) {
        let completion = self.stage_stopped_trailers(status);
        (self.take_terminal(), completion)
    }

    fn complete_polled(
        &mut self,
        polled: &FramePoll<tonic::Status>,
    ) -> Option<StreamCompletion<B>> {
        let Self::Open(active) = self else {
            return None;
        };
        let outcome = match polled {
            Poll::Ready(Some(Ok(frame))) => {
                let trailers = frame.trailers_ref()?;
                ClientOutcome::from_headers(&active.observation, trailers)
                    .unwrap_or(active.missing_status)
            }
            Poll::Ready(Some(Err(error))) => ClientOutcome::local(error.code()),
            Poll::Ready(None) => active.missing_status,
            Poll::Pending => return None,
        };
        self.transition(Self::Done, outcome)
    }

    fn take_terminal(&mut self) -> FramePoll<tonic::Status> {
        match std::mem::replace(self, Self::Done) {
            Self::Trailers(trailers) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            other => {
                *self = other;
                Poll::Ready(None)
            }
        }
    }
}

impl<B> TerminableBody for StreamPhase<B>
where
    B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
{
    type Error = tonic::Status;
    type Stop = tonic::Status;
    type Finalizer = StreamCompletion<B>;

    fn poll_watched(
        &mut self,
        cx: &mut Context<'_>,
        watched: bool,
    ) -> (WatchedPoll<Self::Error>, Option<Self::Finalizer>) {
        let Self::Open(active) = self else {
            return (Ok(self.take_terminal()), None);
        };
        if let Some(reason) = active.lifetime.stopped() {
            let (frame, completion) = self.poll_stopped(&client_stopped_status(reason));
            return (Ok(frame), completion);
        }
        if watched {
            register_waker(&mut active.waker, cx.waker());
        }
        let polled = {
            let _entered = active.observation.span().enter();
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                Pin::new(&mut active.body).poll_frame(cx)
            }))
        };
        let polled = match polled {
            Ok(polled) => polled,
            Err(panic) => {
                let completion =
                    self.transition(Self::Done, ClientOutcome::local(tonic::Code::Cancelled));
                return (Err(panic), completion);
            }
        };
        if let Some(status) = self.stopped_status() {
            let (frame, completion) = self.poll_stopped(&status);
            return (Ok(frame), completion);
        }
        let completion = self.complete_polled(&polled);
        (Ok(polled), completion)
    }

    fn terminate(&mut self, stop: Self::Stop) -> Option<(Option<Waker>, Self::Finalizer)> {
        let mut completion = self.stage_stopped_trailers(&stop)?;
        let waker = completion.active.waker.take();
        Some((waker, completion))
    }

    fn take_for_drop(&mut self) -> Self {
        std::mem::replace(self, Self::Done)
    }

    fn is_end_stream(&self) -> bool {
        matches!(self, Self::Done)
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Open(active) => active.body.size_hint(),
            Self::Trailers(_) | Self::Done => SizeHint::with_exact(0),
        }
    }

    fn watcher_lost() -> Self::Stop {
        tonic::Status::internal("internal error")
    }

    fn finish(finalizer: Self::Finalizer) {
        finalizer.finish();
    }
}

pub(crate) fn attach_client_response(
    mut response: http::Response<tonic::body::Body>,
    observation: ClientObservation,
    custody: ClientCustody,
) -> http::Response<tonic::body::Body> {
    let initial = if let Some(reason) = custody.lifetime.stopped() {
        let status = client_stopped_status(reason);
        observation.record_local_stop(&status);
        for name in STATUS_HEADERS {
            response.headers_mut().remove(name);
        }
        let _ = status.add_header(response.headers_mut());
        Some(ClientOutcome::local(status.code()))
    } else {
        ClientOutcome::from_headers(&observation, response.headers())
    };
    let missing_status = grpc_code_from_http_status(response.status()).map_or(
        ClientOutcome::local(tonic::Code::Unknown),
        |code| ClientOutcome {
            metric_code: code,
            status_hook_code: Some(code),
        },
    );
    response.map(|body| {
        start_response_body(body, observation, custody, initial, missing_status)
            .map_or_else(tonic::body::Body::empty, tonic::body::Body::new)
    })
}

fn start_response_body<B>(
    body: B,
    observation: ClientObservation,
    custody: ClientCustody,
    initial: Option<ClientOutcome>,
    missing_status: ClientOutcome,
) -> Option<ClientResponseBody<B>>
where
    B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
{
    let ended = body.is_end_stream();
    let watch_stop = custody.watch_stop;
    let mut phase = StreamPhase::Open(ActiveStream {
        body,
        observation,
        _guards: custody.guards,
        lifetime: custody.lifetime,
        waker: None,
        on_status: custody.on_status,
        missing_status,
    });
    let completion = if let Some(outcome) = initial {
        phase.transition(StreamPhase::Done, outcome)
    } else if let Some(status) = phase.stopped_status() {
        phase.stage_stopped_trailers(&status)
    } else if ended {
        phase.transition(StreamPhase::Done, missing_status)
    } else {
        None
    };
    if let Some(completion) = completion {
        completion.finish();
    }
    match &phase {
        StreamPhase::Open(active) if watch_stop => {
            let lifetime = active.lifetime.clone();
            Some(MaybeWatched::watched(phase, async move {
                client_stopped_status(lifetime.wait_stopped().await)
            }))
        }
        StreamPhase::Open(_) | StreamPhase::Trailers(_) => Some(MaybeWatched::local(phase)),
        StreamPhase::Done => None,
    }
}

enum UploadPhase {
    Open {
        body: tonic::body::Body,
        waker: Option<Waker>,
    },
    Aborted,
    Done,
}

pub(crate) struct UploadGuard(Arc<Mutex<UploadPhase>>);
struct Upload(Arc<Mutex<UploadPhase>>);

pub(crate) fn guard_upload(body: tonic::body::Body) -> (tonic::body::Body, Option<UploadGuard>) {
    if body.is_end_stream() {
        return (body, None);
    }
    let phase = Arc::new(Mutex::new(UploadPhase::Open { body, waker: None }));
    (
        tonic::body::Body::new(Upload(Arc::clone(&phase))),
        Some(UploadGuard(phase)),
    )
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        let (released, waker) = {
            let mut phase = self.0.lock();
            match &mut *phase {
                UploadPhase::Open { body, waker } => {
                    let (next, waker) = if body.is_end_stream() {
                        (UploadPhase::Done, None)
                    } else {
                        (UploadPhase::Aborted, waker.take())
                    };
                    (Some(std::mem::replace(&mut *phase, next)), waker)
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
        let mut phase = self.0.lock();
        let (polled, finished) = match &mut *phase {
            UploadPhase::Open { body, waker } => {
                register_waker(waker, context.waker());
                let polled = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    Pin::new(&mut *body).poll_frame(context)
                }));
                let finished = matches!(polled, Ok(Poll::Ready(None | Some(Err(_)))) | Err(_))
                    || body.is_end_stream();
                (polled, finished)
            }
            UploadPhase::Aborted => {
                *phase = UploadPhase::Done;
                return Poll::Ready(Some(Err(tonic::Status::cancelled("call cancelled"))));
            }
            UploadPhase::Done => return Poll::Ready(None),
        };
        let released = finished.then(|| std::mem::replace(&mut *phase, UploadPhase::Done));
        drop(phase);
        drop(released);
        match polled {
            Ok(polled) => polled,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn is_end_stream(&self) -> bool {
        let phase = self.0.lock();
        match &*phase {
            UploadPhase::Open { body, .. } => body.is_end_stream(),
            UploadPhase::Aborted => false,
            UploadPhase::Done => true,
        }
    }

    fn size_hint(&self) -> SizeHint {
        let phase = self.0.lock();
        match &*phase {
            UploadPhase::Open { body, .. } => body.size_hint(),
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
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    use super::*;

    const TEST_BUDGET: Duration = Duration::from_secs(1);
    const TEST_PANIC: &str = "stream source panic";

    struct PendingFrames {
        queued: VecDeque<Frame<Bytes>>,
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
        on_drop: Option<oneshot::Sender<()>>,
    }

    impl PendingFrames {
        fn stalled() -> Self {
            Self {
                queued: VecDeque::new(),
                polls: Arc::default(),
                dropped: Arc::default(),
                on_drop: None,
            }
        }

        fn stalled_with_drop() -> (Self, oneshot::Receiver<()>) {
            let (tx, rx) = oneshot::channel();
            (
                Self {
                    queued: VecDeque::new(),
                    polls: Arc::default(),
                    dropped: Arc::default(),
                    on_drop: Some(tx),
                },
                rx,
            )
        }
    }

    impl Drop for PendingFrames {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
            if let Some(tx) = self.on_drop.take() {
                let _ = tx.send(());
            }
        }
    }

    impl Body for PendingFrames {
        type Data = Bytes;
        type Error = tonic::Status;

        fn poll_frame(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> FramePoll<tonic::Status> {
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

        fn poll_frame(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> FramePoll<tonic::Status> {
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
            crate::observe::Destination::from_uri(&"http://127.0.0.1:50051".parse().unwrap());
        crate::observe::start_call(
            crate::observe::Series::client(),
            &request,
            destination.as_ref(),
        )
    }

    #[test]
    fn empty_upload_skips_guard_allocation() {
        let (body, guard) = guard_upload(tonic::body::Body::empty());
        assert!(guard.is_none());
        assert!(body.is_end_stream());
    }

    #[tokio::test]
    async fn aborted_upload_yields_cancelled_error_before_end_of_stream() {
        let source = PendingFrames::stalled();
        let dropped = Arc::clone(&source.dropped);
        let (mut outgoing, guard) = guard_upload(tonic::body::Body::new(source));
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
            guard_upload(tonic::body::Body::new(UnwindingBody(Arc::clone(&dropped))));
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

    fn test_guards(upload_guard: Option<UploadGuard>) -> ClientGuards {
        ClientGuards {
            upload_guard,
            cancel_on_drop: CancellationToken::new().drop_guard(),
        }
    }

    #[tokio::test]
    async fn response_start_watches_open_streams_and_releases_terminal_custody() {
        let open_without_watch = start_response_body(
            PendingFrames::stalled(),
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::unbounded(),
                watch_stop: false,
                guards: test_guards(None),
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        )
        .unwrap();
        assert!(!open_without_watch.is_watching());

        let open_with_watch = start_response_body(
            PendingFrames::stalled(),
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::unbounded(),
                watch_stop: true,
                guards: test_guards(None),
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        )
        .unwrap();
        assert!(open_with_watch.is_watching());

        let source = PendingFrames::stalled();
        let dropped = Arc::clone(&source.dropped);
        let upload = PendingFrames::stalled();
        let upload_dropped = Arc::clone(&upload.dropped);
        let (_outgoing, upload_guard) = guard_upload(tonic::body::Body::new(upload));
        let cancellation = CancellationToken::new();
        let (report, mut wire_code) = oneshot::channel();
        let response = attach_client_response(
            http::Response::builder()
                .header(tonic::Status::GRPC_STATUS, "14")
                .body(tonic::body::Body::new(source))
                .unwrap(),
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::unbounded(),
                watch_stop: true,
                guards: ClientGuards {
                    upload_guard,
                    cancel_on_drop: cancellation.clone().drop_guard(),
                },
                on_status: Some(Box::new(move |code| report.send(code).unwrap())),
            },
        );
        assert_eq!(wire_code.try_recv().unwrap(), tonic::Code::Unavailable);
        assert!(cancellation.is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(upload_dropped.load(Ordering::SeqCst));
        assert!(response.body().is_end_stream());
        assert_eq!(response.body().size_hint().exact(), Some(0));
    }

    #[tokio::test]
    async fn parent_cancellation_releases_an_unbounded_unpolled_response() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let (source, dropped_rx) = PendingFrames::stalled_with_drop();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let mut body = start_response_body(
            source,
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(None, child.clone()),
                watch_stop: true,
                guards: ClientGuards {
                    upload_guard: None,
                    cancel_on_drop: child.drop_guard(),
                },
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        )
        .unwrap();
        parent.cancel();
        tokio::time::timeout(TEST_BUDGET, dropped_rx)
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
    async fn response_drop_cancels_unpolled_upload() {
        let cancellation = CancellationToken::new();
        let source = PendingFrames::stalled();
        let dropped = Arc::clone(&source.dropped);
        let upload_source = PendingFrames::stalled();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload_guard) = guard_upload(tonic::body::Body::new(upload_source));
        let body = start_response_body(
            source,
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime: OperationContext::new(
                    Some(Deadline::after(TEST_BUDGET)),
                    cancellation.clone(),
                ),
                watch_stop: true,
                guards: ClientGuards {
                    upload_guard,
                    cancel_on_drop: cancellation.clone().drop_guard(),
                },
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        )
        .unwrap();
        assert!(body.is_watching());
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
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_expiry_releases_unpolled_stream_and_aborts_upload() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _guard = metrics::set_default_local_recorder(&recorder);

        let response_source = PendingFrames::stalled();
        let response_dropped = Arc::clone(&response_source.dropped);
        let response_polls = Arc::clone(&response_source.polls);

        let (upload_source, upload_dropped_rx) = PendingFrames::stalled_with_drop();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload_guard) = guard_upload(tonic::body::Body::new(upload_source));

        let lifetime = OperationContext::with_timeout(TEST_BUDGET);
        let mut body = start_response_body(
            response_source,
            ClientObservation::Transport(sample_call()),
            ClientCustody {
                lifetime,
                watch_stop: true,
                guards: test_guards(upload_guard),
                on_status: None,
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        )
        .unwrap();
        tokio::time::advance(TEST_BUDGET).await;
        tokio::time::timeout(TEST_BUDGET, upload_dropped_rx)
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
        let lifetime = OperationContext::with_timeout(TEST_BUDGET);
        let mut body = start_response_body(
            UnwindingBody(Arc::clone(&dropped)),
            ClientObservation::Decoded {
                span: tracing::Span::none(),
                observer,
            },
            ClientCustody {
                lifetime,
                watch_stop: true,
                guards: test_guards(None),
                on_status: Some(on_status),
            },
            None,
            ClientOutcome::local(tonic::Code::Unknown),
        )
        .unwrap();
        assert!(body.is_watching());
        let mut cx = Context::from_waker(Waker::noop());
        let caught =
            std::panic::catch_unwind(AssertUnwindSafe(|| Pin::new(&mut body).poll_frame(&mut cx)))
                .unwrap_err();
        assert_eq!(caught.downcast_ref::<&str>(), Some(&TEST_PANIC));
        assert!(!body.is_watching());
        assert_eq!(*recorded.lock(), [tonic::Code::Cancelled]);
        assert!(wire_codes.lock().is_empty());
        assert!(dropped.load(Ordering::SeqCst));
        assert!(body.is_end_stream());
    }
}
