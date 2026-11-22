use std::{
    mem::DropGuard,
    num::NonZeroU32,
    pin::pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use infra_outbound_http::{Client, Origin};
use operation_context::{Deadline, OperationContext};
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
    state: DropGuard<Arc<ServiceState>, fn(Arc<ServiceState>)>,
}

struct RefreshRequest {
    current: Arc<Token>,
    cutoff: Instant,
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
    state: DropGuard<Arc<ServiceState>, fn(Arc<ServiceState>)>,
}

impl RefreshDriver {
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        let Self {
            mut requests,
            state,
        } = self;
        let mut registration_closed = state.registration.closed.clone();
        let mut stop = pin!(async {
            tokio::select! {
                biased;
                () = shutdown => {}
                _ = registration_closed.changed() => {}
            }
        });
        loop {
            let refresh_requested = tokio::select! {
                biased;
                () = &mut stop => false,
                result = requests.wait_for(|slot| {
                    matches!(slot, RefreshSlot::Queued(_) | RefreshSlot::Stopped)
                }) => result.is_ok_and(|slot| matches!(*slot, RefreshSlot::Queued(_))),
            };
            if !refresh_requested {
                break;
            }
            let Some(request) = state.take_queued_refresh() else {
                continue;
            };
            tokio::select! {
                biased;
                () = &mut stop => break,
                _ = requests.wait_for(|slot| matches!(slot, RefreshSlot::Stopped)) => break,
                () = state.background_refresh(request) => {}
            }
            state.finish_refresh();
        }
    }
}

mod service_cache {
    use std::sync::Arc;

    use arc_swap::ArcSwapOption;
    use parking_lot::Mutex;
    use tokio::time::Instant;

    use super::{
        AcquisitionError, FAILURE_WINDOW, Token, is_cacheable_failure, refresh_retry,
        service_refresh_after,
    };

    struct ScheduledToken {
        token: Arc<Token>,
        refresh_after: Instant,
    }

    struct CachedFailure {
        error: AcquisitionError,
        until: Instant,
    }

    fn unexpired_failure(
        failure: Option<&CachedFailure>,
        now: Instant,
    ) -> Option<AcquisitionError> {
        failure
            .filter(|failure| now < failure.until)
            .map(|failure| failure.error)
    }

    #[derive(Default)]
    pub(super) struct ServiceTokenCache {
        active: ArcSwapOption<ScheduledToken>,
        writer: Mutex<Option<CachedFailure>>,
    }

    impl ServiceTokenCache {
        pub(super) fn reuse_or_schedule(
            &self,
            now: Instant,
            try_queue: impl FnOnce(&Arc<Token>) -> bool,
        ) -> Option<Arc<Token>> {
            let active = self.active.load();
            if let Some(scheduled) = active.as_deref()
                && now < scheduled.refresh_after
                && !scheduled.token.is_rejected()
            {
                return Some(Arc::clone(&scheduled.token));
            }
            drop(active);
            let failure = self.writer.lock();
            let active = self.active.load();
            let scheduled = active.as_deref()?;
            let reuse_until = scheduled.token.reusable_until(now)?;
            let token = Arc::clone(&scheduled.token);
            let refresh_due = now >= scheduled.refresh_after
                && unexpired_failure(failure.as_ref(), now).is_none();
            if refresh_due && try_queue(&token) {
                self.postpone_refresh(scheduled, now, reuse_until);
            }
            Some(token)
        }

        pub(super) fn matches_active(&self, expected: &Arc<Token>) -> bool {
            self.active.load().as_deref().is_some_and(|scheduled| {
                Arc::ptr_eq(&scheduled.token, expected) && !scheduled.token.is_rejected()
            })
        }

        pub(super) fn cached_failure(&self, now: Instant) -> Option<AcquisitionError> {
            unexpired_failure(self.writer.lock().as_ref(), now)
        }

        pub(super) fn remember_failure(&self, error: AcquisitionError, now: Instant) {
            if !is_cacheable_failure(error) {
                return;
            }
            *self.writer.lock() = Some(CachedFailure {
                error,
                until: now + FAILURE_WINDOW,
            });
        }

        pub(super) fn store(&self, token: Token, now: Instant) -> Arc<Token> {
            let token = Arc::new(token);
            let mut failure = self.writer.lock();
            *failure = None;
            if let Some(reuse_until) = token.reusable_until(now) {
                self.store_active(Arc::clone(&token), reuse_until);
            }
            token
        }

        pub(super) fn store_or_defer(&self, token: Token, now: Instant) {
            let mut failure = self.writer.lock();
            *failure = None;
            if let Some(reuse_until) = token.reusable_until(now) {
                self.store_active(Arc::new(token), reuse_until);
            } else if let Some(scheduled) = self.active.load().as_deref()
                && let Some(reuse_until) = scheduled.token.reusable_until(now)
            {
                self.postpone_refresh(scheduled, now, reuse_until);
            }
        }

        fn store_active(&self, token: Arc<Token>, reuse_until: Instant) {
            let refresh_after = service_refresh_after(token.requested_at, reuse_until);
            self.active.store(Some(Arc::new(ScheduledToken {
                token,
                refresh_after,
            })));
        }

        fn postpone_refresh(&self, scheduled: &ScheduledToken, now: Instant, reuse_until: Instant) {
            let next = now + refresh_retry();
            self.active.store(Some(Arc::new(ScheduledToken {
                token: Arc::clone(&scheduled.token),
                refresh_after: scheduled.refresh_after.max(next).min(reuse_until),
            })));
        }

        #[cfg(test)]
        pub(super) fn clear(&self) {
            let mut failure = self.writer.lock();
            *failure = None;
            self.active.store(None);
        }
    }
}

struct ServiceState {
    registration: Arc<RegistrationState>,
    target: ServiceTokenTarget,
    resource_origin: http::Uri,
    cache: service_cache::ServiceTokenCache,
    refresh: watch::Sender<RefreshSlot>,
    fetch_lock: tokio::sync::Mutex<()>,
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
        resource_origin: Origin,
    ) -> (ServiceTokens, RefreshDriver) {
        self.service_tokens_uri(target, resource_origin.into())
    }

    pub(crate) fn service_tokens_uri(
        &self,
        target: ServiceTokenTarget,
        resource_origin: http::Uri,
    ) -> (ServiceTokens, RefreshDriver) {
        let (refresh, requests) = watch::channel(RefreshSlot::Idle);
        let state = Arc::new(ServiceState {
            registration: self.shared.clone(),
            target,
            resource_origin,
            cache: service_cache::ServiceTokenCache::default(),
            refresh,
            fetch_lock: tokio::sync::Mutex::new(()),
        });
        (
            ServiceTokens(Arc::new(ServiceOwner {
                state: DropGuard::new(Arc::clone(&state), ServiceState::stop),
            })),
            RefreshDriver {
                requests,
                state: DropGuard::new(state, ServiceState::stop),
            },
        )
    }

    pub fn exchange_tokens(
        &self,
        target: ExchangeTokenTarget,
        resource_origin: Origin,
        capacity: NonZeroU32,
    ) -> Result<ExchangeTokens, ConfigurationError> {
        self.exchange_tokens_uri(target, resource_origin.into(), capacity)
    }

    pub(crate) fn exchange_tokens_uri(
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
        &self.0.state.resource_origin
    }

    pub(crate) fn check_active(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<(), AcquisitionError> {
        self.0.state.registration.check_active(deadline)?;
        if self.0.state.refresh.is_closed() {
            return Err(AcquisitionError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn acquire_for_request(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_active(deadline)?;
        let outcome = if tokio::task::coop::has_budget_remaining()
            && let Some(token) = self.0.state.reuse_or_schedule(Instant::now())
        {
            Ok(token)
        } else {
            let mut registration_closed = self.0.state.registration.closed.clone();
            tokio::select! {
                biased;
                stopped = deadline.context.wait_stopped() => return Err(stopped.into()),
                _ = registration_closed.changed() => Err(AcquisitionError::Unavailable),
                result = self.service_token(deadline) => result,
            }
        };
        self.check_active(deadline)?;
        let token = outcome?;
        token.check_expiry(Instant::now())?;
        Ok(token)
    }

    pub(crate) async fn service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        self.check_active(deadline)?;
        let now = Instant::now();
        if let Some(token) = self.0.state.reuse_or_schedule(now) {
            return Ok(token);
        }
        Box::pin(self.acquire_service_token(deadline)).await
    }

    async fn acquire_service_token(
        &self,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let state: &ServiceState = &self.0.state;
        let _fetch_guard = tokio::time::timeout_at(deadline.cutoff, state.fetch_lock.lock())
            .await
            .map_err(|_| deadline.expiry_error())?;
        self.check_active(deadline)?;
        let now = Instant::now();
        if let Some(token) = state.reuse_or_schedule(now) {
            return Ok(token);
        }
        if let Some(error) = state.cache.cached_failure(now) {
            return Err(error);
        }
        let context = deadline.fetch_context(now);
        let result = state
            .registration
            .endpoint
            .fetch_service_token(&state.target, &context)
            .await;
        self.check_active(deadline)?;
        match result.map_err(timeout_as_unavailable) {
            Ok(token) => Ok(state.cache.store(token, Instant::now())),
            Err(error) => {
                state.cache.remember_failure(error, Instant::now());
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_cached(&self) {
        self.0.state.cache.clear();
        self.0.state.refresh.send_if_modified(|slot| {
            if !matches!(slot, RefreshSlot::Stopped) {
                *slot = RefreshSlot::Idle;
            }
            false
        });
    }

    #[cfg(test)]
    pub(crate) fn refresh_pending(&self) -> bool {
        matches!(
            *self.0.state.refresh.borrow(),
            RefreshSlot::Queued(_) | RefreshSlot::Running
        )
    }

    #[cfg(test)]
    pub(crate) fn fetch_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.0.state.fetch_lock
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

    fn reuse_or_schedule(&self, now: Instant) -> Option<Arc<Token>> {
        self.cache
            .reuse_or_schedule(now, |token| self.try_queue_refresh(now, token))
    }

    fn try_queue_refresh(&self, now: Instant, token: &Arc<Token>) -> bool {
        self.refresh.send_if_modified(|slot| {
            if !matches!(slot, RefreshSlot::Idle) {
                return false;
            }
            *slot = RefreshSlot::Queued(RefreshRequest {
                current: Arc::clone(token),
                cutoff: now + FETCH_TIMEOUT,
            });
            true
        })
    }

    async fn background_refresh(&self, request: RefreshRequest) {
        let Ok(_fetch_guard) =
            tokio::time::timeout_at(request.cutoff, self.fetch_lock.lock()).await
        else {
            return;
        };
        let now = Instant::now();
        if now >= request.cutoff || self.cache.cached_failure(now).is_some() {
            return;
        }
        if !self.cache.matches_active(&request.current) {
            return;
        }
        let endpoint = &self.registration.endpoint;
        let context = OperationContext::from_deadline(Deadline::at(request.cutoff));
        match endpoint.fetch_service_token(&self.target, &context).await {
            Ok(token) => self.cache.store_or_defer(token, Instant::now()),
            Err(error) => {
                let cached = timeout_as_unavailable(error);
                self.cache.remember_failure(cached, Instant::now());
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
