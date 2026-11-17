use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use futures_util::FutureExt as _;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use operation_context::{Deadline, Stopped};
use parking_lot::Mutex;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::DropGuard;
use tokio_util::task::AbortOnDropHandle;

use crate::observe::Call;
use crate::protocol::{GRPC_MESSAGE, GRPC_STATUS, GRPC_STATUS_DETAILS, header_status_code};
use crate::status::internal_status;

#[derive(Default)]
pub(crate) struct ServerCustody {
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) cancel_on_drop: Option<DropGuard>,
    pub(crate) deadline: Option<Deadline>,
}

pub(crate) fn deadline_status() -> tonic::Status {
    crate::Failure::new(service_failure::Code::GatewayTimeout).into()
}

pub(crate) fn server_stopped(reason: Stopped) -> tonic::Status {
    match reason {
        Stopped::Deadline => deadline_status(),
        Stopped::Cancelled => tonic::Status::cancelled("request cancelled"),
    }
}

fn header_catalog_code(headers: &HeaderMap) -> Option<service_failure::Code> {
    headers.get(GRPC_STATUS_DETAILS)?;
    tonic::Status::from_header_map(headers)
        .as_ref()
        .and_then(crate::status::catalog_code)
}

#[derive(Clone, Copy)]
struct ServerOutcome {
    code: tonic::Code,
    failure: Option<service_failure::Code>,
}

impl ServerOutcome {
    const fn unknown() -> Self {
        Self {
            code: tonic::Code::Unknown,
            failure: None,
        }
    }

    fn from_status(status: &tonic::Status) -> Self {
        Self {
            code: status.code(),
            failure: crate::status::catalog_code(status),
        }
    }

    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        Some(Self {
            code: header_status_code(headers)?,
            failure: header_catalog_code(headers),
        })
    }

    fn from_response<B>(response: &http::Response<B>) -> Option<Self> {
        response
            .extensions()
            .get::<tonic::Status>()
            .map(Self::from_status)
            .or_else(|| Self::from_headers(response.headers()))
    }
}

type FramePoll<E> = Poll<Option<Result<Frame<Bytes>, E>>>;

struct ActiveStream<B: Body> {
    body: B,
    call: Call,
    _permit: Option<OwnedSemaphorePermit>,
    _cancel_on_drop: Option<DropGuard>,
    deadline: Option<Deadline>,
    fallback: Option<opentelemetry::Context>,
    waker: Option<Waker>,
    map_error: fn(&B::Error) -> tonic::Code,
}

enum StreamStage<B: Body> {
    Active(ActiveStream<B>),
    PendingTrailers(HeaderMap),
    Closed,
}

struct StreamFinalizer<B: Body> {
    active: ActiveStream<B>,
    outcome: ServerOutcome,
}

impl<B: Body> StreamFinalizer<B> {
    fn emit(self) {
        if let Some(failure) = self.outcome.failure {
            self.active.call.record_failure(failure);
        }
        self.active.call.finish(self.outcome.code);
    }
}

struct ServerState<B: Body> {
    stage: StreamStage<B>,
}

impl<B: Body> ServerState<B> {
    fn is_expired(&self) -> bool {
        let StreamStage::Active(active) = &self.stage else {
            return false;
        };
        active.deadline.is_some_and(Deadline::expired)
    }

    fn transition(
        &mut self,
        next: StreamStage<B>,
        outcome: ServerOutcome,
    ) -> Option<StreamFinalizer<B>> {
        match std::mem::replace(&mut self.stage, next) {
            StreamStage::Active(active) => Some(StreamFinalizer { active, outcome }),
            other => {
                self.stage = other;
                None
            }
        }
    }

    fn stage_status_trailers(&mut self, status: &tonic::Status) -> Option<StreamFinalizer<B>> {
        let mut trailers = HeaderMap::new();
        let _ = status.add_header(&mut trailers);
        self.transition(
            StreamStage::PendingTrailers(trailers),
            ServerOutcome::from_status(status),
        )
    }

    fn cut_off(
        &mut self,
        status: &tonic::Status,
    ) -> (FramePoll<B::Error>, Option<StreamFinalizer<B>>) {
        let finalizer = self.stage_status_trailers(status);
        (self.pop_terminal(), finalizer)
    }

    fn inspect_frame(&mut self, polled: &FramePoll<B::Error>) -> Option<StreamFinalizer<B>> {
        let StreamStage::Active(active) = &self.stage else {
            return None;
        };
        let outcome = match polled {
            Poll::Ready(Some(Ok(frame))) => {
                let trailers = frame.trailers_ref()?;
                ServerOutcome::from_headers(trailers).unwrap_or_else(ServerOutcome::unknown)
            }
            Poll::Ready(Some(Err(error))) => ServerOutcome {
                code: (active.map_error)(error),
                failure: None,
            },
            Poll::Ready(None) => ServerOutcome::unknown(),
            Poll::Pending => return None,
        };
        self.transition(StreamStage::Closed, outcome)
    }

    fn pop_terminal(&mut self) -> FramePoll<B::Error> {
        match std::mem::replace(&mut self.stage, StreamStage::Closed) {
            StreamStage::PendingTrailers(trailers) => {
                Poll::Ready(Some(Ok(Frame::trailers(trailers))))
            }
            stage => {
                self.stage = stage;
                Poll::Ready(None)
            }
        }
    }
}

struct ResponseBody<B: Body> {
    state: Arc<Mutex<ServerState<B>>>,
    timer: Option<AbortOnDropHandle<()>>,
    #[cfg(test)]
    timer_done: Option<tokio::sync::oneshot::Receiver<()>>,
}

pub(crate) fn attach_server<B>(
    mut response: http::Response<B>,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    custody: ServerCustody,
) -> http::Response<impl Body<Data = Bytes, Error = B::Error> + Send + 'static>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    let initial = if custody.deadline.is_some_and(Deadline::expired) {
        let status = deadline_status();
        for header in [GRPC_STATUS, GRPC_MESSAGE, GRPC_STATUS_DETAILS] {
            response.headers_mut().remove(header);
        }
        let _ = status.add_header(response.headers_mut());
        Some(ServerOutcome::from_status(&status))
    } else {
        ServerOutcome::from_response(&response)
    };
    response.map(|body| {
        ResponseBody::start(
            body,
            call,
            fallback,
            custody,
            |_error| tonic::Code::Unknown,
            initial,
        )
    })
}

impl<B> ResponseBody<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    fn start(
        body: B,
        call: Call,
        fallback: Option<opentelemetry::Context>,
        custody: ServerCustody,
        map_error: fn(&B::Error) -> tonic::Code,
        initial: Option<ServerOutcome>,
    ) -> Self {
        let body_ended = body.is_end_stream();
        let mut state = ServerState {
            stage: StreamStage::Active(ActiveStream {
                body,
                call,
                _permit: custody.permit,
                _cancel_on_drop: custody.cancel_on_drop,
                deadline: custody.deadline,
                fallback,
                waker: None,
                map_error,
            }),
        };
        let finalizer = if let Some(outcome) = initial {
            state.transition(StreamStage::Closed, outcome)
        } else if state.is_expired() {
            state.stage_status_trailers(&deadline_status())
        } else if body_ended {
            state.transition(StreamStage::Closed, ServerOutcome::unknown())
        } else {
            None
        };
        if let Some(finalizer) = finalizer {
            finalizer.emit();
        }
        let armed_deadline = match &state.stage {
            StreamStage::Active(active) => active.deadline,
            StreamStage::PendingTrailers(_) | StreamStage::Closed => None,
        };
        let mut this = Self {
            state: Arc::new(Mutex::new(state)),
            timer: None,
            #[cfg(test)]
            timer_done: None,
        };
        if let Some(deadline) = armed_deadline {
            this.arm_timer(deadline);
        }
        this
    }

    fn arm_timer(&mut self, deadline: Deadline) {
        let weak = Arc::downgrade(&self.state);
        #[cfg(test)]
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        #[cfg(test)]
        {
            self.timer_done = Some(done_rx);
        }
        self.timer = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            #[cfg(test)]
            let _done_tx = done_tx;
            let result = AssertUnwindSafe(async {
                deadline.wait().await;
                if let Some(state) = weak.upgrade() {
                    terminate_from_timer(&state, &deadline_status());
                }
            })
            .catch_unwind()
            .await;
            if result.is_err()
                && let Some(state) = weak.upgrade()
            {
                terminate_from_timer(&state, &internal_status());
            }
        })));
    }

    fn poll_timer(&mut self, cx: &mut Context<'_>) {
        if let Some(timer) = &mut self.timer
            && let Poll::Ready(joined) = std::future::Future::poll(Pin::new(timer), cx)
        {
            self.timer = None;
            if joined.is_err() {
                terminate_from_timer(&self.state, &internal_status());
            }
        }
    }
}

fn terminate_from_timer<B: Body>(state: &Mutex<ServerState<B>>, status: &tonic::Status) {
    let finalizer = state.lock().stage_status_trailers(status);
    if let Some(mut finalizer) = finalizer {
        if let Some(waker) = finalizer.active.waker.take() {
            waker.wake();
        }
        finalizer.emit();
    }
}

fn register_waker(slot: &mut Option<Waker>, waker: &Waker) {
    match slot {
        Some(existing) => existing.clone_from(waker),
        None => *slot = Some(waker.clone()),
    }
}

impl<B> Body for ResponseBody<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> FramePoll<B::Error> {
        let this = self.get_mut();
        this.poll_timer(cx);
        let mut state = this.state.lock();
        let (polled, finalizer) = if state.is_expired() {
            state.cut_off(&deadline_status())
        } else {
            let StreamStage::Active(active) = &mut state.stage else {
                return state.pop_terminal();
            };
            register_waker(&mut active.waker, cx.waker());
            let inner = {
                let _entered = active.call.span().enter();
                let _attached = active.fallback.clone().map(opentelemetry::Context::attach);
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    Pin::new(&mut active.body).poll_frame(cx)
                }))
            };
            match inner {
                Ok(polled) => {
                    if state.is_expired() {
                        state.cut_off(&deadline_status())
                    } else {
                        let finalizer = state.inspect_frame(&polled);
                        (polled, finalizer)
                    }
                }
                Err(_panic) => state.cut_off(&internal_status()),
            }
        };
        drop(state);
        if let Some(finalizer) = finalizer {
            this.timer = None;
            finalizer.emit();
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        let guard = self.state.lock();
        matches!(guard.stage, StreamStage::Closed)
    }

    fn size_hint(&self) -> SizeHint {
        let guard = self.state.lock();
        match &guard.stage {
            StreamStage::Active(active) => active.body.size_hint(),
            StreamStage::PendingTrailers(_) | StreamStage::Closed => SizeHint::with_exact(0),
        }
    }
}

impl<B: Body> Drop for ResponseBody<B> {
    fn drop(&mut self) {
        let stage = {
            let mut guard = self.state.lock();
            std::mem::replace(&mut guard.stage, StreamStage::Closed)
        };
        drop(self.timer.take());
        drop(stage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    const BUDGET: Duration = Duration::from_secs(1);

    fn budget_deadline() -> Deadline {
        Deadline::new(Instant::now(), BUDGET)
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

    fn server_call() -> Call {
        let request = http::Request::builder()
            .uri("/test.Service/Stream")
            .body(())
            .unwrap();
        let methods = crate::observe::Methods::new(
            std::iter::once((
                "/test.Service/Stream".into(),
                crate::observe::CallType::BidiStream,
            ))
            .collect(),
        );
        Call::start(&methods, &request)
    }

    fn start_response<B>(body: B, custody: ServerCustody) -> ResponseBody<B>
    where
        B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
    {
        ResponseBody::start(
            body,
            server_call(),
            None,
            custody,
            tonic::Status::code,
            None,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn watched_deadline_releases_unpolled_resources_and_joins_its_timer() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let token = CancellationToken::new();
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let mut body = start_response(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: Some(token.clone().drop_guard()),
                deadline: Some(budget_deadline()),
            },
        );
        let timer = body.timer.take().unwrap();
        tokio::time::advance(BUDGET).await;
        tokio::time::timeout(BUDGET, timer).await.unwrap().unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert!(token.is_cancelled());
        assert_eq!(permits.available_permits(), 1);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert!(!body.is_end_stream());
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
    async fn terminal_status_releases_capacity_once_and_aborts_the_timer() {
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
        let mut body = start_response(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: None,
                deadline: Some(budget_deadline()),
            },
        );
        let done = body.timer_done.take().unwrap();
        assert_eq!(
            header_status_code(body.frame().await.unwrap().unwrap().trailers_ref().unwrap()),
            Some(tonic::Code::Aborted)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(body.timer.is_none());
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
    async fn response_drop_cancels_propagated_work_and_aborts_timer() {
        let cancellation = CancellationToken::new();
        let permits = Arc::new(Semaphore::new(1));
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let mut body = start_response(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: Some(cancellation.clone().drop_guard()),
                deadline: Some(budget_deadline()),
            },
        );
        let done = body.timer_done.take().unwrap();
        drop(body);
        assert!(cancellation.is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(tokio::time::timeout(BUDGET, done).await.unwrap().is_err());
    }

    struct PanicBody(Arc<AtomicBool>);

    const PANIC_MESSAGE: &str = "source panic";

    impl Drop for PanicBody {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl Body for PanicBody {
        type Data = Bytes;
        type Error = tonic::Status;

        fn poll_frame(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
            std::panic::resume_unwind(Box::new(PANIC_MESSAGE))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn server_body_panic_answers_internal_and_releases_capacity() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let dropped = Arc::new(AtomicBool::new(false));
        let mut body = start_response(
            PanicBody(Arc::clone(&dropped)),
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: None,
                deadline: Some(budget_deadline()),
            },
        );
        let done = body.timer_done.take().unwrap();
        let trailers = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        let status = tonic::Status::from_header_map(&trailers).unwrap();
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), service_failure::SANITIZED_DETAIL);
        assert_eq!(
            crate::status::catalog_code(&status),
            Some(service_failure::Code::InternalServerError)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(tokio::time::timeout(BUDGET, done).await.unwrap().is_err());
        assert!(body.frame().await.is_none());
        drop(body);
        let rendered = handle.render();
        assert!(rendered.contains("grpc_code=\"Internal\"} 1"), "{rendered}");
        assert!(!rendered.contains("grpc_code=\"Canceled\""), "{rendered}");
    }

    #[test]
    fn lost_timer_runtime_ends_the_call_internal() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let source = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut body = {
            let _entered = runtime.enter();
            start_response(
                source,
                ServerCustody {
                    permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                    cancel_on_drop: None,
                    deadline: Some(budget_deadline()),
                },
            )
        };
        drop(runtime);
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut cx) else {
            unreachable!("lost timer runtime produces terminal internal trailers");
        };
        let status = tonic::Status::from_header_map(&frame.into_trailers().unwrap()).unwrap();
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(
            crate::status::catalog_code(&status),
            Some(service_failure::Code::InternalServerError)
        );
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(matches!(
            Pin::new(&mut body).poll_frame(&mut cx),
            Poll::Ready(None)
        ));
        drop(body);
        let rendered = handle.render();
        assert!(rendered.contains("grpc_code=\"Internal\"} 1"), "{rendered}");
    }

    #[test]
    fn server_stopped_maps_deadline_and_cancellation() {
        let deadline = server_stopped(Stopped::Deadline);
        assert_eq!(deadline.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(
            crate::status::catalog_code(&deadline),
            Some(service_failure::Code::GatewayTimeout)
        );
        let cancelled = server_stopped(Stopped::Cancelled);
        assert_eq!(cancelled.code(), tonic::Code::Cancelled);
        assert_eq!(crate::status::catalog_code(&cancelled), None);
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
                self.0.lock().push(
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
        let call = tracing::subscriber::with_default(
            tracing::subscriber::NoSubscriber::default(),
            server_call,
        );
        assert!(call.span().is_disabled());
        let mut body = ResponseBody::start(
            ContextBody(Arc::clone(&seen)),
            call,
            Some(fallback),
            ServerCustody::default(),
            tonic::Status::code,
            None,
        );
        let ambient = opentelemetry::Context::current()
            .span()
            .span_context()
            .trace_id();
        assert!(futures_util::poll!(body.frame()).is_pending());
        assert_eq!(*seen.lock(), [trace_id]);
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
