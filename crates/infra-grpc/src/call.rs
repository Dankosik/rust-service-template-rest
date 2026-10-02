//! One terminal owner for native response work, capacity and upload cancellation.

use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use tokio::sync::OwnedSemaphorePermit;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tonic::Code;

use crate::observe::Call;

/// Origin plus duration avoids overflow even for the largest legal wire budget.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadline {
    origin: Instant,
    budget: Duration,
}

impl Deadline {
    pub(crate) const fn new(origin: Instant, budget: Duration) -> Self {
        Self { origin, budget }
    }

    pub(crate) fn remaining(self) -> Duration {
        self.budget.saturating_sub(self.origin.elapsed())
    }

    pub(crate) fn expired(self) -> bool {
        self.remaining().is_zero()
    }

    pub(crate) async fn wait(self) {
        // Large legal grpc-timeout values need not fit an Instant addition.
        // Recheck elapsed time after each bounded sleep, including early wakes.
        while !self.expired() {
            tokio::time::sleep(self.remaining().min(Duration::from_secs(86_400))).await;
        }
    }
}

/// HTTP extensions require Clone; cloning this holder never clones its permit.
#[derive(Clone)]
pub(crate) struct Permit(Arc<Mutex<Option<OwnedSemaphorePermit>>>);

impl Permit {
    pub(crate) fn new(permit: OwnedSemaphorePermit) -> Self {
        Self(Arc::new(Mutex::new(Some(permit))))
    }

    pub(crate) fn take(self) -> Option<OwnedSemaphorePermit> {
        lock(&self.0).take()
    }
}

#[derive(Default)]
pub(crate) struct Lifetime {
    pub(crate) deadline: Option<Deadline>,
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) upload: Option<UploadGuard>,
}

#[derive(Clone, Copy)]
pub(crate) enum Side {
    Server,
    Client,
}

impl Side {
    fn deadline(self) -> tonic::Status {
        match self {
            Self::Server => crate::Failure::new(service_failure::Code::GatewayTimeout).into(),
            Self::Client => tonic::Status::deadline_exceeded("request deadline exceeded"),
        }
    }
}

fn code(headers: &HeaderMap) -> Option<Code> {
    headers
        .get("grpc-status")
        .map(|value| Code::from_bytes(value.as_bytes()))
}

fn failure(headers: &HeaderMap) -> Option<service_failure::Code> {
    // Tonic's details decoder can panic on invalid base64; feature headers
    // cannot poison the terminal owner or skip its cleanup.
    std::panic::catch_unwind(|| tonic::Status::from_header_map(headers))
        .ok()
        .flatten()
        .as_ref()
        .and_then(crate::status::catalog_code)
}

fn internal() -> tonic::Status {
    crate::Failure::new(service_failure::Code::InternalServerError).into()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Resources<B> {
    body: B,
    call: Call,
    _permit: Option<OwnedSemaphorePermit>,
    _upload: Option<UploadGuard>,
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
    deadline: Option<Deadline>,
    side: Side,
    fallback: Option<opentelemetry::Context>,
    terminal: Option<HeaderMap>,
    waker: Option<Waker>,
    error_code: fn(&B::Error) -> Code,
}

impl<B: Body> State<B> {
    fn finish(
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

    fn status(&mut self, status: &tonic::Status) -> Option<Completion<B>> {
        let mut trailers = HeaderMap::new();
        // Locally generated sanitized statuses always encode.
        let _ = status.add_header(&mut trailers);
        let failure = matches!(self.side, Side::Server)
            .then(|| crate::status::catalog_code(status))
            .flatten();
        let completion = self.finish(status.code(), failure);
        if completion.is_some() {
            self.terminal = Some(trailers);
        }
        completion
    }

    fn take_terminal(&mut self) -> Poll<Option<Result<Frame<Bytes>, B::Error>>> {
        Poll::Ready(
            self.terminal
                .take()
                .map(|trailers| Ok(Frame::trailers(trailers))),
        )
    }
}

/// Pass-through response custody. The timer holds only a Weak reference.
struct ResponseBody<B: Body> {
    state: Arc<Mutex<State<B>>>,
    timer: Option<JoinHandle<()>>,
    #[cfg(test)]
    timer_done: Option<tokio::sync::oneshot::Receiver<()>>,
}

pub(crate) fn attach<B>(
    mut response: http::Response<B>,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    lifetime: Lifetime,
    side: Side,
    error_code: fn(&B::Error) -> Code,
) -> http::Response<impl Body<Data = Bytes, Error = B::Error> + Send + 'static>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    // Check the original deadline at handoff; headers do not restart it.
    if lifetime.deadline.is_some_and(Deadline::expired) {
        for name in ["grpc-status", "grpc-message", "grpc-status-details-bin"] {
            response.headers_mut().remove(name);
        }
        let _ = side.deadline().add_header(response.headers_mut());
    }
    let initial_status = code(response.headers()).map(|code| {
        let failure = if matches!(side, Side::Server) {
            failure(response.headers())
        } else {
            None
        };
        (code, failure)
    });
    response.map(|body| {
        ResponseBody::new(
            body,
            call,
            fallback,
            lifetime,
            side,
            error_code,
            initial_status,
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
        lifetime: Lifetime,
        side: Side,
        error_code: fn(&B::Error) -> Code,
        initial_status: Option<(Code, Option<service_failure::Code>)>,
    ) -> Self {
        let ended = body.is_end_stream();
        let mut state = State {
            resources: Some(Resources {
                body,
                call,
                _permit: lifetime.permit,
                _upload: lifetime.upload,
            }),
            deadline: lifetime.deadline,
            side,
            fallback,
            terminal: None,
            waker: None,
            error_code,
        };
        if let Some((code, failure)) = initial_status {
            if let Some(completion) = state.finish(code, failure) {
                completion.finish();
            }
        } else if lifetime.deadline.is_some_and(Deadline::expired) {
            if let Some(completion) = state.status(&side.deadline()) {
                completion.finish();
            }
        } else if ended && let Some(completion) = state.finish(Code::Unknown, None) {
            completion.finish();
        }
        let active = state.resources.is_some();
        let state = Arc::new(Mutex::new(state));
        let mut this = Self {
            state,
            timer: None,
            #[cfg(test)]
            timer_done: None,
        };
        if active && let Some(deadline) = lifetime.deadline {
            let weak = Arc::downgrade(&this.state);
            #[cfg(test)]
            let (done, receiver) = tokio::sync::oneshot::channel();
            #[cfg(test)]
            {
                this.timer_done = Some(receiver);
            }
            this.timer = Some(tokio::spawn(async move {
                // Captured before first poll: abortion also drops the sender.
                #[cfg(test)]
                let _done = done;
                let result = AssertUnwindSafe(async {
                    deadline.wait().await;
                    if let Some(state) = weak.upgrade() {
                        terminate(&state, side.deadline());
                    }
                })
                .catch_unwind()
                .await;
                if result.is_err()
                    && let Some(state) = weak.upgrade()
                {
                    terminate(&state, internal());
                }
            }));
        }
        this
    }

    fn reap_timer(&mut self, context: &mut Context<'_>) {
        if let Some(timer) = &mut self.timer
            && let Poll::Ready(result) = std::future::Future::poll(Pin::new(timer), context)
        {
            self.timer = None;
            if result.is_err() {
                terminate(&self.state, internal());
            }
        }
    }
}

fn terminate<B: Body>(state: &Mutex<State<B>>, status: tonic::Status) {
    let (completion, waker) = {
        let mut state = lock(state);
        let completion = state.status(&status);
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
        let (polled, completion, panic) = if state.deadline.is_some_and(Deadline::expired) {
            let status = state.side.deadline();
            let completion = state.status(&status);
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
                Ok(_polled) if state.deadline.is_some_and(Deadline::expired) => {
                    let status = state.side.deadline();
                    let completion = state.status(&status);
                    (state.take_terminal(), completion, None)
                }
                Ok(polled) => {
                    let completion = match &polled {
                        Poll::Ready(Some(Ok(frame))) => frame.trailers_ref().and_then(|trailers| {
                            let code = code(trailers).unwrap_or(Code::Unknown);
                            let failure = matches!(state.side, Side::Server)
                                .then(|| failure(trailers))
                                .flatten();
                            state.finish(code, failure)
                        }),
                        Poll::Ready(Some(Err(error))) => {
                            let code = (state.error_code)(error);
                            state.finish(code, None)
                        }
                        Poll::Ready(None) => state.finish(Code::Unknown, None),
                        Poll::Pending => None,
                    };
                    (polled, completion, None)
                }
                Err(panic) if matches!(state.side, Side::Client) => {
                    let completion = state.finish(Code::Cancelled, None);
                    (Poll::Ready(None), completion, Some(panic))
                }
                Err(_panic) => {
                    let completion = state.status(&internal());
                    (state.take_terminal(), completion, None)
                }
            }
        };
        drop(state);
        if let Some(completion) = completion {
            if let Some(timer) = &this.timer {
                timer.abort();
            }
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
        if let Some(timer) = &self.timer {
            timer.abort();
        }
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "owner-local lifetime fixtures fail with their setup context"
)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, SpanKind, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::sync::Semaphore;

    const BUDGET: Duration = Duration::from_secs(1);

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
            SpanKind::Server,
        )
    }

    fn response<B>(body: B, lifetime: Lifetime) -> ResponseBody<B>
    where
        B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
    {
        ResponseBody::new(
            body,
            call(),
            None,
            lifetime,
            Side::Server,
            tonic::Status::code,
            None,
        )
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
            Lifetime {
                deadline: Some(Deadline::new(Instant::now(), BUDGET)),
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                upload: Some(upload),
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
            Lifetime {
                deadline: Some(Deadline::new(Instant::now(), BUDGET)),
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                upload: None,
            },
        );
        assert_eq!(
            code(body.frame().await.unwrap().unwrap().trailers_ref().unwrap()),
            Some(Code::Aborted)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(
            tokio::time::timeout(BUDGET, body.timer.take().unwrap())
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
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
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let upload_source = ProbeBody::pending();
        let upload_dropped = Arc::clone(&upload_source.dropped);
        let (mut outgoing, upload) = upload(tonic::body::Body::new(upload_source));
        let mut body = response(
            source,
            Lifetime {
                deadline: Some(Deadline::new(Instant::now(), BUDGET)),
                permit: None,
                upload: Some(upload),
            },
        );
        // Only tests retain this completion receiver. Its sender is owned by
        // the timer future, so closure observes destruction after abort.
        let done = body.timer_done.take().unwrap();
        drop(body);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(upload_dropped.load(Ordering::SeqCst));
        assert!(outgoing.frame().await.is_none());
        assert!(tokio::time::timeout(BUDGET, done).await.unwrap().is_err());
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
            Lifetime::default(),
            Side::Server,
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
