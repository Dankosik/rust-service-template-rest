use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use moka::Expiry;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::{sync::watch, time::Instant};

use super::{AcquisitionDeadline, Credentials, provider_timeout};
use crate::{
    AcquisitionError,
    token::Token,
    token_endpoint::{FETCH_TIMEOUT, TokenEndpoint},
};

const EXCHANGE_CACHE_BYTES: u32 = 16 * 1024 * 1024;

pub(crate) struct ExchangeState {
    pub(crate) cache: moka::future::Cache<SubjectKey, Arc<Token>>,
    flights: ExchangeFlights,
}

impl ExchangeState {
    pub(super) fn new(capacity: u32) -> Self {
        // Rounded down: moka admits an entry only while the total weight stays
        // within the limit, so a rounded-up weight would retain fewer subjects
        // than configured.
        let minimum_weight = EXCHANGE_CACHE_BYTES / capacity;
        Self {
            cache: moka::future::Cache::builder()
                .max_capacity(u64::from(EXCHANGE_CACHE_BYTES))
                .weigher(move |_: &SubjectKey, token: &Arc<Token>| {
                    u32::try_from(token.header.as_bytes().len())
                        .unwrap_or(u32::MAX)
                        .max(minimum_weight)
                })
                .expire_after(ExchangedExpiry)
                .build(),
            flights: Mutex::default(),
        }
    }

    async fn fetch(
        &self,
        endpoint: &TokenEndpoint,
        key: SubjectKey,
        subject: &SecretString,
        deadline: AcquisitionDeadline<'_>,
    ) -> ExchangeResult {
        let fetch_deadline = deadline.cutoff.min(Instant::now() + FETCH_TIMEOUT);
        let result = endpoint
            .fetch_exchange(subject, fetch_deadline)
            .await
            .map(Arc::new);
        if let Some(error) = deadline.stopped_error() {
            return Err(error);
        }
        if let Ok(token) = &result
            && token.is_reusable(Instant::now())
        {
            self.cache.insert(key, token.clone()).await;
        }
        result.map_err(provider_timeout)
    }

    async fn reusable(&self, key: SubjectKey) -> Option<Arc<Token>> {
        self.cache
            .get(&key)
            .await
            .filter(|token| token.is_reusable(Instant::now()))
    }

    fn join(&self, key: SubjectKey) -> ExchangeFlight<'_> {
        let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
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

/// Only the fetching caller owns the sender. If it drops this guard before
/// [`Self::publish`], waiters wake without an outcome and can start a new attempt.
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
        self.flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
        if let Some(result) = self.completed.take() {
            self.sender.send_replace(Some(result));
        }
    }
}

/// The SHA-256 digest of a subject's access token, keying its exchanged
/// tokens. It has no `Debug`, so the digest never reaches a log.
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

/// Retention is best-effort. Each reader checks reuse on the Tokio clock.
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

impl Credentials {
    /// Returns a live exchanged token for `subject`, exchanging it when the
    /// cache has none. Concurrent misses for one subject wait on the exchange
    /// of the first caller, bounded by that caller's `deadline`
    /// and [`FETCH_TIMEOUT`]. When that caller is dropped or out of budget,
    /// its exchange is cancelled and a waiting caller starts its own; a
    /// waiter never fails because of another caller's deadline. Exchanges
    /// for different subjects run concurrently.
    pub(crate) async fn exchange(
        &self,
        subject: &SecretString,
        deadline: AcquisitionDeadline<'_>,
    ) -> Result<Arc<Token>, AcquisitionError> {
        let key = SubjectKey::of(subject);
        self.check_admission(deadline)?;
        let result = if let Some(token) = self.0.inner.exchange.reusable(key).await {
            Ok(token)
        } else {
            tokio::time::timeout_at(
                deadline.cutoff,
                self.acquire_exchange(key, subject, deadline),
            )
            .await
            .map_err(|_| deadline.expiry)?
        };
        if let Some(error) = deadline.stopped_error() {
            return Err(error);
        }
        result
    }

    async fn acquire_exchange(
        &self,
        key: SubjectKey,
        subject: &SecretString,
        deadline: AcquisitionDeadline<'_>,
    ) -> ExchangeResult {
        let inner = &self.0.inner;
        loop {
            self.check_admission(deadline)?;
            match inner.exchange.join(key) {
                ExchangeFlight::Leader(leader) => {
                    // A previous exchange may have finished between the cache
                    // miss and this registration.
                    let result = if let Some(token) = inner.exchange.reusable(key).await {
                        Ok(token)
                    } else {
                        inner
                            .exchange
                            .fetch(&inner.endpoint, key, subject, deadline)
                            .await
                    };
                    if let Some(error) = deadline.stopped_error() {
                        return Err(error);
                    }
                    leader.publish(result.clone());
                    return result;
                }
                ExchangeFlight::Waiter(mut receiver) => {
                    if let Ok(result) = receiver.wait_for(Option::is_some).await {
                        match result.as_ref() {
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
}
