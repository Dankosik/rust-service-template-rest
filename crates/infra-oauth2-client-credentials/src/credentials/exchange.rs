use std::{collections::hash_map::Entry, sync::Arc, time::Duration};

use foldhash::HashMap;
use moka::Expiry;
use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::{sync::watch, time::Instant};

use super::{AcquisitionDeadline, RegistrationState, timeout_as_unavailable};
use crate::{AcquisitionError, ConfigurationError, ExchangeTokenTarget, token::Token};

const EXCHANGE_CACHE_BYTES: u32 = 16 * 1024 * 1024;
const EXCHANGE_CACHE_CAPACITY_RANGE: std::ops::RangeInclusive<u32> = 1..=65_536;

#[derive(Clone, derive_more::Debug)]
#[debug("ExchangeTokens([REDACTED])")]
pub struct ExchangeTokens(Arc<ExchangeOwner>);

struct ExchangeOwner {
    registration: Arc<RegistrationState>,
    target: ExchangeTokenTarget,
    resource_origin: http::Uri,
    state: ExchangeState,
}

impl ExchangeOwner {
    async fn fetch(
        &self,
        key: SubjectKey,
        subject: &SecretString,
        deadline: AcquisitionDeadline<'_>,
    ) -> ExchangeResult {
        let context = deadline.fetch_context(Instant::now());
        let result = self
            .registration
            .endpoint
            .fetch_exchange(subject, &self.target, &context)
            .await
            .map(Arc::new);
        if let Some(error) = deadline.stopped_error() {
            return Err(error);
        }
        let token = result.map_err(timeout_as_unavailable)?;
        if token.is_reusable(Instant::now()) {
            self.state.cache.insert(key, token.clone()).await;
        }
        Ok(token)
    }
}

type ExchangedTokenCache =
    moka::future::Cache<SubjectKey, Arc<Token>, foldhash::quality::RandomState>;

struct ExchangeState {
    cache: ExchangedTokenCache,
    flights: ExchangeFlights,
}

impl ExchangeState {
    fn new(capacity: u32) -> Self {
        let minimum_weight = EXCHANGE_CACHE_BYTES / capacity;
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(u64::from(minimum_weight * capacity))
                .weigher(move |_: &SubjectKey, token: &Arc<Token>| {
                    u32::try_from(token.header.as_bytes().len())
                        .unwrap_or(u32::MAX)
                        .max(minimum_weight)
                })
                .expire_after(ExchangedExpiry)
                .build_with_hasher(foldhash::quality::RandomState::default()),
            flights: Mutex::default(),
        }
    }

    async fn reusable(&self, key: SubjectKey) -> Option<Arc<Token>> {
        self.cache
            .get(&key)
            .await
            .filter(|token| token.is_reusable(Instant::now()))
    }

    fn join(&self, key: SubjectKey) -> ExchangeFlight<'_> {
        let mut flights = self.flights.lock();
        match flights.entry(key) {
            Entry::Occupied(entry) => ExchangeFlight::Waiter(entry.get().clone()),
            Entry::Vacant(entry) => {
                let (sender, receiver) = watch::channel(None);
                entry.insert(receiver);
                ExchangeFlight::Leader(ExchangeLeader {
                    flights: &self.flights,
                    key,
                    sender,
                    completed: None,
                })
            }
        }
    }
}

type ExchangeResult = Result<Arc<Token>, AcquisitionError>;
type ExchangeReceiver = watch::Receiver<Option<ExchangeResult>>;
type ExchangeFlights = Mutex<HashMap<SubjectKey, ExchangeReceiver>>;

enum ExchangeFlight<'a> {
    Leader(ExchangeLeader<'a>),
    Waiter(ExchangeReceiver),
}

struct ExchangeLeader<'a> {
    flights: &'a ExchangeFlights,
    key: SubjectKey,
    sender: watch::Sender<Option<ExchangeResult>>,
    completed: Option<ExchangeResult>,
}

impl ExchangeLeader<'_> {
    fn publish(mut self, result: ExchangeResult) {
        self.completed = Some(result);
    }
}

impl Drop for ExchangeLeader<'_> {
    fn drop(&mut self) {
        self.flights.lock().remove(&self.key);
        if let Some(result) = self.completed.take() {
            self.sender.send_replace(Some(result));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SubjectKey([u8; 32]);

impl SubjectKey {
    fn of(subject: &SecretString) -> Self {
        let digest = aws_lc_rs::digest::digest(
            &aws_lc_rs::digest::SHA256,
            subject.expose_secret().as_bytes(),
        );
        let mut key = [0_u8; 32];
        key.copy_from_slice(digest.as_ref());
        Self(key)
    }
}

struct ExchangedExpiry;

impl Expiry<SubjectKey, Arc<Token>> for ExchangedExpiry {
    fn expire_after_create(
        &self,
        _key: &SubjectKey,
        token: &Arc<Token>,
        _current_time: std::time::Instant,
    ) -> Option<Duration> {
        let now = Instant::now();
        let until = token.reusable_until(now);
        Some(until.map_or(Duration::ZERO, |until| until.saturating_duration_since(now)))
    }

    fn expire_after_update(
        &self,
        key: &SubjectKey,
        token: &Arc<Token>,
        current_time: std::time::Instant,
        _duration_until_expiry: Option<Duration>,
    ) -> Option<Duration> {
        self.expire_after_create(key, token, current_time)
    }
}

impl ExchangeTokens {
    pub(super) fn try_new(
        registration: Arc<RegistrationState>,
        target: ExchangeTokenTarget,
        resource_origin: http::Uri,
        capacity: u32,
    ) -> Result<Self, ConfigurationError> {
        if !EXCHANGE_CACHE_CAPACITY_RANGE.contains(&capacity) {
            return Err(ConfigurationError {
                key: "exchange_cache_capacity",
                reason: "must be from 1 to 65536",
            });
        }
        Ok(Self(Arc::new(ExchangeOwner {
            registration,
            target,
            resource_origin,
            state: ExchangeState::new(capacity),
        })))
    }

    pub(super) fn registration(&self) -> &RegistrationState {
        &self.0.registration
    }

    pub(super) fn resource_origin(&self) -> &http::Uri {
        &self.0.resource_origin
    }

    #[cfg(test)]
    pub(crate) fn cache(&self) -> &ExchangedTokenCache {
        &self.0.state.cache
    }

    pub(crate) async fn exchange(
        &self,
        subject: &SecretString,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let key = SubjectKey::of(subject);
        let registration = self.registration();
        registration.check_active(deadline)?;
        let result = if let Some(token) = self.0.state.reusable(key).await {
            Ok(token)
        } else {
            tokio::time::timeout_at(
                deadline.cutoff,
                self.acquire_exchange(key, subject, deadline),
            )
            .await
            .map_err(|_| deadline.expiry_error)?
        };
        registration.check_active(deadline)?;
        result
    }

    async fn acquire_exchange(
        &self,
        key: SubjectKey,
        subject: &SecretString,
        deadline: AcquisitionDeadline<'_>,
    ) -> ExchangeResult {
        let registration = self.registration();
        let state = &self.0.state;
        loop {
            registration.check_active(deadline)?;
            match state.join(key) {
                ExchangeFlight::Leader(leader) => {
                    let result = if let Some(token) = state.reusable(key).await {
                        Ok(token)
                    } else {
                        self.0.fetch(key, subject, deadline).await
                    };
                    registration.check_active(deadline)?;
                    leader.publish(result.clone());
                    return result;
                }
                ExchangeFlight::Waiter(mut receiver) => {
                    let Ok(published) = receiver.wait_for(Option::is_some).await else {
                        continue;
                    };
                    match published.as_ref() {
                        Some(Ok(token)) if token.is_reusable(Instant::now()) => {
                            return Ok(token.clone());
                        }
                        Some(Err(error)) => return Err(*error),
                        _ => {}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn short_tokens_cannot_use_the_weight_remainder_for_an_extra_subject()
    -> Result<(), AcquisitionError> {
        const CAPACITY: u32 = 4_160;
        let cache = ExchangeState::new(CAPACITY).cache;
        let token = Arc::new(Token::bearer("short", Instant::now(), Some(3600))?);
        for subject in 0..=CAPACITY {
            let mut key = [0; 32];
            key[..4].copy_from_slice(&subject.to_be_bytes());
            cache.insert(SubjectKey(key), token.clone()).await;
        }
        cache.run_pending_tasks().await;
        assert!(cache.entry_count() <= u64::from(CAPACITY));
        Ok(())
    }
}
