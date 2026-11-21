use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use arc_swap::{ArcSwap, Guard};
use shared_rules::oidc_jwt::REFRESH_COOLDOWN;
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::warn;

use crate::{
    DiscoveryError, JwksError,
    jwks::{AllowedAlgorithms, JwksSource, KeySet, KeySetFailure},
    provider::{PROVIDER_TIMEOUT, ProviderClient},
};

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_mins(15);
const MAX_REFRESH_SPREAD: Duration = Duration::from_secs(REFRESH_INTERVAL.as_secs() / 10);
const REFRESH_METRIC: &str = "authn_jwks_refreshes_total";
const ACQUISITION_METRIC: &str = "authn_jwks_last_successful_acquisition_timestamp_seconds";

#[derive(derive_more::Debug)]
pub(crate) enum SignatureRefresh {
    #[debug("NewerSet(..)")]
    NewerSet(Arc<KeySet>),
    NoNewerSet,
    WorkerLost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerState {
    Running,
    Stopped,
    Lost,
}

struct State {
    requested_ticket: u64,
    finished_ticket: u64,
    cooldown_started_at: Instant,
    worker: WorkerState,
}

impl State {
    fn pending(&self) -> Option<u64> {
        (self.requested_ticket > self.finished_ticket).then_some(self.requested_ticket)
    }

    fn request_fetch(&mut self) -> u64 {
        self.requested_ticket += 1;
        self.cooldown_started_at = Instant::now();
        self.requested_ticket
    }
}

#[derive(Debug)]
enum MissAdmission {
    Answer(SignatureRefresh),
    Wait(u64),
}

pub(crate) struct KeyStore {
    keys: ArcSwap<KeySet>,
    state: watch::Sender<State>,
    last_acquisition: metrics::Gauge,
}

impl KeyStore {
    pub(crate) fn new(keys: Arc<KeySet>) -> Arc<Self> {
        metrics::describe_counter!(REFRESH_METRIC, "JWKS refresh outcomes by closed reason");
        let keys = ArcSwap::new(keys);
        let state = watch::Sender::new(State {
            requested_ticket: 0,
            finished_ticket: 0,
            cooldown_started_at: Instant::now(),
            worker: WorkerState::Running,
        });
        metrics::describe_gauge!(
            ACQUISITION_METRIC,
            metrics::Unit::Seconds,
            "Unix timestamp of the last successfully acquired JWKS set"
        );
        let last_acquisition = metrics::gauge!(ACQUISITION_METRIC);
        last_acquisition.set(acquisition_timestamp(SystemTime::now()));
        Arc::new(Self {
            keys,
            state,
            last_acquisition,
        })
    }

    pub(crate) fn current(&self) -> Guard<Arc<KeySet>> {
        self.keys.load()
    }

    fn newer_set(&self, checked: &Arc<KeySet>) -> Option<Arc<KeySet>> {
        let installed = self.keys.load();
        (!Arc::ptr_eq(&installed, checked)).then(|| Arc::clone(&installed))
    }

    fn inspect_signature_miss(
        &self,
        state: &State,
        checked: &Arc<KeySet>,
    ) -> Option<MissAdmission> {
        if state.worker == WorkerState::Stopped {
            return Some(MissAdmission::Answer(SignatureRefresh::NoNewerSet));
        }
        if let Some(keys) = self.newer_set(checked) {
            return Some(MissAdmission::Answer(SignatureRefresh::NewerSet(keys)));
        }
        if state.worker == WorkerState::Lost {
            return Some(MissAdmission::Answer(SignatureRefresh::WorkerLost));
        }
        if let Some(ticket) = state.pending() {
            return Some(MissAdmission::Wait(ticket));
        }
        if state.cooldown_started_at.elapsed() < REFRESH_COOLDOWN {
            return Some(MissAdmission::Answer(SignatureRefresh::NoNewerSet));
        }
        None
    }

    fn admit_signature_miss(&self, checked: &Arc<KeySet>) -> MissAdmission {
        let fast_path = {
            let state = self.state.borrow();
            self.inspect_signature_miss(&state, checked)
        };
        if let Some(admission) = fast_path {
            return admission;
        }
        let mut admission = MissAdmission::Answer(SignatureRefresh::NoNewerSet);
        self.state.send_if_modified(|state| {
            if let Some(decided) = self.inspect_signature_miss(state, checked) {
                admission = decided;
                return false;
            }
            admission = MissAdmission::Wait(state.request_fetch());
            true
        });
        admission
    }

    pub(crate) async fn refresh_for_signature_miss(
        &self,
        checked: &Arc<KeySet>,
    ) -> SignatureRefresh {
        let ticket = match self.admit_signature_miss(checked) {
            MissAdmission::Answer(answer) => return answer,
            MissAdmission::Wait(ticket) => ticket,
        };
        let mut state_changes = self.state.subscribe();
        let worker = state_changes
            .wait_for(|state| {
                state.worker != WorkerState::Running || state.finished_ticket >= ticket
            })
            .await
            .map(|state| state.worker);
        match worker {
            Ok(WorkerState::Lost) => SignatureRefresh::WorkerLost,
            Ok(WorkerState::Stopped) | Err(_) => SignatureRefresh::NoNewerSet,
            Ok(WorkerState::Running) => self
                .newer_set(checked)
                .map_or(SignatureRefresh::NoNewerSet, SignatureRefresh::NewerSet),
        }
    }

    fn request_periodic(&self) {
        self.state.send_if_modified(|state| {
            if state.pending().is_some() {
                return false;
            }
            state.request_fetch();
            true
        });
    }

    pub(crate) fn pending(&self) -> Option<u64> {
        self.state.borrow().pending()
    }

    pub(crate) fn finish(&self, ticket: u64, replacement: Result<Arc<KeySet>, KeySetFailure>) {
        let (result, reason) = match &replacement {
            Ok(_) => ("success", "success"),
            Err(failure) => {
                let reason = failure.label();
                match failure {
                    KeySetFailure::Discovery(DiscoveryError::Fetch(cause)) => {
                        warn!(reason, ?cause, "authn_jwks_refresh_failed");
                    }
                    KeySetFailure::Discovery(error) => {
                        warn!(
                            reason,
                            cause = error.reason_label(),
                            "authn_jwks_refresh_failed"
                        );
                    }
                    KeySetFailure::DiscoveryTimeout => {
                        warn!(reason, cause = "timeout", "authn_jwks_refresh_failed");
                    }
                    KeySetFailure::Jwks(JwksError::Fetch(cause)) => {
                        warn!(reason, ?cause, "authn_jwks_refresh_failed");
                    }
                    KeySetFailure::Jwks(JwksError::Parse | JwksError::NoUsableKeys) => {
                        warn!(reason, "authn_jwks_refresh_failed");
                    }
                }
                ("failure", reason)
            }
        };
        let mut previous = None;
        self.state.send_modify(|state| {
            if let Ok(keys) = replacement {
                previous = Some(self.keys.swap(keys));
                self.last_acquisition
                    .set(acquisition_timestamp(SystemTime::now()));
            }
            state.finished_ticket = ticket;
        });
        drop(previous);
        metrics::counter!(REFRESH_METRIC, "result" => result, "reason" => reason).increment(1);
    }

    fn stop(&self) {
        self.state
            .send_modify(|state| state.worker = WorkerState::Stopped);
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "std::mem::DropGuard invokes its drop callback with owned Arc<KeyStore>"
    )]
    fn mark_lost(store: Arc<Self>) {
        store.state.send_if_modified(|state| {
            if state.worker != WorkerState::Running {
                return false;
            }
            state.worker = WorkerState::Lost;
            true
        });
    }

    #[cfg(test)]
    pub(crate) fn expire_refresh_cooldown_for_test(&self) {
        self.state.send_modify(|state| {
            state.cooldown_started_at = Instant::now() - REFRESH_COOLDOWN - Duration::from_secs(1);
        });
    }
}

fn acquisition_timestamp(now: SystemTime) -> f64 {
    match now.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs_f64(),
        Err(before_epoch) => -before_epoch.duration().as_secs_f64(),
    }
}

#[must_use = "drive and await RefreshDriver::run for the verifier lifetime"]
#[derive(derive_more::Debug)]
#[debug("RefreshDriver([REDACTED])")]
pub struct RefreshDriver {
    store: std::mem::DropGuard<Arc<KeyStore>, fn(Arc<KeyStore>)>,
    provider: ProviderClient,
    source: JwksSource,
    algorithms: AllowedAlgorithms,
    jwks_max_age: Duration,
}

impl RefreshDriver {
    pub(crate) fn new(
        store: Arc<KeyStore>,
        provider: ProviderClient,
        source: JwksSource,
        algorithms: AllowedAlgorithms,
        jwks_max_age: Duration,
    ) -> Self {
        Self {
            store: std::mem::DropGuard::new(store, KeyStore::mark_lost),
            provider,
            source,
            algorithms,
            jwks_max_age,
        }
    }

    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        let Self {
            store,
            provider,
            source,
            algorithms,
            jwks_max_age,
        } = self;
        let next_period = move || refresh_period().min(jwks_max_age / 2);
        let mut next_refresh = Instant::now() + next_period();
        let mut state_changes = store.state.subscribe();
        let mut shutdown = std::pin::pin!(shutdown);
        'worker: loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                _ = state_changes.wait_for(|state| state.pending().is_some()) => {}
                () = tokio::time::sleep_until(next_refresh) => {
                    store.request_periodic();
                    next_refresh = Instant::now() + next_period();
                }
            }
            while let Some(ticket) = store.pending() {
                let replacement = tokio::select! {
                    biased;
                    () = &mut shutdown => break 'worker,
                    replacement = source.fetch(&provider, algorithms, Instant::now() + PROVIDER_TIMEOUT) => replacement,
                };
                store.finish(ticket, replacement);
            }
        }
        std::mem::DropGuard::dismiss(store).stop();
    }
}

fn refresh_period() -> Duration {
    let mut bytes = [0_u8; 2];
    let sample = aws_lc_rs::rand::fill(&mut bytes).map(|()| u16::from_be_bytes(bytes));
    refresh_period_for_sample(sample)
}

fn refresh_period_for_sample(sample: Result<u16, aws_lc_rs::error::Unspecified>) -> Duration {
    let spread = sample.map_or(Duration::ZERO, |sample| {
        shared_rules::jitter::share_of_window(MAX_REFRESH_SPREAD, sample)
    });
    REFRESH_INTERVAL.saturating_sub(spread)
}

#[cfg(test)]
mod tests {
    use super::{Guard, KeySetFailure, KeyStore, SignatureRefresh, acquisition_timestamp};
    use crate::{
        jwks::{
            KeySet,
            tests::{jwk, rsa_signing},
        },
        test_metrics::Diagnostics,
    };
    use jsonwebtoken::Algorithm;
    use std::assert_matches;
    use std::{
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Waker},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use tokio_util::sync::CancellationToken;
    #[test]
    fn acquisition_time_retains_fractional_seconds_on_both_sides_of_epoch() {
        for (clock, expected) in [
            (UNIX_EPOCH, 0.0_f64),
            (UNIX_EPOCH + Duration::from_millis(1_250), 1.25),
            (UNIX_EPOCH - Duration::from_millis(250), -0.25),
            (UNIX_EPOCH - Duration::from_millis(1_250), -1.25),
        ] {
            assert_eq!(acquisition_timestamp(clock).to_bits(), expected.to_bits());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn only_admitted_sets_sample_acquisition_time_including_unchanged_keys() {
        let diagnostics = Diagnostics::default();
        let keys = key_set("old");
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let store =
            metrics::with_local_recorder(diagnostics.recorder(), || KeyStore::new(keys.clone()));
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let samples = diagnostics.acquisitions();
        assert_eq!(samples.len(), 1);
        assert!((before.min(after)..=before.max(after)).contains(&samples[0]));
        assert_matches!(
            store.refresh_for_signature_miss(&keys).await,
            SignatureRefresh::NoNewerSet
        );

        assert_eq!(diagnostics.acquisitions().len(), 1);

        for failure in [
            KeySetFailure::Jwks(crate::JwksError::Fetch(crate::ProviderFailure::Timeout)),
            KeySetFailure::Jwks(crate::JwksError::Parse),
            KeySetFailure::Jwks(crate::JwksError::NoUsableKeys),
        ] {
            store.request_periodic();
            store.finish(store.pending().unwrap(), Err(failure));
            assert!(Arc::ptr_eq(&store.current(), &keys));
            assert_eq!(diagnostics.acquisitions(), samples);
        }

        store.request_periodic();
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        store.finish(store.pending().unwrap(), Ok(key_set("old")));
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let samples = diagnostics.acquisitions();
        assert_eq!(samples.len(), 2);
        assert!((before.min(after)..=before.max(after)).contains(&samples[1]));
        assert!(store.current().has_kid("old"));
    }

    #[tokio::test]
    async fn cancelling_an_inflight_worker_fetch_preserves_acquisition_time_and_keys() {
        let diagnostics = Diagnostics::default();
        let keys = key_set("old");
        let store =
            metrics::with_local_recorder(diagnostics.recorder(), || KeyStore::new(keys.clone()));
        let initial = diagnostics.acquisitions();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_, root) = crate::provider::fixture_acceptor("jwks.test");
        let cancel = tokio_util::sync::CancellationToken::new();
        let provider =
            crate::provider::new_fixture_client("jwks.test", address, &root, cancel.clone())
                .unwrap();
        let endpoint =
            crate::EndpointUrl::parse(&format!("https://jwks.test:{}/keys", address.port()))
                .unwrap();
        store.request_periodic();
        let driver = super::RefreshDriver::new(
            store.clone(),
            provider,
            super::JwksSource::Configured(endpoint),
            crate::jwks::allowed_algorithms(&[crate::JwtAlgorithm::Rs256]),
            Duration::from_hours(1),
        );
        let worker = tokio::spawn(driver.run(cancel.clone().cancelled_owned()));
        let (_socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(diagnostics.acquisitions(), initial);
        assert!(Arc::ptr_eq(&store.current(), &keys));
        assert_matches!(
            store.refresh_for_signature_miss(&keys).await,
            SignatureRefresh::NoNewerSet
        );
    }

    fn key_set(kid: &str) -> Arc<KeySet> {
        crate::jwks::tests::key_set(
            &[jwk(&rsa_signing(), Algorithm::RS256, Some(kid), true)],
            &[crate::JwtAlgorithm::Rs256],
        )
    }

    fn spawn_refresh(store: &Arc<KeyStore>) -> tokio::task::JoinHandle<Option<bool>> {
        let store = Arc::clone(store);
        tokio::spawn(async move {
            let checked = Guard::into_inner(store.current());
            match store.refresh_for_signature_miss(&checked).await {
                SignatureRefresh::NewerSet(keys) => Some(keys.has_kid("new")),
                SignatureRefresh::NoNewerSet => None,
                SignatureRefresh::WorkerLost => panic!("worker unexpectedly dropped"),
            }
        })
    }

    #[test]
    fn periodic_spread_stays_bounded_and_rng_failure_preserves_the_original_wait() {
        for (sample, expected) in [
            (Ok(0), Duration::from_mins(15)),
            (Ok(u16::MAX), Duration::from_secs(810)),
            (Err(aws_lc_rs::error::Unspecified), Duration::from_mins(15)),
        ] {
            assert_eq!(super::refresh_period_for_sample(sample), expected);
        }
    }

    fn worker_with_unavailable_provider(
        store: &Arc<KeyStore>,
        jwks_max_age: Duration,
    ) -> super::RefreshDriver {
        let host = "authn.fixture.test";
        let material = tls_test_support::TlsMaterial::new(host);
        let deny_dns = CancellationToken::new();
        deny_dns.cancel();
        let provider = crate::provider::new_fixture_client(
            host,
            "127.0.0.1:443".parse().unwrap(),
            &material.root,
            deny_dns,
        )
        .unwrap();
        super::RefreshDriver::new(
            store.clone(),
            provider,
            super::JwksSource::Configured(
                crate::EndpointUrl::parse("https://authn.fixture.test/keys").unwrap(),
            ),
            crate::jwks::allowed_algorithms(&[crate::JwtAlgorithm::Rs256]),
            jwks_max_age,
        )
    }

    async fn poll_waiting_worker(mut worker: Pin<&mut impl Future<Output = ()>>) {
        assert!(futures_util::poll!(worker.as_mut()).is_pending());
    }

    async fn finish_worker_fetch(
        worker: Pin<&mut impl Future<Output = ()>>,
        store: &KeyStore,
        ticket: u64,
    ) {
        let mut state = store.state.subscribe();
        tokio::select! {
            biased;
            () = worker => panic!("refresh worker stopped unexpectedly"),
            result = tokio::time::timeout(
                Duration::from_secs(1),
                state.wait_for(|state| state.finished_ticket >= ticket),
            ) => {
                assert_eq!(result.unwrap().unwrap().finished_ticket, ticket);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_key_work_does_not_postpone_initial_or_subsequent_periods() {
        let store = KeyStore::new(key_set("old"));
        let cancel = CancellationToken::new();
        let mut worker = std::pin::pin!(
            worker_with_unavailable_provider(&store, Duration::from_hours(1))
                .run(cancel.cancelled())
        );
        poll_waiting_worker(worker.as_mut()).await;
        for period in 0..2 {
            let started = tokio::time::Instant::now();
            tokio::time::advance(Duration::from_secs(809)).await;
            poll_waiting_worker(worker.as_mut()).await;
            assert_eq!(store.state.borrow().requested_ticket, period * 2);

            let checked = Guard::into_inner(store.current());
            let mut unknown = std::pin::pin!(store.refresh_for_signature_miss(&checked));
            assert!(futures_util::poll!(unknown.as_mut()).is_pending());
            finish_worker_fetch(worker.as_mut(), &store, period * 2 + 1).await;
            assert_matches!(unknown.await, SignatureRefresh::NoNewerSet);

            let due = started + Duration::from_secs(901);
            tokio::time::advance(due - tokio::time::Instant::now()).await;
            finish_worker_fetch(worker.as_mut(), &store, period * 2 + 2).await;
            assert!(store.current().has_kid("old"));
        }
        cancel.cancel();
        worker.await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_overdue_period_rearms_from_observed_time_without_a_backlog() {
        let store = KeyStore::new(key_set("old"));
        let cancel = CancellationToken::new();
        let mut worker = std::pin::pin!(
            worker_with_unavailable_provider(&store, Duration::from_hours(1))
                .run(cancel.cancelled())
        );
        poll_waiting_worker(worker.as_mut()).await;
        tokio::time::advance(Duration::from_secs(4_000)).await;
        finish_worker_fetch(worker.as_mut(), &store, 1).await;
        poll_waiting_worker(worker.as_mut()).await;
        assert_eq!(store.state.borrow().requested_ticket, 1);
        tokio::time::advance(Duration::from_secs(809)).await;
        poll_waiting_worker(worker.as_mut()).await;
        assert_eq!(store.state.borrow().requested_ticket, 1);
        tokio::time::advance(Duration::from_secs(92)).await;
        finish_worker_fetch(worker.as_mut(), &store, 2).await;
        tokio::time::advance(Duration::from_secs(1_000)).await;
        cancel.cancel();
        worker.await;
        assert_eq!(store.state.borrow().requested_ticket, 2);
        assert_eq!(store.state.borrow().worker, super::WorkerState::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_share_one_fetch_and_see_its_keys() {
        let store = KeyStore::new(key_set("old"));
        store.expire_refresh_cooldown_for_test();
        let first = spawn_refresh(&store);
        tokio::task::yield_now().await;
        let second = spawn_refresh(&store);
        tokio::task::yield_now().await;
        assert_eq!(store.pending(), Some(1));
        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        store.finish(1, Ok(key_set("new")));
        assert_eq!(first.await.unwrap(), Some(true));
        assert_eq!(second.await.unwrap(), Some(true));
        assert!(store.current().has_kid("new"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_short_freshness_window_renews_before_the_normal_refresh_period() {
        let store = KeyStore::new(key_set("old"));
        let cancel = CancellationToken::new();
        let mut worker = std::pin::pin!(
            worker_with_unavailable_provider(&store, Duration::from_mins(1))
                .run(cancel.cancelled())
        );
        poll_waiting_worker(worker.as_mut()).await;
        tokio::time::advance(Duration::from_secs(30)).await;
        finish_worker_fetch(worker.as_mut(), &store, 1).await;
        cancel.cancel();
        worker.await;
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_a_waiter_does_not_cancel_the_shared_fetch() {
        let diagnostics = Diagnostics::default();
        let store =
            metrics::with_local_recorder(diagnostics.recorder(), || KeyStore::new(key_set("old")));
        let initial = diagnostics.acquisitions();
        store.expire_refresh_cooldown_for_test();
        let first = spawn_refresh(&store);
        tokio::task::yield_now().await;
        let second = spawn_refresh(&store);
        tokio::task::yield_now().await;
        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        assert_eq!(diagnostics.acquisitions(), initial);
        store.finish(1, Ok(key_set("new")));
        assert_eq!(first.await.unwrap(), Some(true));
        assert_eq!(diagnostics.acquisitions().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_fetch_offers_no_newer_set_to_its_waiter_or_during_its_cooldown() {
        let store = KeyStore::new(key_set("old"));
        assert_eq!(spawn_refresh(&store).await.unwrap(), None);
        store.expire_refresh_cooldown_for_test();
        let waiter = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(
            1,
            Err(super::KeySetFailure::Jwks(crate::JwksError::Fetch(
                crate::ProviderFailure::Timeout,
            ))),
        );
        assert_eq!(waiter.await.unwrap(), None);
        assert_eq!(spawn_refresh(&store).await.unwrap(), None);
        assert_eq!(store.pending(), None);
        assert!(store.current().has_kid("old"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_gets_its_fetch_set_when_a_later_fetch_fails_before_it_is_polled() {
        let store = KeyStore::new(key_set("old"));
        store.expire_refresh_cooldown_for_test();
        let waiter = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(1, Ok(key_set("new")));
        store.request_periodic();
        store.finish(
            2,
            Err(super::KeySetFailure::Jwks(crate::JwksError::Fetch(
                crate::ProviderFailure::Timeout,
            ))),
        );
        assert_eq!(waiter.await.unwrap(), Some(true));
    }

    #[tokio::test(start_paused = true)]
    async fn a_set_installed_after_the_token_was_checked_is_the_answer_without_a_fetch() {
        let store = KeyStore::new(key_set("old"));
        let checked = Guard::into_inner(store.current());
        store.expire_refresh_cooldown_for_test();
        let rotation = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(1, Ok(key_set("new")));
        assert_eq!(rotation.await.unwrap(), Some(true));
        let SignatureRefresh::NewerSet(keys) = store.refresh_for_signature_miss(&checked).await
        else {
            panic!("the newer installed set must be offered");
        };
        assert!(keys.has_kid("new"));
        assert_eq!(store.pending(), None);
        assert_matches!(
            store.refresh_for_signature_miss(&keys).await,
            SignatureRefresh::NoNewerSet
        );

        store.request_periodic();
        let during_fetch = std::pin::pin!(store.refresh_for_signature_miss(&checked))
            .poll(&mut Context::from_waker(Waker::noop()));
        store.finish(
            2,
            Err(KeySetFailure::Jwks(crate::JwksError::Fetch(
                crate::ProviderFailure::Timeout,
            ))),
        );
        let after_failure = store.refresh_for_signature_miss(&checked).await;

        assert_matches!(during_fetch, Poll::Ready(SignatureRefresh::NewerSet(keys)) if keys.has_kid("new"));
        assert_matches!(after_failure, SignatureRefresh::NewerSet(keys) if keys.has_kid("new"));
    }

    #[tokio::test]
    async fn stopping_releases_current_and_future_waiters() {
        let store = KeyStore::new(key_set("old"));
        store.expire_refresh_cooldown_for_test();
        let waiter = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.stop();
        assert_eq!(waiter.await.unwrap(), None);
        assert_eq!(spawn_refresh(&store).await.unwrap(), None);
    }

    #[tokio::test]
    async fn dropping_the_refresh_driver_marks_the_worker_lost_for_current_and_future_waiters() {
        let store = KeyStore::new(key_set("old"));
        let checked = Guard::into_inner(store.current());
        let driver = worker_with_unavailable_provider(&store, Duration::from_hours(1));
        store.expire_refresh_cooldown_for_test();
        let mut waiter = std::pin::pin!(store.refresh_for_signature_miss(&checked));
        assert!(futures_util::poll!(waiter.as_mut()).is_pending());
        drop(driver);
        assert_matches!(waiter.await, SignatureRefresh::WorkerLost);
        assert_matches!(
            store.refresh_for_signature_miss(&checked).await,
            SignatureRefresh::WorkerLost
        );
        assert_eq!(store.state.borrow().worker, super::WorkerState::Lost);
    }

    #[tokio::test(start_paused = true)]
    async fn read_only_miss_admissions_never_wait_for_a_write_lock() {
        let admit_under_read_lock = |store: &Arc<KeyStore>, checked: &Arc<KeySet>| {
            let reader = store.state.borrow();
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            let handle = tokio::runtime::Handle::current();
            let probe = {
                let store = Arc::clone(store);
                let checked = Arc::clone(checked);
                std::thread::spawn(move || {
                    let _entered = handle.enter();
                    let _ = done_tx.send(store.admit_signature_miss(&checked));
                })
            };
            let result = done_rx.recv_timeout(Duration::from_secs(2));
            drop(reader);
            probe.join().unwrap();
            result.expect("read-only miss admission must not acquire a write lock")
        };

        let store = KeyStore::new(key_set("old"));
        let checked = Guard::into_inner(store.current());
        assert_matches!(
            admit_under_read_lock(&store, &checked),
            super::MissAdmission::Answer(SignatureRefresh::NoNewerSet)
        );

        store.expire_refresh_cooldown_for_test();
        assert_matches!(
            store.admit_signature_miss(&checked),
            super::MissAdmission::Wait(1)
        );
        assert_matches!(
            admit_under_read_lock(&store, &checked),
            super::MissAdmission::Wait(1)
        );

        store.finish(1, Ok(key_set("new")));
        assert_matches!(
            admit_under_read_lock(&store, &checked),
            super::MissAdmission::Answer(SignatureRefresh::NewerSet(keys)) if keys.has_kid("new")
        );

        store.stop();
        assert_matches!(
            admit_under_read_lock(&store, &checked),
            super::MissAdmission::Answer(SignatureRefresh::NoNewerSet)
        );

        let lost_store = KeyStore::new(key_set("old"));
        let lost_checked = Guard::into_inner(lost_store.current());
        let driver = worker_with_unavailable_provider(&lost_store, Duration::from_hours(1));
        drop(driver);
        assert_matches!(
            admit_under_read_lock(&lost_store, &lost_checked),
            super::MissAdmission::Answer(SignatureRefresh::WorkerLost)
        );
    }
}
