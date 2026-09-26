use std::cell::Cell;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll};

use futures_util::task::AtomicWaker;
use futures_util::{FutureExt as _, Stream};
use pin_project_lite::pin_project;
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tonic::{Response, Status};
use tracing::Instrument as _;

use crate::status::{panic_status, sanitize_handler_status};
use crate::validation::Validation;

tokio::task_local! {
    static CURRENT: Arc<CallState>;
}

thread_local! {
    static PANIC_SCOPE: Cell<bool> = const { Cell::new(false) };
}

static PANIC_HOOK: std::sync::Once = std::sync::Once::new();

pub(crate) fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !PANIC_SCOPE.with(Cell::get) {
                previous(info);
            }
        }));
    });
}

struct PanicScope(bool);

impl PanicScope {
    fn enter() -> Self {
        let previous = PANIC_SCOPE.with(|active| {
            let previous = active.get();
            active.set(true);
            previous
        });
        Self(previous)
    }
}

impl Drop for PanicScope {
    fn drop(&mut self) {
        PANIC_SCOPE.with(|active| active.set(self.0));
    }
}

pin_project! {
    pub(crate) struct ScopedFuture<F> {
        #[pin]
        future: F,
    }
}

pub(crate) async fn recover<F, T>(future: F) -> Result<T, ()>
where
    F: Future<Output = T>,
{
    AssertUnwindSafe(ScopedFuture { future })
        .catch_unwind()
        .await
        .map_err(|_| ())
}

pub(crate) fn recover_poll<T>(poll: impl FnOnce() -> Poll<T>) -> Result<Poll<T>, ()> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _scope = PanicScope::enter();
        poll()
    }))
    .map_err(|_| ())
}

impl<F> Future for ScopedFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let _scope = PanicScope::enter();
        self.project().future.poll(context)
    }
}

/// Shared per-RPC ownership.  It is intentionally private to generated code:
/// only the outer HTTP/2 service establishes it and codecs capture it.
pub(crate) struct CallState {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
    validation_failure: Mutex<Option<Status>>,
    terminal_status: Mutex<Option<Status>>,
    validation: Arc<Validation>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
    response_body: Mutex<Option<tonic::body::Body>>,
    body_waker: AtomicWaker,
    observation: Mutex<Option<crate::observe::Observation>>,
}

impl CallState {
    pub(crate) fn new(
        permit: Option<OwnedSemaphorePermit>,
        deadline: Option<Instant>,
        validation: Arc<Validation>,
        observation: Option<crate::observe::Observation>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cancellation: CancellationToken::new(),
            deadline,
            validation_failure: Mutex::new(None),
            terminal_status: Mutex::new(None),
            validation,
            permit: Mutex::new(permit),
            response_body: Mutex::new(None),
            body_waker: AtomicWaker::new(),
            observation: Mutex::new(observation),
        })
    }

    pub(crate) async fn scope<F, T>(state: Arc<Self>, future: F) -> T
    where
        F: Future<Output = T>,
    {
        let span = state.span();
        CURRENT.scope(state, future.instrument(span)).await
    }

    pub(crate) fn current() -> Option<Arc<Self>> {
        CURRENT.try_with(Arc::clone).ok()
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn scope_poll<T>(state: Arc<Self>, operation: impl FnOnce() -> T) -> T {
        let span = state.span();
        CURRENT.sync_scope(state, || span.in_scope(operation))
    }

    fn span(&self) -> tracing::Span {
        self.observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or_else(tracing::Span::none, crate::observe::Observation::span)
    }

    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub(crate) fn reject_validation(&self, status: Status) {
        let mut failure = self
            .validation_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if failure.is_none() {
            *failure = Some(status);
        }
    }

    pub(crate) fn validation_failure(&self) -> Option<Status> {
        self.validation_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn terminal_status(&self) -> Option<Status> {
        self.terminal_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn install_response_body(&self, body: tonic::body::Body) {
        // Initial-call ownership transfers here before any terminal waiter is
        // spawned. No completion path may release admission before this move.
        *self
            .response_body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(body);
    }

    pub(crate) fn register_body_waker(&self, waker: &std::task::Waker) {
        self.body_waker.register(waker);
    }

    pub(crate) fn with_response_body<T>(
        &self,
        operation: impl FnOnce(&mut tonic::body::Body) -> T,
    ) -> Option<T> {
        let mut body = self
            .response_body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        body.as_mut().map(operation)
    }

    fn take_response_body(&self) {
        let body = self
            .response_body
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(body);
    }

    pub(crate) fn validate<M: prost_reflect::ReflectMessage>(
        &self,
        message: &M,
    ) -> Result<(), Status> {
        let result = self.validation.validate(message);
        if let Err(status) = &result {
            self.reject_validation(status.clone());
        }
        result
    }

    pub(crate) fn finish_with_status(&self, status: Status) -> Status {
        let mut terminal = self
            .terminal_status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(status) = terminal.as_ref() {
            return status.clone();
        }
        let code = status.code();
        *terminal = Some(status.clone());
        drop(terminal);
        self.cancellation.cancel();
        self.take_response_body();
        self.permit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(observation) = self
            .observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            observation.finish(code);
        }
        self.body_waker.wake();
        status
    }

    pub(crate) fn finish_with_deadline(&self) {
        self.finish_with_status(Status::deadline_exceeded("request deadline exceeded"));
    }

    pub(crate) fn finish_with_code(&self, code: tonic::Code) -> Status {
        self.finish_with_status(Status::new(code, ""))
    }
}

/// Generated wrapper around a feature implementation.  It exposes only the
/// immutable delegate reference required by native tonic trait implementations.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct GovernedService<T> {
    inner: T,
}

impl<T> GovernedService<T> {
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    #[must_use]
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

/// Catches a panic or raw Status from a generated unary implementation while
/// the outer service has an active call state.
#[doc(hidden)]
pub async fn guard_unary<F, T>(future: F) -> Result<Response<T>, Status>
where
    F: Future<Output = Result<Response<T>, Status>>,
{
    if CallState::current().is_none() {
        return Err(panic_status());
    }
    match recover(future).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(status)) => Err(sanitize_handler_status(status)),
        Err(()) => Err(panic_status()),
    }
}

/// Catches initial and later stream failures from a generated implementation.
#[doc(hidden)]
pub async fn guard_stream<F, S, T>(future: F) -> Result<Response<GuardedStream<S>>, Status>
where
    F: Future<Output = Result<Response<S>, Status>>,
    S: Stream<Item = Result<T, Status>> + Send + 'static,
{
    let state = CallState::current().ok_or_else(panic_status)?;
    match recover(future).await {
        Ok(Ok(response)) => Ok(response.map(|inner| GuardedStream { inner, state })),
        Ok(Err(status)) => Err(sanitize_handler_status(status)),
        Err(()) => Err(panic_status()),
    }
}

pin_project! {
    /// A response stream whose errors preserve classified provenance and whose
    /// terminal state observes a sticky validation rejection.
    #[doc(hidden)]
    pub struct GuardedStream<S> {
        #[pin]
        inner: S,
        state: Arc<CallState>,
    }
}

impl<S, T> Stream for GuardedStream<S>
where
    S: Stream<Item = Result<T, Status>>,
{
    type Item = Result<T, Status>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        if let Some(status) = this.state.validation_failure() {
            return Poll::Ready(Some(Err(status)));
        }
        if let Some(status) = this.state.terminal_status() {
            return Poll::Ready(Some(Err(status)));
        }
        let polled = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _scope = PanicScope::enter();
            this.inner.poll_next(context)
        }));
        match polled {
            Err(_) => Poll::Ready(Some(Err(panic_status()))),
            Ok(Poll::Ready(Some(Ok(item)))) => Poll::Ready(Some(Ok(item))),
            Ok(Poll::Ready(Some(Err(status)))) => {
                Poll::Ready(Some(Err(sanitize_handler_status(status))))
            }
            Ok(Poll::Ready(None)) => Poll::Ready(None),
            Ok(Poll::Pending) => Poll::Pending,
        }
    }
}
