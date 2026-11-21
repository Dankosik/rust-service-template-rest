use http::HeaderMap;
use infra_grpc_core::protocol::{STATUS_HEADERS, header_status_code};
use infra_grpc_core::watched::{PanicOutcome, StreamEnd, StreamHooks, WatchedBody};
use operation_context::{Deadline, OperationContext, Stopped};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::observe::Call;
use crate::status::FixedFailure;

pub(crate) struct ServerCustody {
    pub(crate) permit: Option<OwnedSemaphorePermit>,
    pub(crate) cancel_on_drop: Option<DropGuard>,
    pub(crate) stop: Option<ServerStop>,
}

impl ServerCustody {
    pub(crate) fn deadline(deadline: Option<Deadline>) -> Self {
        Self {
            permit: None,
            cancel_on_drop: None,
            stop: deadline.map(ServerStop::Deadline),
        }
    }

    fn into_hooks(
        self,
        call: Call,
        fallback: Option<opentelemetry::Context>,
    ) -> (ServerHooks, Option<ServerStop>) {
        let deadline = self.stop.as_ref().and_then(ServerStop::deadline);
        let hooks = ServerHooks {
            call,
            permit: self.permit,
            cancel_on_drop: self.cancel_on_drop,
            deadline,
            fallback,
        };
        (hooks, self.stop)
    }
}

pub(crate) fn stopped_status(reason: Stopped) -> tonic::Status {
    match reason {
        Stopped::Deadline => FixedFailure::Deadline.status(),
        Stopped::Cancelled => tonic::Status::cancelled("request cancelled"),
    }
}

pub(crate) fn check_open(context: &OperationContext) -> Result<(), tonic::Status> {
    context.check().map_err(stopped_status)
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

struct ServerGuards {
    _permit: Option<OwnedSemaphorePermit>,
    _cancel_on_drop: Option<DropGuard>,
}

struct ServerHooks {
    call: Call,
    permit: Option<OwnedSemaphorePermit>,
    cancel_on_drop: Option<DropGuard>,
    deadline: Option<Deadline>,
    fallback: Option<opentelemetry::Context>,
}

impl<E> StreamHooks<E> for ServerHooks {
    type Stop = LocalStatus;
    type Outcome = ServerOutcome;
    type Guards = ServerGuards;

    fn stopped(&self) -> Option<LocalStatus> {
        self.deadline
            .is_some_and(Deadline::expired)
            .then(|| LocalStatus::failure(FixedFailure::Deadline))
    }

    fn in_scope<R>(&self, poll: impl FnOnce() -> R) -> R {
        let _entered = self.call.span().enter();
        let _attached = self.fallback.clone().map(opentelemetry::Context::attach);
        poll()
    }

    fn on_panic(&self) -> PanicOutcome<LocalStatus, ServerOutcome> {
        PanicOutcome::Stop(LocalStatus::failure(FixedFailure::Internal))
    }

    fn write_stop(&self, stop: LocalStatus, trailers: &mut HeaderMap) -> ServerOutcome {
        stop.write_headers(trailers)
    }

    fn end_outcome(&self, end: StreamEnd<'_, E>) -> ServerOutcome {
        match end {
            StreamEnd::Trailers(trailers) => {
                ServerOutcome::from_headers(trailers).unwrap_or_else(ServerOutcome::unknown)
            }
            StreamEnd::Error(_) | StreamEnd::Empty => ServerOutcome::unknown(),
        }
    }

    fn watcher_lost() -> LocalStatus {
        LocalStatus::failure(FixedFailure::Internal)
    }

    fn emit(self, outcome: ServerOutcome) -> ServerGuards {
        if let Some(failure) = outcome.failure {
            self.call.record_failure(failure);
        }
        self.call.finish(outcome.code);
        ServerGuards {
            _permit: self.permit,
            _cancel_on_drop: self.cancel_on_drop,
        }
    }
}

pub(crate) enum ServerStop {
    Drain {
        deadline: Option<Deadline>,
        drain: CancellationToken,
    },
    Deadline(Deadline),
}

impl ServerStop {
    const fn deadline(&self) -> Option<Deadline> {
        match *self {
            Self::Drain { deadline, .. } => deadline,
            Self::Deadline(deadline) => Some(deadline),
        }
    }
}

async fn wait_server_stop(stop: ServerStop) -> LocalStatus {
    match stop {
        ServerStop::Drain { deadline, drain } => {
            let context = OperationContext::new(deadline, drain);
            match context.wait_stopped().await {
                Stopped::Deadline => LocalStatus::failure(FixedFailure::Deadline),
                Stopped::Cancelled => LocalStatus::drained(),
            }
        }
        ServerStop::Deadline(deadline) => {
            deadline.wait().await;
            LocalStatus::failure(FixedFailure::Deadline)
        }
    }
}

pub(crate) fn wrap_response(
    response: axum::response::Response,
    call: Call,
    fallback: Option<opentelemetry::Context>,
    custody: ServerCustody,
) -> axum::response::Response {
    let (hooks, stop) = custody.into_hooks(call, fallback);
    WatchedBody::attach_response(
        response,
        hooks,
        |_, response| ServerOutcome::from_response(response),
        move |_| stop.map(wait_server_stop),
        axum::body::Body::new,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use bytes::Bytes;
    use http_body::{Body, Frame};
    use http_body_util::BodyExt as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };
    use parking_lot::Mutex;
    use tokio::sync::{Semaphore, oneshot};

    use super::*;

    const BUDGET: Duration = Duration::from_secs(1);

    type ResponseBody<B> = WatchedBody<B, ServerHooks>;

    fn start_response_body<B>(
        body: B,
        call: Call,
        fallback: Option<opentelemetry::Context>,
        custody: ServerCustody,
        initial: Option<ServerOutcome>,
    ) -> Option<ResponseBody<B>>
    where
        B: Body<Data = Bytes> + Unpin + Send + 'static,
    {
        let (hooks, stop) = custody.into_hooks(call, fallback);
        WatchedBody::start(body, hooks, initial, move |_| stop.map(wait_server_stop))
    }

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
        let methods = crate::services::Methods::new([(
            path.into(),
            crate::services::CallType::BidiStream,
            false,
            crate::services::MethodPolicy::None,
        )]);
        Call::start(methods.resolve(path), path)
    }

    fn start_watched<B>(body: B, custody: ServerCustody) -> ResponseBody<B>
    where
        B: Body<Data = Bytes, Error = tonic::Status> + Unpin + Send + 'static,
    {
        let body = start_response_body(body, server_call(), None, custody, None)
            .expect("expected a response with a stop watcher");
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
        let mut body = axum::body::Body::new(
            start_response_body(
                http_body_util::StreamBody::new(frames),
                server_call(),
                None,
                ServerCustody::deadline(None),
                None,
            )
            .unwrap(),
        );
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
                stop: Some(ServerStop::Deadline(budget_deadline())),
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
                stop: Some(ServerStop::Drain {
                    deadline: None,
                    drain: response_token,
                }),
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
                stop: Some(ServerStop::Deadline(budget_deadline())),
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
                stop: Some(ServerStop::Deadline(budget_deadline())),
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
                stop: Some(ServerStop::Deadline(budget_deadline())),
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
                    stop: Some(ServerStop::Deadline(budget_deadline())),
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
        let mut body = axum::body::Body::new(
            start_response_body(
                ContextBody(Arc::clone(&seen)),
                call,
                Some(fallback),
                ServerCustody::deadline(None),
                None,
            )
            .unwrap(),
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
