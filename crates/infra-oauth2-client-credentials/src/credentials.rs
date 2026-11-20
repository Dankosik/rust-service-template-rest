use std::{
    mem::DropGuard,
    num::NonZeroU32,
    pin::pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use arc_swap::ArcSwapOption;
use infra_outbound_http::Client;
use operation_context::{Deadline, OperationContext};
use parking_lot::{Mutex, MutexGuard};
use secrecy::{ExposeSecret as _, SecretString};
use tokio::{sync::watch, time::Instant};

use crate::{
    AcquisitionError, ConfigurationError, ExchangeTokenTarget, Options, PreparationError,
    ServiceTokenTarget,
    assertion::Signer,
    options::{ValidatedRegistration, validate_options},
    token::Token,
    token_endpoint::{self, FETCH_TIMEOUT, TokenEndpoint},
};

mod exchange;

pub use self::exchange::ExchangeTokens;

const FAILURE_WINDOW: Duration = Duration::from_secs(1);
const REFRESH_AHEAD: Duration = Duration::from_mins(5);
const REFRESH_RETRY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("on-behalf-of subject token must not be empty")]
pub struct EmptySubject;

#[derive(Clone, derive_more::Debug)]
#[debug("OnBehalfOf([REDACTED])")]
pub struct OnBehalfOf(SecretString);

impl OnBehalfOf {
    pub fn try_new(subject_token: SecretString) -> Result<Self, EmptySubject> {
        if subject_token.expose_secret().is_empty() {
            return Err(EmptySubject);
        }
        Ok(Self(subject_token))
    }

    pub(crate) const fn as_secret(&self) -> &SecretString {
        &self.0
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
    inner: DropGuard<Arc<ServiceState>, fn(Arc<ServiceState>)>,
}

struct RefreshRequest {
    current: Arc<Token>,
    deadline: Instant,
}

enum RefreshSlot {
    Idle,
    Queued(RefreshRequest),
    Running,
    Stopped,
}

#[must_use = "drive and await RefreshDriver::run for the integration lifetime"]
#[derive(derive_more::Debug)]
#[debug("RefreshDriver([REDACTED])")]
pub struct RefreshDriver {
    requests: watch::Receiver<RefreshSlot>,
    inner: DropGuard<Arc<ServiceState>, fn(Arc<ServiceState>)>,
}

impl RefreshDriver {
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        let inner = self.inner;
        let mut requests = self.requests;
        let mut stop_changes = requests.clone();
        let mut registration_closed = inner.registration.closed.clone();
        let mut stop = pin!(async {
            tokio::select! {
                biased;
                () = shutdown => {}
                _ = registration_closed.changed() => {}
                _ = stop_changes.wait_for(|slot| matches!(slot, RefreshSlot::Stopped)) => {}
            }
        });
        loop {
            tokio::select! {
                biased;
                () = &mut stop => break,
                result = requests.wait_for(|slot| matches!(slot, RefreshSlot::Queued(_))) => {
                    if result.is_err() {
                        break;
                    }
                }
            }
            let Some(request) = inner.take_queued_refresh() else {
                continue;
            };
            tokio::select! {
                biased;
                () = &mut stop => break,
                () = inner.refresh_ahead(request) => {}
            }
            inner.finish_refresh();
        }
    }
}

struct ServiceState {
    registration: Arc<RegistrationState>,
    target: ServiceTokenTarget,
    resource_origin: http::Uri,
    active: ArcSwapOption<ServiceToken>,
    cached: Mutex<Option<CachedFailure>>,
    refresh: watch::Sender<RefreshSlot>,
    fetch_lock: tokio::sync::Mutex<()>,
}

struct ServiceToken {
    token: Arc<Token>,
    refresh_after: Instant,
}

struct CachedFailure {
    error: AcquisitionError,
    until: Instant,
}

impl CachedFailure {
    fn active(&self, now: Instant) -> Option<AcquisitionError> {
        (now < self.until).then_some(self.error)
    }
}

impl Registration {
    pub fn prepare(options: Options) -> Result<Self, PreparationError> {
        let validated = validate_options(options)?;
        let token_http = token_endpoint::build_https_client(&validated.token_url, "token_url")?;
        Self::build(validated, token_http).map_err(PreparationError::from)
    }

    pub(crate) fn build(
        validated: ValidatedRegistration,
        token_http: Client,
    ) -> Result<Self, ConfigurationError> {
        let signer = Signer::try_new(validated.signer, SystemTime::now())?;
        let endpoint =
            TokenEndpoint::new(validated.token_url, token_http, signer, validated.permits)?;
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
        let (refresh, requests) = watch::channel(RefreshSlot::Idle);
        let inner = Arc::new(ServiceState {
            registration: self.shared.clone(),
            target,
            resource_origin,
            active: ArcSwapOption::empty(),
            cached: Mutex::default(),
            refresh,
            fetch_lock: tokio::sync::Mutex::new(()),
        });
        (
            ServiceTokens(Arc::new(ServiceOwner {
                inner: DropGuard::new(Arc::clone(&inner), ServiceState::stop),
            })),
            RefreshDriver {
                requests,
                inner: DropGuard::new(inner, ServiceState::stop),
            },
        )
    }

    pub fn exchange_tokens(
        &self,
        target: ExchangeTokenTarget,
        resource_origin: http::Uri,
        capacity: NonZeroU32,
    ) -> Result<ExchangeTokens, ConfigurationError> {
        ExchangeTokens::try_new(self.shared.clone(), target, resource_origin, capacity)
    }
}

impl ServiceTokens {
    pub(crate) fn resource_origin(&self) -> &http::Uri {
        &self.0.inner.resource_origin
    }

    pub(crate) fn check_active(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<(), AcquisitionError> {
        self.0.inner.registration.check_active(deadline)?;
        if self.0.inner.refresh.is_closed() {
            return Err(AcquisitionError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn acquire_for_request(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_active(deadline)?;
        let acquired = if tokio::task::coop::has_budget_remaining()
            && let Some(token) = self.reuse_service_token_and_schedule_refresh(Instant::now())
        {
            Ok(token)
        } else {
            let mut registration_closed = self.0.inner.registration.closed.clone();
            tokio::select! {
                biased;
                stopped = deadline.context.wait_stopped() => return Err(stopped.into()),
                _ = registration_closed.changed() => Err(AcquisitionError::Unavailable),
                result = self.service_token(deadline) => result,
            }
        };
        self.check_active(deadline)?;
        let acquired = acquired?;
        acquired.check_expiry(Instant::now())?;
        Ok(acquired)
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
        let inner: &ServiceState = &self.0.inner;
        let _fetch_guard = tokio::time::timeout_at(deadline.cutoff, inner.fetch_lock.lock())
            .await
            .map_err(|_| deadline.expiry_error())?;
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
        let active = self.0.inner.active.load();
        if let Some(service) = active.as_deref()
            && service.token.is_reusable(now)
            && now < service.refresh_after
        {
            return Some(Arc::clone(&service.token));
        }
        drop(active);
        let mut cached = self.0.inner.lock_cached();
        let active = self.0.inner.active.load();
        let service = active
            .as_deref()
            .filter(|service| service.token.is_reusable(now))?;
        let token = Arc::clone(&service.token);
        if now >= service.refresh_after {
            self.schedule_refresh_ahead(&mut cached, now, &token);
        }
        Some(token)
    }

    fn schedule_refresh_ahead(
        &self,
        cached: &mut Option<CachedFailure>,
        now: Instant,
        token: &Arc<Token>,
    ) {
        if cached
            .as_ref()
            .and_then(|failure| failure.active(now))
            .is_some()
        {
            return;
        }
        let mut queued = false;
        self.0.inner.refresh.send_if_modified(|slot| {
            if !matches!(slot, RefreshSlot::Idle) {
                return false;
            }
            *slot = RefreshSlot::Queued(RefreshRequest {
                current: Arc::clone(token),
                deadline: now + FETCH_TIMEOUT,
            });
            queued = true;
            true
        });
        if queued {
            self.0.inner.defer_refresh(cached, now);
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_cached(&self) {
        let mut cached = self.0.inner.lock_cached();
        *cached = None;
        self.0.inner.active.store(None);
        self.0.inner.refresh.send_if_modified(|slot| {
            if !matches!(slot, RefreshSlot::Stopped) {
                *slot = RefreshSlot::Idle;
            }
            false
        });
    }

    #[cfg(test)]
    pub(crate) fn refresh_pending(&self) -> bool {
        matches!(
            *self.0.inner.refresh.borrow(),
            RefreshSlot::Queued(_) | RefreshSlot::Running
        )
    }

    #[cfg(test)]
    pub(crate) fn fetch_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.0.inner.fetch_lock
    }
}

impl ServiceState {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "std::mem::DropGuard invokes its drop callback with owned Arc<ServiceState>"
    )]
    fn stop(state: Arc<Self>) {
        state.refresh.send_if_modified(|slot| {
            if matches!(slot, RefreshSlot::Stopped) {
                return false;
            }
            *slot = RefreshSlot::Stopped;
            true
        });
    }

    fn take_queued_refresh(&self) -> Option<RefreshRequest> {
        let mut request = None;
        self.refresh.send_if_modified(|slot| {
            if matches!(slot, RefreshSlot::Queued(_))
                && let RefreshSlot::Queued(queued) = std::mem::replace(slot, RefreshSlot::Running)
            {
                request = Some(queued);
            }
            false
        });
        request
    }

    fn finish_refresh(&self) {
        self.refresh.send_if_modified(|slot| {
            if matches!(slot, RefreshSlot::Running) {
                *slot = RefreshSlot::Idle;
            }
            false
        });
    }

    fn lock_cached(&self) -> MutexGuard<'_, Option<CachedFailure>> {
        self.cached.lock()
    }

    fn cached_failure(&self, now: Instant) -> Option<AcquisitionError> {
        self.lock_cached()
            .as_ref()
            .and_then(|failure| failure.active(now))
    }

    fn remember_failure(&self, error: AcquisitionError) {
        if !is_cacheable_failure(error) {
            return;
        }
        *self.lock_cached() = Some(CachedFailure {
            error,
            until: Instant::now() + FAILURE_WINDOW,
        });
    }

    fn store_service_token(&self, token: Token) -> Arc<Token> {
        let token = Arc::new(token);
        let mut cached = self.lock_cached();
        self.store_service_token_locked(&mut cached, &token, Instant::now());
        token
    }

    fn store_service_token_locked(
        &self,
        cached: &mut Option<CachedFailure>,
        token: &Arc<Token>,
        now: Instant,
    ) -> bool {
        *cached = None;
        let Some(reuse_until) = token.reusable_until(now) else {
            return false;
        };
        let refresh_after = service_refresh_after(token.requested_at, reuse_until);
        self.active.store(Some(Arc::new(ServiceToken {
            token: Arc::clone(token),
            refresh_after,
        })));
        true
    }

    fn defer_refresh(&self, _cached: &mut Option<CachedFailure>, from: Instant) {
        let next = from + refresh_retry();
        if let Some(service) = self.active.load().as_deref() {
            self.active.store(Some(Arc::new(ServiceToken {
                token: Arc::clone(&service.token),
                refresh_after: service.refresh_after.max(next),
            })));
        }
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
        if !self.active.load().as_deref().is_some_and(|service| {
            Arc::ptr_eq(&service.token, &request.current) && !service.token.is_rejected()
        }) {
            return;
        }
        let endpoint = &self.registration.endpoint;
        let context = OperationContext::from_deadline(Deadline::at(request.deadline));
        match endpoint.fetch_service_token(&self.target, &context).await {
            Ok(token) => {
                let now = Instant::now();
                let mut cached = self.lock_cached();
                if !self.store_service_token_locked(&mut cached, &Arc::new(token), now) {
                    self.defer_refresh(&mut cached, now);
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
    source: CutoffSource,
    context: &'a OperationContext,
}

#[derive(Clone, Copy)]
enum CutoffSource {
    Request,
    Synthetic,
}

impl AcquisitionDeadline<'_> {
    fn fetch_context(self, now: Instant) -> OperationContext {
        let ceiling = Deadline::at(self.cutoff.min(now + FETCH_TIMEOUT));
        self.context.child_until(ceiling)
    }

    fn expiry_error(self) -> AcquisitionError {
        match self.source {
            CutoffSource::Request => AcquisitionError::Timeout,
            CutoffSource::Synthetic => AcquisitionError::Unavailable,
        }
    }

    fn stopped_error(self) -> Option<AcquisitionError> {
        if matches!(self.source, CutoffSource::Synthetic) && Instant::now() >= self.cutoff {
            return Some(self.expiry_error());
        }
        self.context.stopped().map(AcquisitionError::from)
    }
}

pub(crate) fn acquisition_deadline(context: &OperationContext) -> AcquisitionDeadline<'_> {
    let (cutoff, source) = match context.deadline().and_then(Deadline::instant) {
        Some(cutoff) => (cutoff, CutoffSource::Request),
        None => (Instant::now() + FETCH_TIMEOUT, CutoffSource::Synthetic),
    };
    AcquisitionDeadline {
        cutoff,
        source,
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
