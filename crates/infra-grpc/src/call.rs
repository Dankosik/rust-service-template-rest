use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use http::HeaderMap;
use http_body::{Body, Frame, SizeHint};
use infra_grpc_core::watched::{
    FramePoll, MaybeWatched, TerminableBody, WatchedPoll, register_waker,
};
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::observe::Call;
use crate::protocol::{STATUS_HEADERS, header_status_code};
use crate::status::FixedFailure;

#[derive(Default)]
pub(crate) struct ServerCustody {
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) cancel_on_drop: Option<DropGuard>,
    pub(crate) deadline: Option<Deadline>,
    pub(crate) end_on_drain: Option<CancellationToken>,
}

pub(crate) fn stopped_status(reason: Stopped) -> tonic::Status {
    match reason {
        Stopped::Deadline => FixedFailure::Deadline.status(),
        Stopped::Cancelled => tonic::Status::cancelled("request cancelled"),
    }
}

#[expect(
    clippy::result_large_err,
    reason = "the error is the refusal response the call returns"
)]
pub(crate) fn check_open(context: &OperationContext) -> Result<(), axum::response::Response> {
    context
        .check()
        .map_err(|reason| stopped_status(reason).into_http())
}

const MIN_HEADERS_FOR_STATUS_PROJECTION: usize = 32;

fn header_catalog_code(headers: &HeaderMap) -> Option<service_failure::Code> {
    headers.get(tonic::Status::GRPC_STATUS_DETAILS)?;
    let status = if headers.len() < MIN_HEADERS_FOR_STATUS_PROJECTION {
        tonic::Status::from_header_map(headers)
    } else {
        let mut fields = HeaderMap::with_capacity(STATUS_HEADERS.len());
        for name in STATUS_HEADERS {
            if let Some(value) = headers.get(&name) {
                fields.insert(name, value.clone());
            }
        }
        tonic::Status::from_header_map(&fields)
    }?;
    crate::status::catalog_code(&status)
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

struct LocalStatus {
    status: tonic::Status,
    failure: Option<service_failure::Code>,
}

impl LocalStatus {
    fn failure(failure: FixedFailure) -> Self {
        Self {
            status: failure.status(),
            failure: Some(failure.catalog_code()),
        }
    }

    fn drained() -> Self {
        Self {
            status: tonic::Status::ok(""),
            failure: None,
        }
    }

    fn write_headers(self, headers: &mut HeaderMap) -> ServerOutcome {
        let _ = self.status.add_header(headers);
        ServerOutcome {
            code: self.status.code(),
            failure: self.failure,
        }
    }
}

struct ActiveStream<B> {
    body: B,
    call: Call,
    _permit: Option<OwnedSemaphorePermit>,
    _cancel_on_drop: Option<DropGuard>,
    deadline: Option<Deadline>,
    fallback: Option<opentelemetry::Context>,
    waker: Option<Waker>,
}

enum ServerState<B> {
    Active(ActiveStream<B>),
    PendingTrailers(HeaderMap),
    Closed,
}

struct StreamFinalizer<B> {
    active: ActiveStream<B>,
    outcome: ServerOutcome,
}

impl<B> StreamFinalizer<B> {
    fn emit(self) {
        if let Some(failure) = self.outcome.failure {
            self.active.call.record_failure(failure);
        }
        self.active.call.finish(self.outcome.code);
    }
}

impl<B> ServerState<B> {
    fn is_expired(&self) -> bool {
        let Self::Active(active) = self else {
            return false;
        };
        active.deadline.is_some_and(Deadline::expired)
    }

    fn transition(&mut self, next: Self, outcome: ServerOutcome) -> Option<StreamFinalizer<B>> {
        match std::mem::replace(self, next) {
            Self::Active(active) => Some(StreamFinalizer { active, outcome }),
            other => {
                *self = other;
                None
            }
        }
    }

    fn stage_status_trailers(&mut self, status: LocalStatus) -> Option<StreamFinalizer<B>> {
        if !matches!(self, Self::Active(_)) {
            return None;
        }
        let mut trailers = HeaderMap::new();
        let outcome = status.write_headers(&mut trailers);
        self.transition(Self::PendingTrailers(trailers), outcome)
    }
}

impl<B: Body> ServerState<B> {
    fn cut_off(
        &mut self,
        status: LocalStatus,
    ) -> (FramePoll<B::Error>, Option<StreamFinalizer<B>>) {
        let finalizer = self.stage_status_trailers(status);
        (self.pop_terminal(), finalizer)
    }

    fn close_if_terminal(&mut self, polled: &FramePoll<B::Error>) -> Option<StreamFinalizer<B>> {
        let outcome = match polled {
            Poll::Ready(Some(Ok(frame))) => {
                let trailers = frame.trailers_ref()?;
                ServerOutcome::from_headers(trailers).unwrap_or_else(ServerOutcome::unknown)
            }
            Poll::Ready(Some(Err(_)) | None) => ServerOutcome::unknown(),
            Poll::Pending => return None,
        };
        self.transition(Self::Closed, outcome)
    }

    fn pop_terminal(&mut self) -> FramePoll<B::Error> {
        match std::mem::replace(self, Self::Closed) {
            Self::PendingTrailers(trailers) => Poll::Ready(Some(Ok(Frame::trailers(trailers)))),
            stage => {
                *self = stage;
                Poll::Ready(None)
            }
        }
    }
}

impl<B> ServerState<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
        watched: bool,
    ) -> (FramePoll<B::Error>, Option<StreamFinalizer<B>>) {
        let Self::Active(active) = self else {
            return (self.pop_terminal(), None);
        };
        if active.deadline.is_some_and(Deadline::expired) {
            return self.cut_off(LocalStatus::failure(FixedFailure::Deadline));
        }
        if watched {
            register_waker(&mut active.waker, cx.waker());
        }
        let inner = {
            let _entered = active.call.span().enter();
            let _attached = active.fallback.clone().map(opentelemetry::Context::attach);
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                Pin::new(&mut active.body).poll_frame(cx)
            }))
        };
        match inner {
            Ok(_) if self.is_expired() => {
                self.cut_off(LocalStatus::failure(FixedFailure::Deadline))
            }
            Ok(polled) => {
                let finalizer = self.close_if_terminal(&polled);
                (polled, finalizer)
            }
            Err(_panic) => self.cut_off(LocalStatus::failure(FixedFailure::Internal)),
        }
    }
}

impl<B> TerminableBody for ServerState<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    type Error = B::Error;
    type Stop = LocalStatus;
    type Finalizer = StreamFinalizer<B>;

    fn poll_watched(
        &mut self,
        cx: &mut Context<'_>,
        watched: bool,
    ) -> (WatchedPoll<B::Error>, Option<StreamFinalizer<B>>) {
        let (polled, finalizer) = self.poll_next(cx, watched);
        (Ok(polled), finalizer)
    }

    fn terminate(&mut self, stop: LocalStatus) -> Option<(Option<Waker>, StreamFinalizer<B>)> {
        let mut finalizer = self.stage_status_trailers(stop)?;
        let waker = finalizer.active.waker.take();
        Some((waker, finalizer))
    }

    fn take_for_drop(&mut self) -> Self {
        std::mem::replace(self, Self::Closed)
    }

    fn is_end_stream(&self) -> bool {
        matches!(self, Self::Closed)
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Active(active) => active.body.size_hint(),
            Self::PendingTrailers(_) | Self::Closed => SizeHint::with_exact(0),
        }
    }

    fn watcher_lost() -> LocalStatus {
        LocalStatus::failure(FixedFailure::Internal)
    }

    fn finish(finalizer: StreamFinalizer<B>) {
        finalizer.emit();
    }
}

type ResponseBody<B> = MaybeWatched<ServerState<B>>;

fn start_response_body<B>(
    body: B,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    custody: ServerCustody,
    initial: Option<ServerOutcome>,
) -> ResponseBody<B>
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
{
    let body_ended = body.is_end_stream();
    let mut state = ServerState::Active(ActiveStream {
        body,
        call,
        _permit: custody.permit,
        _cancel_on_drop: custody.cancel_on_drop,
        deadline: custody.deadline,
        fallback,
        waker: None,
    });
    let finalizer = if let Some(outcome) = initial {
        state.transition(ServerState::Closed, outcome)
    } else if state.is_expired() {
        state.stage_status_trailers(LocalStatus::failure(FixedFailure::Deadline))
    } else if body_ended {
        state.transition(ServerState::Closed, ServerOutcome::unknown())
    } else {
        None
    };
    if let Some(finalizer) = finalizer {
        finalizer.emit();
        return MaybeWatched::local(state);
    }
    if let Some(drain) = custody.end_on_drain {
        let context = OperationContext::new(custody.deadline, drain);
        MaybeWatched::watched(state, async move {
            match context.wait_stopped().await {
                Stopped::Deadline => LocalStatus::failure(FixedFailure::Deadline),
                Stopped::Cancelled => LocalStatus::drained(),
            }
        })
    } else if let Some(deadline) = custody.deadline {
        MaybeWatched::watched(state, async move {
            deadline.wait().await;
            LocalStatus::failure(FixedFailure::Deadline)
        })
    } else {
        MaybeWatched::local(state)
    }
}

fn into_axum_body<B>(body: ResponseBody<B>) -> axum::body::Body
where
    B: Body<Data = Bytes> + Unpin + Send + 'static,
    B::Error: Into<axum::BoxError>,
{
    match body {
        MaybeWatched::Local(ServerState::Closed) => axum::body::Body::empty(),
        other => axum::body::Body::new(other),
    }
}

pub(crate) fn wrap_response(
    mut response: axum::response::Response,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    custody: ServerCustody,
) -> axum::response::Response {
    let initial = if custody.deadline.is_some_and(Deadline::expired) {
        let status = LocalStatus::failure(FixedFailure::Deadline);
        for header in STATUS_HEADERS {
            response.headers_mut().remove(header);
        }
        Some(status.write_headers(response.headers_mut()))
    } else {
        ServerOutcome::from_response(&response)
    };
    response.map(|body| into_axum_body(start_response_body(body, call, fallback, custody, initial)))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use http_body_util::BodyExt as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };
    use parking_lot::Mutex;
    use tokio::sync::{Semaphore, oneshot};

    use super::*;

    const BUDGET: Duration = Duration::from_secs(1);

    #[test]
    fn rich_trailers_use_the_first_status_values_even_with_repeated_metadata() {
        let mut trailers = HeaderMap::new();
        FixedFailure::Deadline
            .status()
            .add_header(&mut trailers)
            .unwrap();
        trailers.append(
            tonic::Status::GRPC_STATUS,
            http::HeaderValue::from_static("0"),
        );
        trailers.append(
            tonic::Status::GRPC_STATUS_DETAILS,
            http::HeaderValue::from_static("!"),
        );
        for _ in 0..64 {
            trailers.append("custom", http::HeaderValue::from_static("metadata"));
        }

        let outcome = ServerOutcome::from_headers(&trailers).unwrap();
        assert_eq!(outcome.code, tonic::Code::DeadlineExceeded);
        assert_eq!(outcome.failure, Some(service_failure::Code::GatewayTimeout));
    }

    fn budget_deadline() -> Deadline {
        Deadline::after(BUDGET)
    }

    struct ProbeBody {
        frames: VecDeque<Frame<Bytes>>,
        polls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
        on_drop: Option<oneshot::Sender<()>>,
    }

    impl ProbeBody {
        fn pending() -> (Self, oneshot::Receiver<()>) {
            let (tx, rx) = oneshot::channel();
            (
                Self {
                    frames: VecDeque::new(),
                    polls: Arc::default(),
                    dropped: Arc::default(),
                    on_drop: Some(tx),
                },
                rx,
            )
        }
    }

    impl Drop for ProbeBody {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
            if let Some(tx) = self.on_drop.take() {
                let _ = tx.send(());
            }
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
        let path = "/test.Service/Stream";
        let methods =
            crate::services::Methods::new([(path.into(), crate::services::CallType::BidiStream)]);
        Call::start(methods.resolve(path), path)
    }

    fn start_watched<B>(body: B, custody: ServerCustody) -> ResponseBody<B>
    where
        B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
    {
        let body = start_response_body(body, server_call(), None, custody, None);
        assert!(
            body.is_watching(),
            "expected a response with a stop watcher"
        );
        body
    }

    #[tokio::test]
    async fn body_read_error_reaches_the_reader_and_counts_as_unknown() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let frames = futures_util::stream::iter([Err::<Frame<Bytes>, _>(
            tonic::Status::unavailable("body read failed"),
        )]);
        let mut body = into_axum_body(start_response_body(
            http_body_util::StreamBody::new(frames),
            server_call(),
            None,
            ServerCustody::default(),
            None,
        ));
        let error = body
            .frame()
            .await
            .unwrap()
            .unwrap_err()
            .into_inner()
            .downcast::<tonic::Status>()
            .unwrap();
        assert_eq!(error.code(), tonic::Code::Unavailable);
        assert_eq!(error.message(), "body read failed");
        assert!(body.frame().await.is_none());
        let rendered = handle.render();
        assert!(rendered.contains("grpc_code=\"Unknown\"} 1"), "{rendered}");
    }

    #[tokio::test(start_paused = true)]
    async fn watched_deadline_releases_unpolled_resources_and_joins_its_stop_watcher() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let token = CancellationToken::new();
        let (source, dropped_rx) = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let mut body = start_watched(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: Some(token.clone().drop_guard()),
                deadline: Some(budget_deadline()),
                end_on_drain: None,
            },
        );
        tokio::time::advance(BUDGET).await;
        tokio::time::timeout(BUDGET, dropped_rx)
            .await
            .unwrap()
            .unwrap();
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
    async fn watched_drain_releases_an_unpolled_response_without_a_deadline() {
        let permits = Arc::new(Semaphore::new(1));
        let listener_drain = CancellationToken::new();
        let response_token = listener_drain.child_token();
        let (source, dropped_rx) = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let mut body = start_watched(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: Some(response_token.clone().drop_guard()),
                deadline: None,
                end_on_drain: Some(response_token),
            },
        );
        listener_drain.cancel();
        tokio::time::timeout(BUDGET, dropped_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
        let trailers = body
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_trailers()
            .unwrap();
        assert_eq!(header_status_code(&trailers), Some(tonic::Code::Ok));
        assert!(body.frame().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_status_releases_capacity_once_and_aborts_the_stop_watcher() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let (mut source, _dropped_rx) = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let mut trailers = HeaderMap::new();
        tonic::Status::aborted("peer terminal")
            .add_header(&mut trailers)
            .unwrap();
        source.frames.push_back(Frame::trailers(trailers));
        let mut body = start_watched(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: None,
                deadline: Some(budget_deadline()),
                end_on_drain: None,
            },
        );
        assert_eq!(
            header_status_code(body.frame().await.unwrap().unwrap().trailers_ref().unwrap()),
            Some(tonic::Code::Aborted)
        );
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
        assert!(!body.is_watching());
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
    async fn response_drop_cancels_propagated_work_and_aborts_stop_watcher() {
        let cancellation = CancellationToken::new();
        let permits = Arc::new(Semaphore::new(1));
        let (source, _dropped_rx) = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let body = start_watched(
            source,
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: Some(cancellation.clone().drop_guard()),
                deadline: Some(budget_deadline()),
                end_on_drain: None,
            },
        );
        drop(body);
        assert!(cancellation.is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
        assert_eq!(permits.available_permits(), 1);
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
        let mut body = start_watched(
            PanicBody(Arc::clone(&dropped)),
            ServerCustody {
                permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                cancel_on_drop: None,
                deadline: Some(budget_deadline()),
                end_on_drain: None,
            },
        );
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
        assert!(!body.is_watching());
        assert!(body.frame().await.is_none());
        drop(body);
        let rendered = handle.render();
        assert!(rendered.contains("grpc_code=\"Internal\"} 1"), "{rendered}");
        assert!(!rendered.contains("grpc_code=\"Canceled\""), "{rendered}");
    }

    #[test]
    fn lost_stop_watcher_runtime_ends_the_call_internal() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let permits = Arc::new(Semaphore::new(1));
        let (source, _dropped_rx) = ProbeBody::pending();
        let dropped = Arc::clone(&source.dropped);
        let polls = Arc::clone(&source.polls);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut body = {
            let _entered = runtime.enter();
            start_watched(
                source,
                ServerCustody {
                    permit: Some(Arc::clone(&permits).try_acquire_owned().unwrap()),
                    cancel_on_drop: None,
                    deadline: Some(budget_deadline()),
                    end_on_drain: None,
                },
            )
        };
        drop(runtime);
        let mut cx = Context::from_waker(Waker::noop());
        let Poll::Ready(Some(Ok(frame))) = Pin::new(&mut body).poll_frame(&mut cx) else {
            unreachable!("lost watcher runtime produces terminal internal trailers");
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
        let deadline = stopped_status(Stopped::Deadline);
        assert_eq!(deadline.code(), tonic::Code::DeadlineExceeded);
        assert_eq!(
            crate::status::catalog_code(&deadline),
            Some(service_failure::Code::GatewayTimeout)
        );
        let cancelled = stopped_status(Stopped::Cancelled);
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
        let mut body = into_axum_body(start_response_body(
            ContextBody(Arc::clone(&seen)),
            call,
            Some(fallback),
            ServerCustody::default(),
            None,
        ));
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
