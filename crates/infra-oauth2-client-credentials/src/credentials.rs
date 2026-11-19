use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use infra_outbound_http::{Client, RequestContext};
use operation_context::{Deadline, OperationContext};
use parking_lot::{Mutex, MutexGuard};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

use crate::{
    AcquisitionError, ConfigurationError, ExchangeTokenTarget, Options, PreparationError,
    ServiceTokenTarget, TransportBuildFailure,
    assertion::Signer,
    options::{Admitted, Scheme, admit_options},
    request_admission::{AdmissionRefusal, RequestSubject},
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
    fn check_active(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
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
        let mut registration_closed = self.inner.registration.closed.clone();
        let stop = async {
            tokio::select! {
                biased;
                () = shutdown => {}
                _ = registration_closed.changed() => {}
                _ = &mut self.owner_gone => {}
            }
        };
        tokio::pin!(stop);
        loop {
            let request = tokio::select! {
                biased;
                () = &mut stop => break,
                request = self.requests.recv() => {
                    let Some(request) = request else { break };
                    request
                }
            };
            tokio::select! {
                biased;
                () = &mut stop => break,
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
    target: ServiceTokenTarget,
    resource_origin: http::Uri,
    cached: Mutex<Cached>,
    fetch_lock: tokio::sync::Mutex<()>,
}

#[derive(Default)]
pub(crate) struct Cached {
    pub(crate) service: Option<ServiceToken>,
    failure: Option<CachedFailure>,
    pub(crate) refresh_pending: bool,
}

pub(crate) struct ServiceToken {
    token: Arc<Token>,
    pub(crate) refresh_after: Instant,
}

impl Cached {
    fn cached_failure(&self, now: Instant) -> Option<AcquisitionError> {
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

struct CachedFailure {
    error: AcquisitionError,
    until: Instant,
}

impl Registration {
    pub fn prepare(options: Options) -> Result<Self, PreparationError> {
        let admitted = admit_options(options, Scheme::Https, Scheme::Https)?;
        let token_http = Client::try_new(&admitted.endpoint, TOKEN_LIMITS)
            .map_err(TransportBuildFailure::token_url)?;
        Self::build(admitted, token_http).map_err(PreparationError::from)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn prepare_for_test_http(options: Options) -> Result<Self, ConfigurationError> {
        let admitted = admit_options(options, Scheme::Http, Scheme::Https)?;
        let token_http =
            Client::try_new_for_test_http(&admitted.endpoint, TOKEN_LIMITS).map_err(|_| {
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
        let signer = Signer::try_new(options, SystemTime::now())?;
        let endpoint = TokenEndpoint::new(endpoint, token_http, signer, permits)?;
        let (lifetime, closed) = watch::channel(());
        Ok(Self {
            shared: Arc::new(RegistrationState { endpoint, closed }),
            _lifetime: lifetime,
        })
    }

    pub fn service_tokens(
        &self,
        target: ServiceTokenTarget,
        resource_origin: http::Uri,
    ) -> (ServiceTokens, RefreshDriver) {
        let inner = Arc::new(ServiceState {
            registration: self.shared.clone(),
            target,
            resource_origin,
            cached: Mutex::default(),
            fetch_lock: tokio::sync::Mutex::new(()),
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
        target: ExchangeTokenTarget,
        resource_origin: http::Uri,
        capacity: u32,
    ) -> Result<ExchangeTokens, ConfigurationError> {
        ExchangeTokens::try_new(self.shared.clone(), target, resource_origin, capacity)
    }
}

#[derive(Clone)]
pub(crate) enum TokenSource<Subject = ()> {
    Service(ServiceTokens),
    Exchange(ExchangeTokens, Subject),
}

impl From<&ServiceTokens> for TokenSource {
    fn from(tokens: &ServiceTokens) -> Self {
        Self::Service(tokens.clone())
    }
}

impl From<&ExchangeTokens> for TokenSource {
    fn from(tokens: &ExchangeTokens) -> Self {
        Self::Exchange(tokens.clone(), ())
    }
}

impl<Subject> TokenSource<Subject> {
    pub(crate) fn resource_origin(&self) -> &http::Uri {
        match self {
            Self::Service(service) => &service.0.inner.resource_origin,
            Self::Exchange(exchange, _) => exchange.resource_origin(),
        }
    }

    fn registration(&self) -> &RegistrationState {
        match self {
            Self::Service(service) => &service.0.inner.registration,
            Self::Exchange(exchange, _) => exchange.registration(),
        }
    }

    fn check_active(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        match self {
            Self::Service(service) => service.check_active(deadline),
            Self::Exchange(exchange, _) => exchange.registration().check_active(deadline),
        }
    }
}

impl TokenSource {
    pub(crate) fn admit(
        &self,
        subject: RequestSubject,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<TokenSource<OnBehalfOf>, AdmissionRefusal> {
        self.check_active(deadline)
            .map_err(AdmissionRefusal::Acquisition)?;
        match (self, subject) {
            (_, RequestSubject::AuthorizationConflict) => {
                Err(AdmissionRefusal::AuthorizationConflict)
            }
            (Self::Service(service), RequestSubject::None) => {
                Ok(TokenSource::Service(service.clone()))
            }
            (Self::Service(_), RequestSubject::OnBehalfOf(_)) => {
                Err(AdmissionRefusal::UnexpectedSubject)
            }
            (Self::Exchange(exchange, ()), RequestSubject::OnBehalfOf(subject))
                if !subject.is_empty() =>
            {
                Ok(TokenSource::Exchange(exchange.clone(), subject))
            }
            (Self::Exchange(_, ()), RequestSubject::None | RequestSubject::OnBehalfOf(_)) => {
                Err(AdmissionRefusal::SubjectRequired)
            }
        }
    }
}

impl TokenSource<OnBehalfOf> {
    pub(crate) async fn acquire_for_request(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_active(deadline)?;
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
            stopped = deadline.context.wait_stopped() => return Err(stopped.into()),
            _ = registration_closed.changed() => Err(AcquisitionError::Unavailable),
            result = acquisition => result,
        };
        self.check_active(deadline)?;
        let acquired = acquired?;
        acquired.check_expiry(Instant::now())?;
        Ok(acquired)
    }
}

impl ServiceTokens {
    fn check_active(&self, deadline: AcquisitionDeadline<'_>) -> Result<(), AcquisitionError> {
        self.0.inner.registration.check_active(deadline)?;
        if self.0.refresh_requests.is_closed() {
            return Err(AcquisitionError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_active(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reuse_service_token_and_schedule_refresh(now) {
            return Ok(token);
        }
        Box::pin(self.acquire_service_token(deadline)).await
    }

    async fn acquire_service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let inner = &self.0.inner;
        let _fetch_guard = tokio::time::timeout_at(deadline.cutoff, inner.fetch_lock.lock())
            .await
            .map_err(|_| deadline.expiry_error)?;
        self.check_active(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.reuse_service_token_and_schedule_refresh(now) {
            return Ok(token);
        }
        if let Some(error) = inner.cached_failure(now) {
            return Err(error);
        }
        let context = deadline.fetch_context(now);
        let result = inner
            .registration
            .endpoint
            .fetch_service_token(&inner.target, &context)
            .await;
        self.check_active(deadline)?;
        match result.map_err(timeout_as_unavailable) {
            Ok(token) => Ok(inner.store_service_token(token)),
            Err(error) => {
                inner.remember_failure(error);
                Err(error)
            }
        }
    }

    fn reuse_service_token_and_schedule_refresh(&self, now: Instant) -> Option<Arc<Token>> {
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
        if cached.refresh_pending || cached.cached_failure(now).is_some() {
            return;
        }
        cached.defer_refresh(now);
        cached.refresh_pending = self
            .0
            .refresh_requests
            .try_send(RefreshRequest {
                current: token.clone(),
                deadline: now + FETCH_TIMEOUT,
            })
            .is_ok();
    }

    pub(crate) fn cached(&self) -> MutexGuard<'_, Cached> {
        self.0.inner.cached()
    }

    #[cfg(test)]
    pub(crate) fn fetch_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.0.inner.fetch_lock
    }
}

impl ServiceState {
    fn cached(&self) -> MutexGuard<'_, Cached> {
        self.cached.lock()
    }

    fn cached_failure(&self, now: Instant) -> Option<AcquisitionError> {
        self.cached().cached_failure(now)
    }

    fn remember_failure(&self, error: AcquisitionError) {
        if !is_cacheable_failure(error) {
            return;
        }
        self.cached().failure = Some(CachedFailure {
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
        let Ok(_fetch_guard) =
            tokio::time::timeout_at(request.deadline, self.fetch_lock.lock()).await
        else {
            return;
        };
        let now = Instant::now();
        if now >= request.deadline || self.cached_failure(now).is_some() {
            return;
        }
        if !self.cached().service.as_ref().is_some_and(|service| {
            Arc::ptr_eq(&service.token, &request.current) && !service.token.is_rejected()
        }) {
            return;
        }
        let endpoint = &self.registration.endpoint;
        let context = OperationContext::from_deadline(Deadline::at(request.deadline)).into();
        match endpoint.fetch_service_token(&self.target, &context).await {
            Ok(token) => {
                self.store_service_token(token);
                let mut cached = self.cached();
                if cached
                    .service
                    .as_ref()
                    .is_some_and(|service| Arc::ptr_eq(&service.token, &request.current))
                {
                    cached.defer_refresh(Instant::now());
                }
            }
            Err(error) => {
                self.remember_failure(timeout_as_unavailable(error));
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
    expiry_error: AcquisitionError,
    context: &'a RequestContext,
}

impl AcquisitionDeadline<'_> {
    fn fetch_context(self, now: Instant) -> RequestContext {
        let ceiling = Deadline::at(self.cutoff.min(now + FETCH_TIMEOUT));
        self.context.child_until(ceiling)
    }

    fn expired_error(self) -> Option<AcquisitionError> {
        (Instant::now() >= self.cutoff).then_some(self.expiry_error)
    }

    fn stopped_error(self) -> Option<AcquisitionError> {
        self.expired_error()
            .or_else(|| self.context.stopped().map(AcquisitionError::from))
    }
}

pub(crate) fn acquisition_deadline(context: &RequestContext) -> AcquisitionDeadline<'_> {
    let (cutoff, expiry_error) = match context.deadline().and_then(Deadline::instant) {
        Some(cutoff) => (cutoff, AcquisitionError::Timeout),
        None => (
            Instant::now() + FETCH_TIMEOUT,
            AcquisitionError::Unavailable,
        ),
    };
    AcquisitionDeadline {
        cutoff,
        expiry_error,
        context,
    }
}

fn timeout_as_unavailable(error: AcquisitionError) -> AcquisitionError {
    match error {
        AcquisitionError::Timeout => AcquisitionError::Unavailable,
        other => other,
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
