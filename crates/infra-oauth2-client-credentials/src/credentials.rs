use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, SystemTime},
};

use http::{HeaderMap, header::AUTHORIZATION};
use infra_outbound_http::Client;
use operation_context::{Deadline, OperationContext, Stopped};
use secrecy::SecretString;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use crate::{
    AcquisitionError, ConfigurationError, Options,
    assertion::Signer,
    options::{Admitted, admit_options},
    token::Token,
    token_endpoint::{FETCH_TIMEOUT, TOKEN_LIMITS, TokenEndpoint},
};

mod exchange;

use self::exchange::ExchangeState;

const FAILURE_WINDOW: Duration = Duration::from_secs(1);
/// A reusable token is replaced in the background once at most this much, or a
/// quarter, of its reuse remains, as Azure.Core refreshes five minutes early.
const REFRESH_AHEAD: Duration = Duration::from_mins(5);
/// Minimum spacing between background refresh attempts. The next attempt is
/// scheduled at least this far ahead, so a failed attempt or a refused enqueue
/// keeps its spacing.
const REFRESH_RETRY: Duration = Duration::from_secs(30);

/// The verified inbound access token a caller acts on behalf of (RFC 8693).
/// Attached to a request's extensions, it replaces the service token with one
/// exchanged for the subject it names. A binding that only ever acts for a
/// verified user can [`require it`](crate::AuthenticatedClient::require_on_behalf_of),
/// so a forgotten subject is an error and never a request made with the
/// service's own authority.
#[derive(Clone, derive_more::Debug)]
#[debug("OnBehalfOf([REDACTED])")]
pub struct OnBehalfOf(SecretString);

impl OnBehalfOf {
    #[must_use]
    pub fn new(subject_token: SecretString) -> Self {
        Self(subject_token)
    }
}

/// An idle, cloneable owner of one private credential tuple and its tokens.
#[derive(Clone, derive_more::Debug)]
#[debug("Credentials([REDACTED])")]
pub struct Credentials(pub(crate) Arc<Owner>);

pub(crate) struct Owner {
    pub(crate) inner: Arc<Inner>,
    refresh_requests: mpsc::Sender<RefreshRequest>,
    // Dropping the final external owner drops both senders. The driver never
    // owns Owner, so its own Inner reference cannot keep credentials alive.
    // This oneshot is the prompt release boundary: it cancels a queued or
    // in-flight refresh. `refresh_requests.is_closed()` is a separate liveness
    // probe for whether the driver has stopped.
    _owner_release: oneshot::Sender<()>,
}

struct RefreshRequest {
    current: Arc<Token>,
    deadline: Instant,
}

/// The refresh lifetime owned by the integration's composition root.
/// Drive [`Self::run`] while credentials are used and await its completion before
/// dropping dependencies. Dropping this driver closes all surviving clients.
#[must_use = "drive and await RefreshDriver::run for the integration lifetime"]
#[derive(derive_more::Debug)]
#[debug("RefreshDriver([REDACTED])")]
pub struct RefreshDriver {
    inner: Arc<Inner>,
    requests: mpsc::Receiver<RefreshRequest>,
    owner_gone: oneshot::Receiver<()>,
}

impl RefreshDriver {
    /// Runs refreshes inline until shutdown or final external-owner release.
    /// This method spawns no tasks; completion observes all owned refresh work.
    /// Final owner release cancels any in-flight refresh and discards queued
    /// requests.
    pub async fn run(mut self, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            let request = tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = &mut self.owner_gone => break,
                request = self.requests.recv() => {
                    let Some(request) = request else { break };
                    request
                }
            };
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = &mut self.owner_gone => break,
                () = self.inner.refresh_ahead(request) => {}
            }
            self.inner.cached().refresh_pending = false;
        }
    }
}

impl Drop for RefreshDriver {
    fn drop(&mut self) {
        // Close before clearing: once `refresh_pending` reads false, no enqueue
        // can succeed, so a caller in [`Credentials::schedule_refresh_ahead`]
        // never leaves the flag set for a request no driver will run.
        self.requests.close();
        self.inner.cached().refresh_pending = false;
    }
}

pub(crate) struct Inner {
    endpoint: TokenEndpoint,
    /// The last service token; never held across an `.await`.
    cached: Mutex<Cached>,
    /// Held by the one caller requesting a new service token, so requests
    /// for it never overlap.
    pub(crate) service_request: tokio::sync::Mutex<()>,
    pub(crate) exchange: ExchangeState,
}

#[derive(Default)]
pub(crate) struct Cached {
    pub(crate) service: Option<ServiceToken>,
    failure: Option<CompletedFailure>,
    /// True from enqueue until the driver finishes the attempt. The driver
    /// clears it after every attempt, so one caller may schedule the next.
    pub(crate) refresh_pending: bool,
}

pub(crate) struct ServiceToken {
    token: Arc<Token>,
    /// When the next background refresh of this still reusable token may start.
    pub(crate) refresh_after: Instant,
}

impl Cached {
    fn completed_failure(&self, now: Instant) -> Option<AcquisitionError> {
        self.failure
            .as_ref()
            .filter(|failure| now < failure.until)
            .map(|failure| failure.error)
    }

    /// Moves the next background attempt to at least `from + refresh_retry()`,
    /// without pulling an existing schedule earlier.
    fn defer_refresh(&mut self, from: Instant) {
        let next = from + refresh_retry();
        if let Some(service) = self.service.as_mut() {
            service.refresh_after = service.refresh_after.max(next);
        }
    }
}

struct CompletedFailure {
    error: AcquisitionError,
    until: Instant,
}

impl Credentials {
    /// Prepares credentials without performing DNS, token, or resource I/O.
    /// The integration must drive and await the returned [`RefreshDriver`].
    /// Dropping or completing that driver closes any surviving clients.
    ///
    /// # Errors
    /// Returns a sanitized option, clock, or transport-construction failure.
    pub fn prepare(options: Options) -> Result<(Self, RefreshDriver), ConfigurationError> {
        let admitted = admit_options(options)?;
        let token_http =
            Client::try_new(admitted.endpoint(), TOKEN_LIMITS).map_err(|_| ConfigurationError {
                key: "token_url",
                reason: "transport construction failed",
            })?;
        Self::build(admitted, token_http)
    }

    pub(crate) fn build(
        admitted: Admitted,
        token_http: Client,
    ) -> Result<(Self, RefreshDriver), ConfigurationError> {
        let (options, endpoint, permits) = admitted.into_parts();
        let signer = Signer::try_new(&options, SystemTime::now())?;
        let endpoint = TokenEndpoint::new(
            endpoint,
            token_http,
            options.scopes,
            options.audience,
            signer,
            permits,
        )?;
        let inner = Arc::new(Inner {
            endpoint,
            cached: Mutex::default(),
            service_request: tokio::sync::Mutex::new(()),
            exchange: ExchangeState::new(options.exchange_cache_capacity),
        });
        let (refresh_requests, requests) = mpsc::channel(1);
        let (owner_release, owner_gone) = oneshot::channel();
        Ok((
            Self(Arc::new(Owner {
                inner: inner.clone(),
                refresh_requests,
                _owner_release: owner_release,
            })),
            RefreshDriver {
                inner,
                requests,
                owner_gone,
            },
        ))
    }

    pub(crate) fn check_lifecycle(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<(), AcquisitionError> {
        if self.0.refresh_requests.is_closed() {
            return Err(deadline
                .stopped_error()
                .unwrap_or(AcquisitionError::Unavailable));
        }
        Ok(())
    }

    fn check_admission(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        if let Some(error) = deadline.expired_error() {
            return Err(error);
        }
        self.check_lifecycle(deadline)
    }

    /// Inserts a sensitive Bearer header for the service token or, when
    /// `on_behalf_of` is set, a token exchanged for its subject. Returns what
    /// was used, so a resource can invalidate exactly that token.
    pub(crate) async fn authorize(
        &self,
        headers: &mut HeaderMap,
        on_behalf_of: Option<OnBehalfOf>,
        context: &OperationContext,
        request_context: Option<&OperationContext>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        context.check().map_err(acquisition_stopped)?;
        let deadline = acquisition_deadline(context, request_context);
        self.check_admission(deadline)?;
        let acquisition = async {
            match on_behalf_of {
                Some(OnBehalfOf(subject)) => self.exchange(&subject, deadline).await,
                None => self.service_token(deadline).await,
            }
        };
        let acquired = tokio::select! {
            biased;
            stopped = context.wait_stopped() => return Err(acquisition_stopped(stopped)),
            result = acquisition => result,
        };
        context.check().map_err(acquisition_stopped)?;
        let acquired = acquired?;
        acquired.check_expiry(Instant::now())?;
        headers.insert(AUTHORIZATION, acquired.header.clone());
        Ok(acquired)
    }

    /// Returns the cached service token while it is reusable, otherwise
    /// requests a new one. Waiting for another caller's request spends this
    /// caller's deadline.
    pub(crate) async fn service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_admission(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reusable_service_token(now) {
            return Ok(token);
        }
        // Boxed so that the reuse path above keeps a small future.
        Box::pin(self.acquire_service_token(deadline)).await
    }

    async fn acquire_service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let inner = &self.0.inner;
        let _service_request =
            tokio::time::timeout_at(deadline.cutoff, inner.service_request.lock())
                .await
                .map_err(|_| deadline.expiry)?;
        self.check_admission(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reusable_service_token(now) {
            return Ok(token);
        }
        if let Some(error) = inner.completed_failure(now) {
            return Err(error);
        }
        let result = inner
            .endpoint
            .fetch_service_token(deadline.cutoff.min(now + FETCH_TIMEOUT))
            .await;
        if let Some(error) = deadline.stopped_error() {
            return Err(error);
        }
        match result.map_err(provider_timeout) {
            Ok(token) => Ok(inner.store_service_token(token)),
            Err(error) => {
                inner.remember_failure(error);
                Err(error)
            }
        }
    }

    fn reusable_service_token(&self, now: Instant) -> Option<Arc<Token>> {
        let mut cached = self.cached();
        let service = cached
            .service
            .as_ref()
            .filter(|service| service.token.is_reusable(now))?;
        let token = service.token.clone();
        if now >= service.refresh_after {
            self.schedule_refresh_ahead(&mut cached, now, &token);
        }
        Some(token)
    }

    /// Queues one background refresh of a token reuse found due for one,
    /// unless a failure or a queued attempt suppresses it. The caller holds
    /// the cached guard across the read and the send.
    fn schedule_refresh_ahead(&self, cached: &mut Cached, now: Instant, token: &Arc<Token>) {
        if cached.refresh_pending || cached.completed_failure(now).is_some() {
            return;
        }
        cached.defer_refresh(now);
        cached.refresh_pending = true;
        if self
            .0
            .refresh_requests
            .try_send(RefreshRequest {
                current: token.clone(),
                deadline: now + FETCH_TIMEOUT,
            })
            .is_err()
        {
            cached.refresh_pending = false;
        }
    }

    pub(crate) fn cached(&self) -> MutexGuard<'_, Cached> {
        self.0.inner.cached()
    }
}

impl Inner {
    fn cached(&self) -> MutexGuard<'_, Cached> {
        self.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn completed_failure(&self, now: Instant) -> Option<AcquisitionError> {
        self.cached().completed_failure(now)
    }

    fn remember_failure(&self, error: AcquisitionError) {
        if !is_cacheable_failure(error) {
            return;
        }
        self.cached().failure = Some(CompletedFailure {
            error,
            until: Instant::now() + FAILURE_WINDOW,
        });
    }

    fn store_service_token(&self, token: Token) -> Arc<Token> {
        let token = Arc::new(token);
        let mut cached = self.cached();
        cached.failure = None;
        if let Some(reuse_until) = token.reusable_until(Instant::now()) {
            let refresh_after = service_refresh_after(token.requested_at, reuse_until);
            cached.service = Some(ServiceToken {
                token: token.clone(),
                refresh_after,
            });
        }
        token
    }

    async fn refresh_ahead(&self, request: RefreshRequest) {
        let Ok(_service_request) =
            tokio::time::timeout_at(request.deadline, self.service_request.lock()).await
        else {
            return;
        };
        let now = Instant::now();
        if now >= request.deadline || self.completed_failure(now).is_some() {
            return;
        }
        if !self.cached().service.as_ref().is_some_and(|service| {
            Arc::ptr_eq(&service.token, &request.current) && !service.token.is_rejected()
        }) {
            return;
        }
        match self.endpoint.fetch_service_token(request.deadline).await {
            Ok(token) => {
                self.store_service_token(token);
                self.cached().defer_refresh(Instant::now());
            }
            Err(error) => {
                self.remember_failure(provider_timeout(error));
                tracing::warn!(
                    server.address = self.endpoint.host(),
                    error.type = error.label(),
                    "oauth2_background_refresh_failed"
                );
            }
        }
    }
}

/// Caller-local refusals stay private to the caller that hit them.
fn is_cacheable_failure(error: AcquisitionError) -> bool {
    match error {
        AcquisitionError::AtCapacity | AcquisitionError::Cancelled | AcquisitionError::Timeout => {
            false
        }
        AcquisitionError::Transport
        | AcquisitionError::ResponseLimit
        | AcquisitionError::Unavailable
        | AcquisitionError::Rejected(_)
        | AcquisitionError::InvalidResponse
        | AcquisitionError::Assertion => true,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct AcquisitionDeadline<'a> {
    cutoff: Instant,
    expiry: AcquisitionError,
    context: &'a OperationContext,
    request_context: Option<&'a OperationContext>,
}

impl AcquisitionDeadline<'_> {
    fn expired_error(self) -> Option<AcquisitionError> {
        (Instant::now() >= self.cutoff).then_some(self.expiry)
    }

    fn stopped_error(self) -> Option<AcquisitionError> {
        self.expired_error().or_else(|| {
            self.request_context
                .and_then(OperationContext::stopped)
                .or_else(|| self.context.stopped())
                .map(acquisition_stopped)
        })
    }
}

pub(crate) fn acquisition_deadline<'a>(
    context: &'a OperationContext,
    request_context: Option<&'a OperationContext>,
) -> AcquisitionDeadline<'a> {
    // Keep acquisition bounded when the resource deadline cannot be expressed
    // as an Instant. That fallback belongs to the adapter.
    let (cutoff, expiry) = match context.deadline().and_then(Deadline::instant) {
        Some(cutoff) => (cutoff, AcquisitionError::Timeout),
        None => (
            Instant::now() + FETCH_TIMEOUT,
            AcquisitionError::Unavailable,
        ),
    };
    AcquisitionDeadline {
        cutoff,
        expiry,
        context,
        request_context,
    }
}

/// Reclassifies an attempt that ended at [`FETCH_TIMEOUT`] as
/// [`AcquisitionError::Unavailable`] while the caller still has budget.
fn provider_timeout(error: AcquisitionError) -> AcquisitionError {
    match error {
        AcquisitionError::Timeout => AcquisitionError::Unavailable,
        other => other,
    }
}

pub(crate) fn acquisition_stopped(stopped: Stopped) -> AcquisitionError {
    match stopped {
        Stopped::Deadline => AcquisitionError::Timeout,
        Stopped::Cancelled => AcquisitionError::Cancelled,
    }
}

/// The single sampled background-refresh instant for a reusable service token:
/// the smaller of five minutes and a quarter of its reusable lifetime, with the
/// lead reduced by at most one tenth.
fn service_refresh_after(requested_at: Instant, reuse_until: Instant) -> Instant {
    let maximum_lead = REFRESH_AHEAD.min(reuse_until.saturating_duration_since(requested_at) / 4);
    let lead = maximum_lead.saturating_sub(sampled_refresh_spread(maximum_lead / 10));
    reuse_until - lead
}

/// The window is at most 30 seconds (one tenth of the maximum refresh lead).
/// Callers add the sample to a minimum interval or subtract it from a maximum
/// lead, so jitter never moves a refresh past its documented bound.
fn sampled_refresh_spread(window: Duration) -> Duration {
    let mut bytes = [0_u8; 2];
    let sample = aws_lc_rs::rand::fill(&mut bytes).map(|()| u16::from_be_bytes(bytes));
    refresh_spread(window, sample)
}

pub(crate) fn refresh_spread(
    window: Duration,
    sample: Result<u16, aws_lc_rs::error::Unspecified>,
) -> Duration {
    // Source failure discards the bytes and falls back to the unjittered
    // base: the maximum lead or the base retry interval.
    sample.map_or(Duration::ZERO, |sample| {
        shared_rules::jitter::share_of_window(window, sample)
    })
}

fn refresh_retry() -> Duration {
    REFRESH_RETRY + sampled_refresh_spread(REFRESH_RETRY / 10)
}
