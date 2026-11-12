//! One terminal owner for native response work, capacity and upload cancellation.

use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use opentelemetry::trace::SpanKind;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::task::AbortOnDropHandle;

use crate::observe::Call;
use crate::protocol::{header_status_code, http_status_code};
use crate::status::internal_status;

use operation_context::{OperationContext, Stopped};
use tokio_util::sync::DropGuard;

/// Transfers one value through HTTP extensions, which require `Clone`:
/// cloning the holder never clones the value, which one owner takes.
#[derive_where::derive_where(Clone)]
pub(crate) struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T> Handoff<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    pub(crate) fn take(self) -> Option<T> {
        lock(&self.0).take()
    }
}

/// What the terminal response owner keeps until the call's status is known.
#[derive(Default)]
pub(crate) struct ResponseCustody {
    /// The lifetime that stops the response; `None` starts no stop watcher.
    pub(crate) context: Option<OperationContext>,
    pub(crate) held: Held,
    pub(crate) on_status: Option<ResponseStatusObserver>,
}

pub(crate) type ResponseStatusObserver = Box<dyn FnOnce(tonic::Code) + Send>;

/// Guards a call keeps until its status is known. They drop in declaration
/// order, after the response body and the call record: the admission
/// permit, then the upload, then the cancellation of propagated work.
#[derive(Default)]
#[expect(dead_code, reason = "each guard acts only when dropped")]
pub(crate) struct Held {
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) upload: Option<UploadGuard>,
    pub(crate) cancel_on_drop: Option<DropGuard>,
}

impl ResponseCustody {
    /// The custody the server layers hand over in the response extensions:
    /// the caller lifetime from the `opening` layer, its cancellation guard,
    /// and the business admission permit.
    pub(crate) fn from_server_response<B>(response: &mut http::Response<B>) -> Self {
        let extensions = response.extensions_mut();
        Self {
            on_status: None,
            context: extensions
                .remove::<crate::ResponseContext>()
                .map(crate::ResponseContext::into_operation),
            held: Held {
                permit: extensions
                    .remove::<Handoff<OwnedSemaphorePermit>>()
                    .and_then(Handoff::take),
                upload: None,
                cancel_on_drop: extensions
                    .remove::<Handoff<DropGuard>>()
                    .and_then(Handoff::take),
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Side {
    Server,
    Client,
}

impl Side {
    /// A server answers its own deadline with the catalog failure; a client
    /// reports the caller's deadline as the bare status.
    pub(crate) fn deadline(self) -> tonic::Status {
        match self {
            Self::Server => crate::Failure::new(service_failure::Code::GatewayTimeout).into(),
            Self::Client => tonic::Status::deadline_exceeded("request deadline exceeded"),
        }
    }

    pub(crate) fn stopped(self, reason: Stopped) -> tonic::Status {
        match reason {
            Stopped::Deadline => self.deadline(),
            Stopped::Cancelled => tonic::Status::cancelled("request cancelled"),
        }
    }

    pub(crate) const fn span_kind(self) -> SpanKind {
        match self {
            Self::Server => SpanKind::Server,
            Self::Client => SpanKind::Client,
        }
    }
}

fn header_catalog_code(headers: &HeaderMap) -> Option<service_failure::Code> {
    // The catalog code travels only in the details, which tonic writes only
    // when they are non-empty; without them its parse, which clones the whole
    // map, is skipped on every successful call.
    headers.get("grpc-status-details-bin")?;
    // Tonic's details decoder can panic on invalid base64; feature headers
    // cannot poison the terminal owner or skip its cleanup.
    std::panic::catch_unwind(|| tonic::Status::from_header_map(headers))
        .ok()
        .flatten()
        .as_ref()
        .and_then(crate::status::catalog_code)
}

/// How a call ended: its status code and the catalog failure behind it.
/// Only a server answers with the catalog, so a client's outcome never reads
/// one, and never runs tonic's details decoder.
#[derive(Clone, Copy)]
struct Outcome {
    code: tonic::Code,
    failure: Option<service_failure::Code>,
    response_derived: bool,
}

impl Outcome {
    const fn bare(code: tonic::Code) -> Self {
        Self {
            code,
            failure: None,
            response_derived: false,
        }
    }

    fn of_status(side: Side, status: &tonic::Status) -> Self {
        Self {
            code: status.code(),
            response_derived: false,
            failure: match side {
                Side::Server => crate::status::catalog_code(status),
                Side::Client => None,
            },
        }
    }

    /// The outcome of response headers. A server's own failure is taken from
    /// the status tonic leaves in the extensions when it writes one, which
    /// saves decoding it from the headers again.
    fn from_response<B>(side: Side, response: &http::Response<B>) -> Option<Self> {
        match (side, response.extensions().get::<tonic::Status>()) {
            (Side::Server, Some(status)) => Some(Self {
                response_derived: true,
                ..Self::of_status(side, status)
            }),
            _ => Self::from_status_headers(side, response.headers()),
        }
    }

    /// Reads the outcome from trailers or the initial headers of a trailers-only answer.
    fn from_status_headers(side: Side, headers: &HeaderMap) -> Option<Self> {
        let code = header_status_code(headers)?;
        Some(Self {
            code,
            response_derived: true,
            failure: match side {
                Side::Server => header_catalog_code(headers),
                Side::Client => None,
            },
        })
    }
}

type ResponseFramePoll<E> = Poll<Option<Result<Frame<Bytes>, E>>>;

fn store_waker(slot: &mut Option<Waker>, waker: &Waker) {
    match slot {
        Some(stored) => stored.clone_from(waker),
        None => *slot = Some(waker.clone()),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Resources<B> {
    body: B,
    call: Call,
    _held: Held,
}

/// A call whose status is not known yet, with everything only it needs.
struct OpenCall<B: Body> {
    resources: Resources<B>,
    context: Option<OperationContext>,
    fallback: Option<opentelemetry::Context>,
    waker: Option<Waker>,
    error_code: fn(&B::Error) -> tonic::Code,
    on_status: Option<ResponseStatusObserver>,
}

/// Where a response stands. A locally generated status waits in `Trailers`
/// until the body is polled once more.
enum Phase<B: Body> {
    Open(OpenCall<B>),
    Trailers(HeaderMap),
    Done,
}

struct Completion<B: Body> {
    open: OpenCall<B>,
    outcome: Outcome,
}

impl<B: Body> Completion<B> {
    fn finish(self) {
        let call = self.open.resources.call;
        if let Some(failure) = self.outcome.failure {
            call.failed(failure);
        }
        call.finish(self.outcome.code);
        if self.outcome.response_derived
            && let Some(on_status) = self.open.on_status
        {
            on_status(self.outcome.code);
        }
        // Remaining fields, including feature destructors, drop outside the lock.
    }
}

struct State<B: Body> {
    phase: Phase<B>,
    side: Side,
    missing_status: Outcome,
}

impl<B: Body> State<B> {
    fn stopped_status(&self) -> Option<tonic::Status> {
        let Phase::Open(open) = &self.phase else {
            return None;
        };
        let reason = open.context.as_ref()?.stopped()?;
        Some(self.side.stopped(reason))
    }

    /// Ends an open call, moving to `next`; `None` once it has ended.
    fn close(&mut self, next: Phase<B>, outcome: Outcome) -> Option<Completion<B>> {
        match std::mem::replace(&mut self.phase, next) {
            Phase::Open(open) => Some(Completion { open, outcome }),
            ended => {
                self.phase = ended;
                None
            }
        }
    }

    /// Completes with a locally generated status, sent as the trailers.
    fn complete_with(&mut self, status: &tonic::Status) -> Option<Completion<B>> {
        let mut trailers = HeaderMap::new();
        // Locally generated sanitized statuses always encode.
        let _ = status.add_header(&mut trailers);
        self.close(
            Phase::Trailers(trailers),
            Outcome::of_status(self.side, status),
        )
    }

    /// Completes with a locally generated status and yields its trailers.
    fn end_with(
        &mut self,
        status: &tonic::Status,
    ) -> (ResponseFramePoll<B::Error>, Option<Completion<B>>) {
        let completion = self.complete_with(status);
        (self.take_terminal(), completion)
    }

    /// Completes from a frame the body yielded; `None` while the call is open.
    fn complete_from(&mut self, polled: &ResponseFramePoll<B::Error>) -> Option<Completion<B>> {
        let outcome = match polled {
            Poll::Ready(Some(Ok(frame))) => {
                let trailers = frame.trailers_ref()?;
                Outcome::from_status_headers(self.side, trailers).unwrap_or(self.missing_status)
            }
            Poll::Ready(Some(Err(error))) => {
                let Phase::Open(open) = &self.phase else {
                    return None;
                };
                Outcome::bare((open.error_code)(error))
            }
            Poll::Ready(None) => self.missing_status,
            Poll::Pending => return None,
        };
        self.close(Phase::Done, outcome)
    }

    fn take_terminal(&mut self) -> ResponseFramePoll<B::Error> {
        match std::mem::replace(&mut self.phase, Phase::Done) {
            Phase::Trailers(trailers) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            phase => {
                self.phase = phase;
                Poll::Ready(None)
            }
        }
    }
}

/// Pass-through response custody. The stop watcher holds only a Weak reference.
struct ResponseBody<B: Body> {
    state: Arc<Mutex<State<B>>>,
    watcher: Option<AbortOnDropHandle<()>>,
    #[cfg(test)]
    watcher_done: Option<tokio::sync::oneshot::Receiver<()>>,
}

pub(crate) fn attach<B>(
    mut response: http::Response<B>,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    custody: ResponseCustody,
    error_code: fn(&B::Error) -> tonic::Code,
) -> http::Response<impl Body<Data = Bytes, Error = B::Error> + Send + 'static>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    let side = call.side();
    // Check the original lifetime at handoff; headers do not restart it.
    let stopped = custody
        .context
        .as_ref()
        .and_then(OperationContext::stopped)
        .map(|reason| side.stopped(reason));
    let initial = if let Some(status) = stopped {
        for name in ["grpc-status", "grpc-message", "grpc-status-details-bin"] {
            response.headers_mut().remove(name);
        }
        let _ = status.add_header(response.headers_mut());
        Some(Outcome::of_status(side, &status))
    } else {
        Outcome::from_response(side, &response)
    };
    if response
        .extensions_mut()
        .remove::<crate::router::Shed>()
        .is_some()
        && initial.is_some_and(|outcome| outcome.code == tonic::Code::ResourceExhausted)
    {
        crate::observe::record_shed();
    }
    let missing_status = match side {
        Side::Client => http_status_code(response.status()),
        Side::Server => None,
    }
    .map_or(Outcome::bare(tonic::Code::Unknown), |code| Outcome {
        response_derived: true,
        ..Outcome::bare(code)
    });
    response.map(|body| {
        ResponseBody::new(
            body,
            call,
            fallback,
            custody,
            error_code,
            initial,
            missing_status,
        )
    })
}

impl<B> ResponseBody<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    fn new(
        body: B,
        call: Call,
        fallback: Option<opentelemetry::Context>,
        custody: ResponseCustody,
        error_code: fn(&B::Error) -> tonic::Code,
        initial: Option<Outcome>,
        missing_status: Outcome,
    ) -> Self {
        let ended = body.is_end_stream();
        let side = call.side();
        let mut state = State {
            phase: Phase::Open(OpenCall {
                resources: Resources {
                    body,
                    call,
                    _held: custody.held,
                },
                context: custody.context,
                fallback,
                waker: None,
                error_code,
                on_status: custody.on_status,
            }),
            side,
            missing_status,
        };
        let completion = if let Some(outcome) = initial {
            state.close(Phase::Done, outcome)
        } else if let Some(status) = state.stopped_status() {
            state.complete_with(&status)
        } else if ended {
            state.close(Phase::Done, missing_status)
        } else {
            None
        };
        if let Some(completion) = completion {
            completion.finish();
        }
        let watched = match &state.phase {
            Phase::Open(open) => open.context.clone(),
            Phase::Trailers(_) | Phase::Done => None,
        };
        let mut this = Self {
            state: Arc::new(Mutex::new(state)),
            watcher: None,
            #[cfg(test)]
            watcher_done: None,
        };
        if let Some(context) = watched {
            this.watch(context, side);
        }
        this
    }

    /// Spawns the task that completes the call once `context` stops, even
    /// while nobody polls the body.
    fn watch(&mut self, context: OperationContext, side: Side) {
        let weak = Arc::downgrade(&self.state);
        #[cfg(test)]
        let (done, receiver) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        {
            self.watcher_done = Some(receiver);
        }
        self.watcher = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            // Captured before first poll: abortion also drops the sender.
            #[cfg(test)]
            let _done = done;
            let result = AssertUnwindSafe(async {
                let reason = context.wait_stopped().await;
                if let Some(state) = weak.upgrade() {
                    terminate(&state, &side.stopped(reason));
                }
            })
            .catch_unwind()
            .await;
            if result.is_err()
                && let Some(state) = weak.upgrade()
            {
                terminate(&state, &internal_status());
            }
        })));
    }

    fn reap_watcher(&mut self, context: &mut Context<'_>) {
        if let Some(watcher) = &mut self.watcher
            && let Poll::Ready(result) = std::future::Future::poll(Pin::new(watcher), context)
        {
            self.watcher = None;
            if result.is_err() {
                terminate(&self.state, &internal_status());
            }
        }
    }
}

fn terminate<B: Body>(state: &Mutex<State<B>>, status: &tonic::Status) {
    let completion = lock(state).complete_with(status);
    if let Some(mut completion) = completion {
        if let Some(waker) = completion.open.waker.take() {
            waker.wake();
        }
        completion.finish();
    }
}

impl<B> Body for ResponseBody<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(self: Pin<&mut Self>, context: &mut Context<'_>) -> ResponseFramePoll<B::Error> {
        let this = self.get_mut();
        this.reap_watcher(context);
        let mut state = lock(&this.state);
        let mut panic = None;
        let (polled, completion) = if let Some(status) = state.stopped_status() {
            state.end_with(&status)
        } else {
            let side = state.side;
            let Phase::Open(open) = &mut state.phase else {
                return state.take_terminal();
            };
            store_waker(&mut open.waker, context.waker());
            let polled = {
                let resources = &mut open.resources;
                let _span = resources.call.span().enter();
                let _fallback = open.fallback.clone().map(opentelemetry::Context::attach);
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    Pin::new(&mut resources.body).poll_frame(context)
                }))
            };
            match polled {
                // A stop observed after the poll wins over whatever it yielded.
                Ok(polled) => {
                    if let Some(status) = state.stopped_status() {
                        state.end_with(&status)
                    } else {
                        let completion = state.complete_from(&polled);
                        (polled, completion)
                    }
                }
                Err(caught) if matches!(side, Side::Client) => {
                    panic = Some(caught);
                    let completion =
                        state.close(Phase::Done, Outcome::bare(tonic::Code::Cancelled));
                    (Poll::Ready(None), completion)
                }
                Err(_panic) => state.end_with(&internal_status()),
            }
        };
        drop(state);
        if let Some(completion) = completion {
            // Dropping the handle aborts the watcher.
            this.watcher = None;
            completion.finish();
        }
        if let Some(panic) = panic {
            std::panic::resume_unwind(panic);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        matches!(lock(&self.state).phase, Phase::Done)
    }

    fn size_hint(&self) -> SizeHint {
        match &lock(&self.state).phase {
            Phase::Open(open) => open.resources.body.size_hint(),
            Phase::Trailers(_) | Phase::Done => SizeHint::with_exact(0),
        }
    }
}

impl<B: Body> Drop for ResponseBody<B> {
    fn drop(&mut self) {
        let phase = std::mem::replace(&mut lock(&self.state).phase, Phase::Done);
        drop(self.watcher.take());
        // Call's drop records cancellation only while still open.
        drop(phase);
    }
}

struct UploadState {
    body: Option<tonic::body::Body>,
    waker: Option<Waker>,
}

/// The opening future, then response custody, owns source cancellation.
pub(crate) struct UploadGuard(Arc<Mutex<UploadState>>);
struct Upload(Arc<Mutex<UploadState>>);

pub(crate) fn upload(body: tonic::body::Body) -> (tonic::body::Body, UploadGuard) {
    let state = Arc::new(Mutex::new(UploadState {
        body: Some(body),
        waker: None,
    }));
    (
        tonic::body::Body::new(Upload(Arc::clone(&state))),
        UploadGuard(state),
    )
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        let (body, waker) = {
            let mut state = lock(&self.0);
            (state.body.take(), state.waker.take())
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        drop(body);
    }
}

impl Body for Upload {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let mut state = lock(&self.0);
        store_waker(&mut state.waker, context.waker());
        let Some(body) = state.body.as_mut() else {
            return Poll::Ready(None);
        };
        // Leave source destruction outside the lock even on an unwinding poll.
        let polled =
            std::panic::catch_unwind(AssertUnwindSafe(|| Pin::new(body).poll_frame(context)));
        let ended = matches!(polled, Ok(Poll::Ready(None | Some(Err(_)))) | Err(_));
        let body = if ended { state.body.take() } else { None };
        drop(state);
        drop(body);
        match polled {
            Ok(polled) => polled,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    fn is_end_stream(&self) -> bool {
        lock(&self.0).body.as_ref().is_none_or(Body::is_end_stream)
    }

    fn size_hint(&self) -> SizeHint {
        lock(&self.0)
            .body
            .as_ref()
            .map_or_else(|| SizeHint::with_exact(0), Body::size_hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };
    use operation_context::Deadline;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    const BUDGET: Duration = Duration::from_secs(1);

    fn deadline_context() -> OperationContext {
        OperationContext::new(
            Some(Deadline::new(Instant::now(), BUDGET)),
            CancellationToken::new(),
        )
    }

    struct ProbeBody {
        frames: VecDeque<Frame<Bytes>>,
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl ProbeBody {
        fn pending() -> Self {
            Self {
                frames: VecDeque::new(),
                polls: Arc::default(),
                dropped: Arc::default(),
            }
        }
    }

    impl Drop for ProbeBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl Body for ProbeBody {
        type Data = Bytes;
        type Error = tonic::Status;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.frames
                .pop_front()
                .map_or(Poll::Pending, |frame| Poll::Ready(Some(Ok(frame))))
        }
    }

    fn call() -> Call {
        let request = http::Request::builder()
            .uri("/test.Service/Stream")
            .body(())
            .unwrap();
        Call::start(
            &crate::observe::Series::server(["/test.Service/Stream".into()].into_iter().collect()),
            &request,
        )
    }

    fn response<B>(body: B, custody: ResponseCustody) -> ResponseBody<B>
    where
        B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
    {
        ResponseBody::new(
            body,
            call(),
            None,
            custody,
            tonic::Status::code,
            None,
            Outcome::bare(tonic::Code::Unknown),
        )
    }

    #[tokio::test]
    async fn parent_cancellation_releases_an_unbounded_unpolled_response() {
        let parent = CancellationToken::new();
        let child = parent.child_token();
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let permits = Arc::new(Semaphore::new(1));
        let mut body = response(
            source,
            ResponseCustody {
                on_status: None,
                context: Some(OperationContext::new(None, child.clone())),
                held: Held {
                    permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                    cancel_on_drop: Some(child.drop_guard()),
                    ..Held::default()
                },
            },
        );
        let watcher = body.watcher.take().unwrap();
        parent.cancel();
        tokio::time::timeout(BUDGET, watcher)
            .await
            .unwrap()
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert_eq!(permits.available_permits(), 1);
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
    async fn deadline_releases_unpolled_resources_and_joins_its_watcher() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let upload_source = ProbeBody::pending();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload) = upload(tonic::body::Body::new(upload_source));
        let mut body = response(
            source,
            ResponseCustody {
                on_status: None,
                context: Some(deadline_context()),
                held: Held {
                    permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                    upload: Some(upload),
                    ..Held::default()
                },
            },
        );
        let watcher = body.watcher.take().unwrap();
        tokio::time::advance(BUDGET).await;
        tokio::time::timeout(BUDGET, watcher)
            .await
            .unwrap()
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(upload_dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert_eq!(
            polls.load(Ordering::SeqCst),
            0,
            "expiry must not poll feature work"
        );
        assert!(outgoing.frame().await.is_none());
        assert!(!body.is_end_stream(), "deadline trailers remain readable");
        let trailers = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        let status = tonic::Status::from_header_map(&trailers).unwrap();
        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(
            crate::status::catalog_code(&status),
            Some(service_failure::Code::GatewayTimeout)
        );
        assert!(body.frame().await.is_none());
        drop(body);
        let rendered = handle.render();
        assert!(
            rendered.contains("grpc_code=\"DeadlineExceeded\"} 1"),
            "{rendered}"
        );
        assert!(!rendered.contains("grpc_code=\"Canceled\""), "{rendered}");
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_status_releases_capacity_once_and_cancels_the_watcher() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let mut source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let mut trailers = HeaderMap::new();
        tonic::Status::aborted("peer terminal")
            .add_header(&mut trailers)
            .unwrap();
        source.frames.push_back(Frame::trailers(trailers));
        let mut body = response(
            source,
            ResponseCustody {
                on_status: None,
                context: Some(deadline_context()),
                held: Held {
                    permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                    ..Held::default()
                },
            },
        );
        let done = body.watcher_done.take().unwrap();
        assert_eq!(
            header_status_code(body.frame().await.unwrap().unwrap().trailers_ref().unwrap()),
            Some(tonic::Code::Aborted)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(body.watcher.is_none(), "completion releases the watcher");
        assert!(tokio::time::timeout(BUDGET, done).await.unwrap().is_err());
        tokio::time::advance(BUDGET).await;
        assert!(body.frame().await.is_none());
        drop(body);
        let rendered = handle.render();
        assert!(rendered.contains("grpc_code=\"Aborted\"} 1"), "{rendered}");
        assert!(
            !rendered.contains("grpc_code=\"DeadlineExceeded\""),
            "{rendered}"
        );
        assert!(!rendered.contains("grpc_code=\"Canceled\""), "{rendered}");
    }

    #[tokio::test(start_paused = true)]
    async fn response_drop_cancels_unpolled_upload_and_completes_watcher_abort() {
        let cancellation = CancellationToken::new();
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let upload_source = ProbeBody::pending();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload) = upload(tonic::body::Body::new(upload_source));
        let mut body = response(
            source,
            ResponseCustody {
                on_status: None,
                context: Some(OperationContext::new(
                    Some(Deadline::new(Instant::now(), BUDGET)),
                    cancellation.clone(),
                )),
                held: Held {
                    permit: None,
                    upload: Some(upload),
                    cancel_on_drop: Some(cancellation.clone().drop_guard()),
                },
            },
        );
        // Only tests retain this completion receiver. Its sender is owned by
        // the watcher future, so closure observes destruction after abort.
        let done = body.watcher_done.take().unwrap();
        drop(body);
        assert!(
            cancellation.is_cancelled(),
            "response drop cancels propagated work"
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert!(upload_dropped.load(Ordering::SeqCst));
        assert!(outgoing.frame().await.is_none());
        assert!(tokio::time::timeout(BUDGET, done).await.unwrap().is_err());
    }

    #[test]
    fn a_stop_answers_by_side() {
        let server = Side::Server.stopped(Stopped::Deadline);
        assert_eq!(server.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(
            crate::status::catalog_code(&server),
            Some(service_failure::Code::GatewayTimeout)
        );
        let client = Side::Client.stopped(Stopped::Deadline);
        assert_eq!(client.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(crate::status::catalog_code(&client), None);
        for side in [Side::Server, Side::Client] {
            assert_eq!(
                side.stopped(Stopped::Cancelled).code(),
                tonic::Code::Cancelled
            );
        }
    }

    #[tokio::test]
    async fn lazy_poll_attaches_disabled_span_fallback_without_leaking_context() {
        struct ContextBody(Arc<Mutex<Vec<TraceId>>>);
        impl Body for ContextBody {
            type Data = Bytes;
            type Error = tonic::Status;
            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
                lock(&self.0).push(
                    opentelemetry::Context::current()
                        .span()
                        .span_context()
                        .trace_id(),
                );
                Poll::Pending
            }
        }
        let trace_id = TraceId::from_bytes([7; 16]);
        let fallback = opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
            trace_id,
            SpanId::from_bytes([8; 8]),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        ));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let call =
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), call);
        assert!(call.span().is_disabled());
        let mut body = ResponseBody::new(
            ContextBody(Arc::clone(&seen)),
            call,
            Some(fallback),
            ResponseCustody::default(),
            tonic::Status::code,
            None,
            Outcome::bare(tonic::Code::Unknown),
        );
        let ambient = opentelemetry::Context::current()
            .span()
            .span_context()
            .trace_id();
        assert!(futures_util::poll!(body.frame()).is_pending());
        assert_eq!(*lock(&seen), [trace_id]);
        assert_eq!(
            opentelemetry::Context::current()
                .span()
                .span_context()
                .trace_id(),
            ambient
        );
        drop(body);
    }
}
