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
use tonic::Code;

use crate::observe::Call;
use crate::status::internal_status;

use operation_context::{OperationContext, Stopped};
use tokio_util::sync::DropGuard;

/// Transfers one value through HTTP extensions, which require `Clone`:
/// cloning the holder never clones the value, which one owner takes.
pub(crate) struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T> Clone for Handoff<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

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
    pub(crate) cancel_on_drop: Option<DropGuard>,
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) upload: Option<UploadGuard>,
}

impl ResponseCustody {
    /// The custody the server layers hand over in the response extensions:
    /// the caller lifetime from `deadline`, its cancellation guard, and the
    /// business admission permit.
    pub(crate) fn from_server_response<B>(response: &mut http::Response<B>) -> Self {
        let extensions = response.extensions_mut();
        Self {
            context: extensions
                .remove::<crate::ResponseContext>()
                .map(|context| context.operation().clone()),
            cancel_on_drop: extensions
                .remove::<Handoff<DropGuard>>()
                .and_then(Handoff::take),
            permit: extensions
                .remove::<Handoff<OwnedSemaphorePermit>>()
                .and_then(Handoff::take),
            upload: None,
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

    /// The status of a call stopped for `reason`.
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

fn header_status_code(headers: &HeaderMap) -> Option<Code> {
    headers
        .get("grpc-status")
        .map(|value| Code::from_bytes(value.as_bytes()))
}

fn header_catalog_code(headers: &HeaderMap) -> Option<service_failure::Code> {
    // Tonic's details decoder can panic on invalid base64; feature headers
    // cannot poison the terminal owner or skip its cleanup.
    std::panic::catch_unwind(|| tonic::Status::from_header_map(headers))
        .ok()
        .flatten()
        .as_ref()
        .and_then(crate::status::catalog_code)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Resources<B> {
    body: B,
    call: Call,
    _permit: Option<OwnedSemaphorePermit>,
    _upload: Option<UploadGuard>,
    _cancel_on_drop: Option<DropGuard>,
}

struct Completion<B> {
    resources: Resources<B>,
    code: Code,
    failure: Option<service_failure::Code>,
}

impl<B> Completion<B> {
    fn finish(self) {
        if let Some(failure) = self.failure {
            self.resources.call.failed(failure);
        }
        self.resources.call.finish(self.code);
        // Remaining fields, including feature destructors, drop outside the lock.
    }
}

struct State<B: Body> {
    resources: Option<Resources<B>>,
    context: Option<OperationContext>,
    side: Side,
    fallback: Option<opentelemetry::Context>,
    terminal: Option<HeaderMap>,
    waker: Option<Waker>,
    error_code: fn(&B::Error) -> Code,
}

impl<B: Body> State<B> {
    fn stopped_status(&self) -> Option<tonic::Status> {
        let reason = self.context.as_ref()?.stopped()?;
        Some(self.side.stopped(reason))
    }

    fn take_completion(
        &mut self,
        code: Code,
        failure: Option<service_failure::Code>,
    ) -> Option<Completion<B>> {
        self.resources.take().map(|resources| Completion {
            resources,
            code,
            failure,
        })
    }

    /// Completes with a locally generated status, sent as the trailers.
    fn complete_with(&mut self, status: &tonic::Status) -> Option<Completion<B>> {
        let mut trailers = HeaderMap::new();
        // Locally generated sanitized statuses always encode.
        let _ = status.add_header(&mut trailers);
        let failure = matches!(self.side, Side::Server)
            .then(|| crate::status::catalog_code(status))
            .flatten();
        let completion = self.take_completion(status.code(), failure);
        if completion.is_some() {
            self.terminal = Some(trailers);
        }
        completion
    }

    /// Completes from a frame the body yielded; `None` while the call is open.
    fn complete_from(
        &mut self,
        polled: &Poll<Option<Result<Frame<Bytes>, B::Error>>>,
    ) -> Option<Completion<B>> {
        match polled {
            Poll::Ready(Some(Ok(frame))) => frame.trailers_ref().and_then(|trailers| {
                let code = header_status_code(trailers).unwrap_or(Code::Unknown);
                let failure = matches!(self.side, Side::Server)
                    .then(|| header_catalog_code(trailers))
                    .flatten();
                self.take_completion(code, failure)
            }),
            Poll::Ready(Some(Err(error))) => {
                let code = (self.error_code)(error);
                self.take_completion(code, None)
            }
            Poll::Ready(None) => self.take_completion(Code::Unknown, None),
            Poll::Pending => None,
        }
    }

    fn take_terminal(&mut self) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        Poll::Ready(
            self.terminal
                .take()
                .map(|trailers| Ok(Frame::trailers(trailers))),
        )
    }
}

/// Pass-through response custody. The stop watcher holds only a Weak reference.
struct ResponseBody<B: Body> {
    state: Arc<Mutex<State<B>>>,
    timer: Option<AbortOnDropHandle<()>>,
    #[cfg(test)]
    timer_done: Option<tokio::sync::oneshot::Receiver<()>>,
}

pub(crate) fn attach<B>(
    mut response: http::Response<B>,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    custody: ResponseCustody,
    error_code: fn(&B::Error) -> Code,
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
    if let Some(status) = stopped {
        for name in ["grpc-status", "grpc-message", "grpc-status-details-bin"] {
            response.headers_mut().remove(name);
        }
        let _ = status.add_header(response.headers_mut());
    }
    let initial_status = header_status_code(response.headers()).map(|code| {
        let failure = if matches!(side, Side::Server) {
            header_catalog_code(response.headers())
        } else {
            None
        };
        (code, failure)
    });
    response
        .map(|body| ResponseBody::new(body, call, fallback, custody, error_code, initial_status))
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
        error_code: fn(&B::Error) -> Code,
        initial_status: Option<(Code, Option<service_failure::Code>)>,
    ) -> Self {
        let ended = body.is_end_stream();
        let side = call.side();
        let mut state = State {
            resources: Some(Resources {
                body,
                call,
                _permit: custody.permit,
                _upload: custody.upload,
                _cancel_on_drop: custody.cancel_on_drop,
            }),
            context: custody.context,
            side,
            fallback,
            terminal: None,
            waker: None,
            error_code,
        };
        let completion = if let Some((code, failure)) = initial_status {
            state.take_completion(code, failure)
        } else if let Some(status) = state.stopped_status() {
            state.complete_with(&status)
        } else if ended {
            state.take_completion(Code::Unknown, None)
        } else {
            None
        };
        if let Some(completion) = completion {
            completion.finish();
        }
        let watched = state
            .resources
            .is_some()
            .then(|| state.context.clone())
            .flatten();
        let mut this = Self {
            state: Arc::new(Mutex::new(state)),
            timer: None,
            #[cfg(test)]
            timer_done: None,
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
            self.timer_done = Some(receiver);
        }
        self.timer = Some(AbortOnDropHandle::new(tokio::spawn(async move {
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

    fn reap_timer(&mut self, context: &mut Context<'_>) {
        if let Some(timer) = &mut self.timer
            && let Poll::Ready(result) = std::future::Future::poll(Pin::new(timer), context)
        {
            self.timer = None;
            if result.is_err() {
                terminate(&self.state, &internal_status());
            }
        }
    }
}

fn terminate<B: Body>(state: &Mutex<State<B>>, status: &tonic::Status) {
    let (completion, waker) = {
        let mut state = lock(state);
        let completion = state.complete_with(status);
        (completion, state.waker.take())
    };
    if let Some(waker) = waker {
        waker.wake();
    }
    if let Some(completion) = completion {
        completion.finish();
    }
}

impl<B> Body for ResponseBody<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        let this = self.get_mut();
        this.reap_timer(context);
        let mut state = lock(&this.state);
        if state.resources.is_none() {
            return state.take_terminal();
        }
        let (polled, completion, panic) = if let Some(status) = state.stopped_status() {
            let completion = state.complete_with(&status);
            (state.take_terminal(), completion, None)
        } else {
            state.waker = Some(context.waker().clone());
            let fallback = state.fallback.clone();
            let polled = {
                let Some(resources) = state.resources.as_mut() else {
                    return state.take_terminal();
                };
                let _span = resources.call.span().enter();
                let _fallback = fallback.map(opentelemetry::Context::attach);
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    Pin::new(&mut resources.body).poll_frame(context)
                }))
            };
            match polled {
                // A stop observed after the poll wins over whatever it yielded.
                Ok(polled) => {
                    if let Some(status) = state.stopped_status() {
                        let completion = state.complete_with(&status);
                        (state.take_terminal(), completion, None)
                    } else {
                        let completion = state.complete_from(&polled);
                        (polled, completion, None)
                    }
                }
                Err(panic) if matches!(state.side, Side::Client) => {
                    let completion = state.take_completion(Code::Cancelled, None);
                    (Poll::Ready(None), completion, Some(panic))
                }
                Err(_panic) => {
                    let completion = state.complete_with(&internal_status());
                    (state.take_terminal(), completion, None)
                }
            }
        };
        drop(state);
        if let Some(completion) = completion {
            // Dropping the handle aborts the watcher.
            this.timer = None;
            completion.finish();
        }
        if let Some(panic) = panic {
            std::panic::resume_unwind(panic);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        let state = lock(&self.state);
        state.resources.is_none() && state.terminal.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        let state = lock(&self.state);
        state.resources.as_ref().map_or_else(
            || SizeHint::with_exact(0),
            |resources| resources.body.size_hint(),
        )
    }
}

impl<B: Body> Drop for ResponseBody<B> {
    fn drop(&mut self) {
        let resources = lock(&self.state).resources.take();
        drop(self.timer.take());
        // Call's drop records cancellation only while still open.
        drop(resources);
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
        state.waker = Some(context.waker().clone());
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
            &Arc::new(crate::observe::Series::server(
                ["/test.Service/Stream".into()].into_iter().collect(),
            )),
            &request,
            Side::Server,
        )
    }

    fn response<B>(body: B, custody: ResponseCustody) -> ResponseBody<B>
    where
        B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
    {
        ResponseBody::new(body, call(), None, custody, tonic::Status::code, None)
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
                context: Some(OperationContext::new(None, child.clone())),
                cancel_on_drop: Some(child.drop_guard()),
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                ..ResponseCustody::default()
            },
        );
        let timer = body.timer.take().unwrap();
        parent.cancel();
        tokio::time::timeout(BUDGET, timer).await.unwrap().unwrap();
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
            Code::Cancelled
        );
        assert!(body.frame().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_releases_unpolled_resources_and_joins_its_timer() {
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
                context: Some(deadline_context()),
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                upload: Some(upload),
                ..ResponseCustody::default()
            },
        );
        let timer = body.timer.take().unwrap();
        tokio::time::advance(BUDGET).await;
        tokio::time::timeout(BUDGET, timer).await.unwrap().unwrap();
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
        assert_eq!(status.code(), Code::DeadlineExceeded);
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
    async fn terminal_status_releases_capacity_once_and_cancels_the_timer() {
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
                context: Some(deadline_context()),
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                ..ResponseCustody::default()
            },
        );
        let done = body.timer_done.take().unwrap();
        assert_eq!(
            header_status_code(body.frame().await.unwrap().unwrap().trailers_ref().unwrap()),
            Some(Code::Aborted)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(body.timer.is_none(), "completion releases the watcher");
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
    async fn response_drop_cancels_unpolled_upload_and_completes_timer_abort() {
        let cancellation = CancellationToken::new();
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let upload_source = ProbeBody::pending();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload) = upload(tonic::body::Body::new(upload_source));
        let mut body = response(
            source,
            ResponseCustody {
                context: Some(OperationContext::new(
                    Some(Deadline::new(Instant::now(), BUDGET)),
                    cancellation.clone(),
                )),
                cancel_on_drop: Some(cancellation.clone().drop_guard()),
                permit: None,
                upload: Some(upload),
            },
        );
        // Only tests retain this completion receiver. Its sender is owned by
        // the timer future, so closure observes destruction after abort.
        let done = body.timer_done.take().unwrap();
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
        assert_eq!(server.code(), Code::DeadlineExceeded);
        assert_eq!(
            crate::status::catalog_code(&server),
            Some(service_failure::Code::GatewayTimeout)
        );
        let client = Side::Client.stopped(Stopped::Deadline);
        assert_eq!(client.code(), Code::DeadlineExceeded);
        assert_eq!(crate::status::catalog_code(&client), None);
        for side in [Side::Server, Side::Client] {
            assert_eq!(side.stopped(Stopped::Cancelled).code(), Code::Cancelled);
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
