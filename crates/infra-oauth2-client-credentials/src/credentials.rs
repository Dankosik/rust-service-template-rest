use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use infra_outbound_http::{Client, PreparedRequest};
use operation_context::{Deadline, OperationContext, Stopped};
use parking_lot::{Mutex, MutexGuard};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

use crate::{
    AcquisitionError, ConfigurationError, Options, TokenTarget,
    assertion::Signer,
    options::{Admitted, admit_options},
    request_admission::AdmissionRefusal,
    token::Token,
    token_endpoint::{FETCH_TIMEOUT, TOKEN_LIMITS, TokenEndpoint},
};

mod exchange;

pub use self::exchange::ExchangeTokens;

const FAILURE_WINDOW: Duration = Duration::from_secs(1);
const REFRESH_AHEAD: Duration = Duration::from_mins(5);
const REFRESH_RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, derive_more::Debug)]
#[debug("OnBehalfOf([REDACTED])")]
pub struct OnBehalfOf(SecretString);

impl OnBehalfOf {
    #[must_use]
    pub fn new(subject_token: SecretString) -> Self {
        Self(subject_token)
    }

    fn is_empty(&self) -> bool {
        self.0.expose_secret().is_empty()
    }
}

#[must_use = "retain Registration while its token sources are in use"]
#[derive(derive_more::Debug)]
#[debug("Registration([REDACTED])")]
pub struct Registration {
    shared: Arc<RegistrationState>,
    _lifetime: watch::Sender<()>,
}

struct RegistrationState {
    endpoint: TokenEndpoint,
    closed: watch::Receiver<()>,
}

impl RegistrationState {
    fn check_admission(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        if let Some(error) = deadline.stopped_error() {
            return Err(error);
        }
        if self.closed.is_closed() {
            return Err(AcquisitionError::Unavailable);
        }
        Ok(())
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("ServiceTokens([REDACTED])")]
pub struct ServiceTokens(Arc<ServiceOwner>);

struct ServiceOwner {
    inner: Arc<ServiceState>,
    refresh_requests: mpsc::Sender<RefreshRequest>,
    _owner_release: oneshot::Sender<()>,
}

struct RefreshRequest {
    current: Arc<Token>,
    deadline: Instant,
}

#[must_use = "drive and await RefreshDriver::run for the integration lifetime"]
#[derive(derive_more::Debug)]
#[debug("RefreshDriver([REDACTED])")]
pub struct RefreshDriver {
    inner: Arc<ServiceState>,
    requests: mpsc::Receiver<RefreshRequest>,
    owner_gone: oneshot::Receiver<()>,
}

impl RefreshDriver {
    pub async fn run(mut self, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        let mut registration_closed = self.inner.registration.closed.clone();
        loop {
            let request = tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = registration_closed.changed() => break,
                _ = &mut self.owner_gone => break,
                request = self.requests.recv() => {
                    let Some(request) = request else { break };
                    request
                }
            };
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = registration_closed.changed() => break,
                _ = &mut self.owner_gone => break,
                () = self.inner.refresh_ahead(request) => {}
            }
            self.inner.cached().refresh_pending = false;
        }
    }
}

impl Drop for RefreshDriver {
    fn drop(&mut self) {
        self.requests.close();
        self.inner.cached().refresh_pending = false;
    }
}

struct ServiceState {
    registration: Arc<RegistrationState>,
    target: TokenTarget,
    cached: Mutex<Cached>,
    service_request: tokio::sync::Mutex<()>,
}

#[derive(Default)]
pub(crate) struct Cached {
    pub(crate) service: Option<ServiceToken>,
    failure: Option<CompletedFailure>,
    pub(crate) refresh_pending: bool,
}

pub(crate) struct ServiceToken {
    token: Arc<Token>,
    pub(crate) refresh_after: Instant,
}

impl Cached {
    fn completed_failure(&self, now: Instant) -> Option<AcquisitionError> {
        self.failure
            .as_ref()
            .filter(|failure| now < failure.until)
            .map(|failure| failure.error)
    }

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

impl Registration {
    pub fn prepare(options: Options) -> Result<Self, ConfigurationError> {
        let admitted = admit_options(options, "https")?;
        let token_http =
            Client::try_new(admitted.endpoint(), TOKEN_LIMITS).map_err(|_| ConfigurationError {
                key: "token_url",
                reason: "transport construction failed",
            })?;
        Self::build(admitted, token_http)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn prepare_for_test_http(options: Options) -> Result<Self, ConfigurationError> {
        let admitted = admit_options(options, "http")?;
        let token_http =
            Client::try_new_for_test_http(admitted.endpoint(), TOKEN_LIMITS).map_err(|_| {
                ConfigurationError {
                    key: "token_url",
                    reason: "requires an HTTP loopback IP address",
                }
            })?;
        Self::build(admitted, token_http)
    }

    pub(crate) fn build(
        admitted: Admitted,
        token_http: Client,
    ) -> Result<Self, ConfigurationError> {
        let (options, endpoint, permits) = admitted.into_parts();
        let signer = Signer::try_new(&options, SystemTime::now())?;
        let endpoint = TokenEndpoint::new(endpoint, token_http, signer, permits)?;
        let (lifetime, closed) = watch::channel(());
        Ok(Self {
            shared: Arc::new(RegistrationState { endpoint, closed }),
            _lifetime: lifetime,
        })
    }

    pub fn service_tokens(&self, target: TokenTarget) -> (ServiceTokens, RefreshDriver) {
        let inner = Arc::new(ServiceState {
            registration: self.shared.clone(),
            target,
            cached: Mutex::default(),
            service_request: tokio::sync::Mutex::new(()),
        });
        let (refresh_requests, requests) = mpsc::channel(1);
        let (owner_release, owner_gone) = oneshot::channel();
        (
            ServiceTokens(Arc::new(ServiceOwner {
                inner: inner.clone(),
                refresh_requests,
                _owner_release: owner_release,
            })),
            RefreshDriver {
                inner,
                requests,
                owner_gone,
            },
        )
    }

    pub fn exchange_tokens(
        &self,
        target: TokenTarget,
        capacity: u32,
    ) -> Result<ExchangeTokens, ConfigurationError> {
        ExchangeTokens::try_new(self.shared.clone(), target, capacity)
    }
}

#[derive(Clone)]
pub(crate) enum TokenSource<Subject = ()> {
    Service(ServiceTokens),
    Exchange(ExchangeTokens, Subject),
}

impl<Subject> TokenSource<Subject> {
    fn registration(&self) -> &RegistrationState {
        match self {
            Self::Service(service) => &service.0.inner.registration,
            Self::Exchange(exchange, _) => exchange.registration(),
        }
    }

    fn check_admission(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        self.registration().check_admission(deadline)?;
        if let Self::Service(service) = self {
            service.check_refresh_driver()?;
        }
        Ok(())
    }
}

impl TokenSource {
    pub(crate) fn admit(
        &self,
        subject: Result<Option<OnBehalfOf>, AdmissionRefusal>,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<TokenSource<OnBehalfOf>, AdmissionRefusal> {
        self.check_admission(deadline)
            .map_err(AdmissionRefusal::Acquisition)?;
        match (self, subject?) {
            (Self::Service(service), None) => Ok(TokenSource::Service(service.clone())),
            (Self::Service(_), Some(_)) => Err(AdmissionRefusal::UnexpectedSubject),
            (Self::Exchange(exchange, ()), Some(subject)) if !subject.is_empty() => {
                Ok(TokenSource::Exchange(exchange.clone(), subject))
            }
            (Self::Exchange(_, ()), _) => Err(AdmissionRefusal::SubjectRequired),
        }
    }
}

impl TokenSource<OnBehalfOf> {
    pub(crate) async fn acquire_for_request(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_admission(deadline)?;
        let mut registration_closed = self.registration().closed.clone();
        let acquisition = async {
            match self {
                Self::Service(service) => service.service_token(deadline).await,
                Self::Exchange(exchange, OnBehalfOf(subject)) => {
                    exchange.exchange(subject, deadline).await
                }
            }
        };
        let acquired = tokio::select! {
            biased;
            stopped = deadline.context.wait_stopped() => return Err(acquisition_stopped(stopped)),
            _ = registration_closed.changed() => Err(AcquisitionError::Unavailable),
            result = acquisition => result,
        };
        self.check_admission(deadline)?;
        let acquired = acquired?;
        acquired.check_expiry(Instant::now())?;
        Ok(acquired)
    }
}

impl ServiceTokens {
    fn check_admission(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        self.0.inner.registration.check_admission(deadline)?;
        self.check_refresh_driver()
    }

    fn check_refresh_driver(&self) -> Result<(), AcquisitionError> {
        if self.0.refresh_requests.is_closed() {
            return Err(AcquisitionError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_admission(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reusable_service_token(now) {
            return Ok(token);
        }
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
        let (context, request_context) = deadline.fetch_contexts(now);
        let result = inner
            .registration
            .endpoint
            .fetch_service_token(&inner.target, &context, request_context.as_ref())
            .await;
        self.check_admission(deadline)?;
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

    #[cfg(test)]
    pub(crate) fn service_request(&self) -> &tokio::sync::Mutex<()> {
        &self.0.inner.service_request
    }
}

impl ServiceState {
    fn cached(&self) -> MutexGuard<'_, Cached> {
        self.cached.lock()
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
        let endpoint = &self.registration.endpoint;
        let context = OperationContext::from_deadline(Deadline::at(request.deadline));
        match endpoint
            .fetch_service_token(&self.target, &context, None)
            .await
        {
            Ok(token) => {
                self.store_service_token(token);
                self.cached().defer_refresh(Instant::now());
            }
            Err(error) => {
                self.remember_failure(provider_timeout(error));
                tracing::warn!(
                    server.address = endpoint.host(),
                    error.type = error.label(),
                    "oauth2_background_refresh_failed"
                );
            }
        }
    }
}

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
    context: AcquisitionContext<'a>,
}

impl AcquisitionDeadline<'_> {
    fn fetch_contexts(self, now: Instant) -> (OperationContext, Option<OperationContext>) {
        let ceiling = Deadline::at(self.cutoff.min(now + FETCH_TIMEOUT));
        match self.context {
            AcquisitionContext::Operation(context) => (context.child_until(ceiling), None),
            AcquisitionContext::Http(request) => request.child_contexts(ceiling),
        }
    }

    fn expired_error(self) -> Option<AcquisitionError> {
        (Instant::now() >= self.cutoff).then_some(self.expiry)
    }

    fn stopped_error(self) -> Option<AcquisitionError> {
        self.expired_error()
            .or_else(|| self.context.stopped().map(acquisition_stopped))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum AcquisitionContext<'a> {
    Operation(&'a OperationContext),
    Http(&'a PreparedRequest<'a>),
}

impl<'a> From<&'a OperationContext> for AcquisitionContext<'a> {
    fn from(context: &'a OperationContext) -> Self {
        Self::Operation(context)
    }
}

impl<'a> From<&'a PreparedRequest<'_>> for AcquisitionContext<'a> {
    fn from(request: &'a PreparedRequest<'_>) -> Self {
        Self::Http(request)
    }
}

impl AcquisitionContext<'_> {
    fn stopped(self) -> Option<Stopped> {
        match self {
            Self::Operation(context) => context.stopped(),
            Self::Http(request) => request.stopped(),
        }
    }

    async fn wait_stopped(self) -> Stopped {
        match self {
            Self::Operation(context) => context.wait_stopped().await,
            Self::Http(request) => request.wait_stopped().await,
        }
    }
}

pub(crate) fn acquisition_deadline<'a>(
    context: impl Into<AcquisitionContext<'a>>,
) -> AcquisitionDeadline<'a> {
    let context = context.into();
    let deadline = match context {
        AcquisitionContext::Operation(context) => context.deadline(),
        AcquisitionContext::Http(request) => request.deadline(),
    };
    let (cutoff, expiry) = match deadline.and_then(Deadline::instant) {
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
    }
}

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

fn service_refresh_after(requested_at: Instant, reuse_until: Instant) -> Instant {
    let maximum_lead = REFRESH_AHEAD.min(reuse_until.saturating_duration_since(requested_at) / 4);
    let lead = maximum_lead.saturating_sub(sampled_refresh_spread(maximum_lead / 10));
    reuse_until - lead
}

fn sampled_refresh_spread(window: Duration) -> Duration {
    let mut bytes = [0_u8; 2];
    let sample = aws_lc_rs::rand::fill(&mut bytes).map(|()| u16::from_be_bytes(bytes));
    refresh_spread(window, sample)
}

pub(crate) fn refresh_spread(
    window: Duration,
    sample: Result<u16, aws_lc_rs::error::Unspecified>,
) -> Duration {
    sample.map_or(Duration::ZERO, |sample| {
        shared_rules::jitter::share_of_window(window, sample)
    })
}

fn refresh_retry() -> Duration {
    REFRESH_RETRY + sampled_refresh_spread(REFRESH_RETRY / 10)
}
