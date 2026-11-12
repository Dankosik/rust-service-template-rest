//! Process-owned JWKS refresh shared by every request.
//!
//! One worker performs every fetch, so a request that stops waiting never
//! cancels the fetch other requests wait for.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    EndpointUrl, JwtAlgorithm,
    jwks::{KeySet, KeySetFailure, fetch_key_set},
    provider::ProviderClient,
};

pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_mins(15);
/// Up to 10% earlier, so replicas started together do not fetch in lockstep.
const MAX_REFRESH_SPREAD: Duration = Duration::from_secs(REFRESH_INTERVAL.as_secs() / 10);
/// A caller can force a signature miss. Each verifier starts at most one
/// request-triggered fetch per cooldown. A miss during cooldown can join a
/// pending fetch. If none is pending, another miss after cooldown or the next
/// periodic fetch can pick up the rotation.
///
/// Cost: a cooldown miss cannot tell a forged `kid` from a freshly rotated one,
/// so for up to this long after a fetch starts (startup included) a valid token
/// signed with a new key gets a non-retryable 401 `invalid_token`. Answering
/// 503 instead would let any caller turn forged `kid` values into unavailability
/// signals and client retries; the rotation's own later traffic triggers the
/// fetch once the cooldown ends.
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const REFRESH_METRIC: &str = "authn_jwks_refreshes_total";
const ACQUISITION_METRIC: &str = "authn_jwks_last_successful_acquisition_timestamp_seconds";

#[derive(derive_more::Debug)]
pub(crate) enum SignatureRefresh {
    /// A newer set than the one the token was checked against is installed:
    /// by a fetch that finished after the request, or by one that finished
    /// between that check and the request.
    #[debug("Refreshed(..)")]
    NewerSet(Arc<KeySet>),
    /// The last acquisition succeeded and left the caller's checked set installed.
    /// Another fetch is still blocked by the cooldown measured from refresh start.
    NoNewerSet,
    Unavailable,
}

#[derive(Clone)]
struct State {
    keys: Arc<KeySet>,
    /// Fetches asked for so far; each is a ticket that waiters compare against.
    requested: u64,
    /// The latest finished ticket, successful or not.
    finished: u64,
    /// The latest ticket whose fetch installed a set; the startup set is 0. A
    /// waiter compares it with its own ticket because a later failure may
    /// finish before the waiter is polled.
    succeeded: u64,
    last_started: Instant,
    stopped: bool,
}

impl State {
    /// The ticket of the fetch asked for but not yet finished.
    fn pending(&self) -> Option<u64> {
        (self.requested > self.finished).then_some(self.requested)
    }

    fn last_fetch_failed(&self) -> bool {
        self.succeeded != self.finished
    }

    fn start_fetch(&mut self) -> u64 {
        self.requested += 1;
        self.last_started = Instant::now();
        self.requested
    }
}

/// How a signature miss is admitted under the watch write lock.
enum Admission {
    Answer(SignatureRefresh),
    Join(u64),
    Start(u64),
}

pub(crate) struct KeyStore {
    state: watch::Sender<State>,
    wake: Notify,
    last_acquisition: metrics::Gauge,
}

impl KeyStore {
    /// Installs the startup key set and starts the first cooldown.
    pub(crate) fn new(keys: Arc<KeySet>) -> Arc<Self> {
        metrics::describe_counter!(REFRESH_METRIC, "JWKS refresh outcomes by closed reason");
        let state = watch::Sender::new(State {
            keys,
            requested: 0,
            finished: 0,
            succeeded: 0,
            last_started: Instant::now(),
            stopped: false,
        });
        metrics::describe_gauge!(
            ACQUISITION_METRIC,
            metrics::Unit::Seconds,
            "Unix timestamp of the last successfully acquired usable JWKS set"
        );
        let last_acquisition = metrics::gauge!(ACQUISITION_METRIC);
        last_acquisition.set(acquisition_timestamp(SystemTime::now()));
        Arc::new(Self {
            state,
            wake: Notify::new(),
            last_acquisition,
        })
    }

    pub(crate) fn current(&self) -> Arc<KeySet> {
        self.state.borrow().keys.clone()
    }

    /// The generation of the installed set, read without cloning it.
    pub(crate) fn generation(&self) -> u64 {
        self.state.borrow().keys.generation()
    }

    /// Returns a set installed since `checked`, otherwise joins the fetch in
    /// flight or starts one outside the cooldown.
    pub(crate) async fn refresh_for_signature_miss(
        &self,
        checked: &Arc<KeySet>,
    ) -> SignatureRefresh {
        let mut admission = Admission::Answer(SignatureRefresh::Unavailable);
        // Inspect the cooldown and claim or join a ticket under the same watch
        // write lock; only a started fetch modifies the state.
        self.state.send_if_modified(|state| {
            admission = if state.stopped {
                Admission::Answer(SignatureRefresh::Unavailable)
            } else if !Arc::ptr_eq(&state.keys, checked) {
                Admission::Answer(SignatureRefresh::NewerSet(state.keys.clone()))
            } else if let Some(ticket) = state.pending() {
                Admission::Join(ticket)
            } else if state.last_started.elapsed() < REFRESH_COOLDOWN {
                Admission::Answer(if state.last_fetch_failed() {
                    SignatureRefresh::Unavailable
                } else {
                    SignatureRefresh::NoNewerSet
                })
            } else {
                Admission::Start(state.start_fetch())
            };
            matches!(admission, Admission::Start(_))
        });
        let ticket = match admission {
            Admission::Answer(answer) => return answer,
            Admission::Join(ticket) => ticket,
            Admission::Start(ticket) => {
                self.wake.notify_one();
                ticket
            }
        };
        // wait_for checks the current snapshot too, covering a fetch that finished
        // before this subscription. Dropping this waiter leaves the worker running.
        let mut state = self.state.subscribe();
        match state
            .wait_for(|state| state.stopped || state.finished >= ticket)
            .await
        {
            Ok(state) if !state.stopped && state.succeeded >= ticket => {
                SignatureRefresh::NewerSet(state.keys.clone())
            }
            _ => SignatureRefresh::Unavailable,
        }
    }

    fn request_periodic(&self) {
        self.state.send_if_modified(|state| {
            if state.pending().is_some() {
                return false;
            }
            state.start_fetch();
            true
        });
    }

    pub(crate) fn pending(&self) -> Option<u64> {
        self.state.borrow().pending()
    }

    /// Records a finished fetch; every ticket up to `ticket` is served by it.
    pub(crate) fn finish(&self, ticket: u64, replacement: Result<Arc<KeySet>, KeySetFailure>) {
        let (result, reason) = match &replacement {
            Ok(_) => ("success", "success"),
            Err(failure) => {
                let reason = failure.label();
                if let KeySetFailure::Fetch(cause) = failure {
                    warn!(reason, ?cause, "authn_jwks_refresh_failed");
                } else {
                    warn!(reason, "authn_jwks_refresh_failed");
                }
                ("failure", reason)
            }
        };
        self.state.send_modify(|state| {
            if let Ok(keys) = replacement {
                state.keys = keys;
                state.succeeded = ticket;
                self.last_acquisition
                    .set(acquisition_timestamp(SystemTime::now()));
            }
            state.finished = ticket;
        });
        metrics::counter!(REFRESH_METRIC, "result" => result, "reason" => reason).increment(1);
    }

    fn stop(&self) {
        self.state.send_modify(|state| state.stopped = true);
    }

    #[cfg(test)]
    pub(crate) fn permit_unknown_refresh_for_test(&self) {
        self.state.send_modify(|state| {
            state.last_started = Instant::now() - REFRESH_COOLDOWN - Duration::from_secs(1);
        });
    }
}

fn acquisition_timestamp(now: SystemTime) -> f64 {
    match now.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_secs_f64(),
        Err(before_epoch) => -before_epoch.duration().as_secs_f64(),
    }
}

pub(crate) async fn run_refresh_worker(
    store: Arc<KeyStore>,
    provider: ProviderClient,
    jwks_uri: EndpointUrl,
    algorithms: Vec<JwtAlgorithm>,
    cancel: CancellationToken,
) {
    let mut next_refresh = Instant::now() + refresh_period();
    'worker: loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = store.wake.notified() => {}
            () = tokio::time::sleep_until(next_refresh) => {
                store.request_periodic();
                next_refresh = Instant::now() + refresh_period();
            }
        }
        while let Some(ticket) = store.pending() {
            let replacement = tokio::select! {
                biased;
                () = cancel.cancelled() => break 'worker,
                replacement = fetch_key_set(&provider, &jwks_uri, &algorithms, None) => replacement,
            };
            store.finish(ticket, replacement);
        }
    }
    store.stop();
}

fn refresh_period() -> Duration {
    let mut bytes = [0_u8; 2];
    let sample = aws_lc_rs::rand::fill(&mut bytes).map(|()| u16::from_be_bytes(bytes));
    refresh_period_for_sample(sample)
}

fn refresh_period_for_sample(sample: Result<u16, aws_lc_rs::error::Unspecified>) -> Duration {
    // A failed random sample only drops the spread; refresh keeps the full interval.
    let spread = sample.map_or(Duration::ZERO, |sample| {
        shared_rules::jitter::share_of_window(MAX_REFRESH_SPREAD, sample)
    });
    REFRESH_INTERVAL.saturating_sub(spread)
}

#[cfg(test)]
mod tests {
    use super::{KeySetFailure, KeyStore, SignatureRefresh, acquisition_timestamp};
    use crate::{
        jwks::{
            KeySet,
            tests::{jwk, rsa_signing},
        },
        test_metrics::Diagnostics,
    };
    use futures_util::FutureExt as _;
    use jsonwebtoken::Algorithm;
    use std::assert_matches;
    use std::{
        future::Future,
        pin::Pin,
        sync::Arc,
        task::Poll,
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
        assert!(diagnostics.acquisitions.lock().unwrap().is_empty());
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let store = metrics::with_local_recorder(&diagnostics, || KeyStore::new(keys.clone()));
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let samples = diagnostics.acquisitions.lock().unwrap().clone();
        assert_eq!(samples.len(), 1);
        assert!((before.min(after)..=before.max(after)).contains(&samples[0]));
        assert_matches!(
            store.refresh_for_signature_miss(&keys).await,
            SignatureRefresh::NoNewerSet
        );

        assert_eq!(diagnostics.acquisitions.lock().unwrap().len(), 1);

        for failure in [
            KeySetFailure::Fetch(crate::ProviderFailure::Timeout),
            KeySetFailure::Parse,
            KeySetFailure::NoUsableKeys,
        ] {
            store.request_periodic();
            store.finish(store.pending().unwrap(), Err(failure));
            assert!(Arc::ptr_eq(&store.current(), &keys));
            assert_eq!(*diagnostics.acquisitions.lock().unwrap(), samples);
        }

        // No local recorder is installed here: the owner must retain its handle.
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
        let samples = diagnostics.acquisitions.lock().unwrap();
        assert_eq!(samples.len(), 2);
        assert!((before.min(after)..=before.max(after)).contains(&samples[1]));
        assert!(store.current().has_kid("old"));
    }

    #[tokio::test]
    async fn cancelling_an_inflight_worker_fetch_preserves_acquisition_time_and_keys() {
        let diagnostics = Diagnostics::default();
        let keys = key_set("old");
        let store = metrics::with_local_recorder(&diagnostics, || KeyStore::new(keys.clone()));
        let initial = diagnostics.acquisitions.lock().unwrap().clone();
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
        store.wake.notify_one();
        let worker = tokio::spawn(super::run_refresh_worker(
            store.clone(),
            provider,
            endpoint,
            vec![crate::JwtAlgorithm::Rs256],
            cancel.clone(),
        ));
        // Hold the accepted socket without completing TLS so the real fetch stays pending.
        let (_socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*diagnostics.acquisitions.lock().unwrap(), initial);
        assert!(Arc::ptr_eq(&store.current(), &keys));
        assert_matches!(
            store.refresh_for_signature_miss(&keys).await,
            SignatureRefresh::Unavailable
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
            match store.refresh_for_signature_miss(&store.current()).await {
                SignatureRefresh::NewerSet(keys) => Some(keys.has_kid("new")),
                SignatureRefresh::NoNewerSet => None,
                SignatureRefresh::Unavailable => Some(false),
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
        cancel: &CancellationToken,
    ) -> impl Future<Output = ()> + use<> {
        let host = "authn.fixture.test";
        let material = crate::tls::TlsMaterial::new(host);
        let deny_dns = CancellationToken::new();
        deny_dns.cancel();
        let provider = crate::provider::new_fixture_client(
            host,
            "127.0.0.1:443".parse().unwrap(),
            &material.root,
            deny_dns,
        )
        .unwrap();
        super::run_refresh_worker(
            store.clone(),
            provider,
            crate::EndpointUrl::parse("https://authn.fixture.test/keys").unwrap(),
            vec![crate::JwtAlgorithm::Rs256],
            cancel.clone(),
        )
    }

    async fn poll_waiting_worker(mut worker: Pin<&mut impl Future<Output = ()>>) {
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(worker.as_mut().poll(cx)))
                .await
                .is_pending()
        );
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
                state.wait_for(|state| state.finished >= ticket),
            ) => {
                assert_eq!(result.unwrap().unwrap().finished, ticket);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_key_work_does_not_postpone_initial_or_subsequent_periods() {
        let store = KeyStore::new(key_set("old"));
        let cancel = CancellationToken::new();
        let mut worker = std::pin::pin!(worker_with_unavailable_provider(&store, &cancel));
        poll_waiting_worker(worker.as_mut()).await;
        for period in 0..2 {
            let started = tokio::time::Instant::now();
            tokio::time::advance(Duration::from_secs(809)).await;
            poll_waiting_worker(worker.as_mut()).await;
            assert_eq!(store.state.borrow().requested, period * 2);

            let checked = store.current();
            let mut unknown = std::pin::pin!(store.refresh_for_signature_miss(&checked));
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(unknown.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            finish_worker_fetch(worker.as_mut(), &store, period * 2 + 1).await;
            assert_matches!(unknown.await, SignatureRefresh::Unavailable);

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
        let mut worker = std::pin::pin!(worker_with_unavailable_provider(&store, &cancel));
        poll_waiting_worker(worker.as_mut()).await;
        tokio::time::advance(Duration::from_secs(4_000)).await;
        finish_worker_fetch(worker.as_mut(), &store, 1).await;
        poll_waiting_worker(worker.as_mut()).await;
        assert_eq!(store.state.borrow().requested, 1);
        tokio::time::advance(Duration::from_secs(809)).await;
        poll_waiting_worker(worker.as_mut()).await;
        assert_eq!(store.state.borrow().requested, 1);
        tokio::time::advance(Duration::from_secs(92)).await;
        finish_worker_fetch(worker.as_mut(), &store, 2).await;
        tokio::time::advance(Duration::from_secs(1_000)).await;
        cancel.cancel();
        worker.await;
        assert_eq!(store.state.borrow().requested, 2);
        assert!(store.state.borrow().stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn waiters_share_one_fetch_and_see_its_keys() {
        let store = KeyStore::new(key_set("old"));
        store.permit_unknown_refresh_for_test();
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
    async fn dropping_a_waiter_does_not_cancel_the_shared_fetch() {
        let diagnostics = Diagnostics::default();
        let store = metrics::with_local_recorder(&diagnostics, || KeyStore::new(key_set("old")));
        let initial = diagnostics.acquisitions.lock().unwrap().clone();
        store.permit_unknown_refresh_for_test();
        let first = spawn_refresh(&store);
        tokio::task::yield_now().await;
        let second = spawn_refresh(&store);
        tokio::task::yield_now().await;
        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        assert_eq!(*diagnostics.acquisitions.lock().unwrap(), initial);
        store.finish(1, Ok(key_set("new")));
        assert_eq!(first.await.unwrap(), Some(true));
        assert_eq!(diagnostics.acquisitions.lock().unwrap().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_cooldown_reports_the_last_outcome() {
        let store = KeyStore::new(key_set("old"));
        assert!(spawn_refresh(&store).await.unwrap().is_none());
        store.permit_unknown_refresh_for_test();
        let waiter = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(
            1,
            Err(super::KeySetFailure::Fetch(crate::ProviderFailure::Timeout)),
        );
        assert_eq!(waiter.await.unwrap(), Some(false));
        assert_eq!(spawn_refresh(&store).await.unwrap(), Some(false));
        assert!(store.current().has_kid("old"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiter_gets_its_fetch_set_when_a_later_fetch_fails_before_it_is_polled() {
        let store = KeyStore::new(key_set("old"));
        store.permit_unknown_refresh_for_test();
        let waiter = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(1, Ok(key_set("new")));
        store.request_periodic();
        store.finish(
            2,
            Err(super::KeySetFailure::Fetch(crate::ProviderFailure::Timeout)),
        );
        assert_eq!(waiter.await.unwrap(), Some(true));
    }

    #[tokio::test(start_paused = true)]
    async fn a_set_installed_after_the_token_was_checked_is_the_answer_without_a_fetch() {
        let store = KeyStore::new(key_set("old"));
        let checked = store.current();
        store.permit_unknown_refresh_for_test();
        let rotation = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(1, Ok(key_set("new")));
        assert_eq!(rotation.await.unwrap(), Some(true));
        // The token missed in the old set while that fetch was finishing.
        let SignatureRefresh::NewerSet(keys) = store.refresh_for_signature_miss(&checked).await
        else {
            panic!("the newer installed set must be offered");
        };
        assert!(keys.has_kid("new"));
        assert_eq!(store.pending(), None);
        // A token checked against the installed set is really unknown.
        assert_matches!(
            store.refresh_for_signature_miss(&keys).await,
            SignatureRefresh::NoNewerSet
        );

        store.request_periodic();
        let during_fetch = store.refresh_for_signature_miss(&checked).now_or_never();
        store.finish(
            2,
            Err(KeySetFailure::Fetch(crate::ProviderFailure::Timeout)),
        );
        let after_failure = store.refresh_for_signature_miss(&checked).await;

        assert_matches!(during_fetch, Some(SignatureRefresh::NewerSet(keys)) if keys.has_kid("new"));
        assert_matches!(after_failure, SignatureRefresh::NewerSet(keys) if keys.has_kid("new"));
    }

    #[tokio::test]
    async fn stopping_releases_current_and_future_waiters() {
        let store = KeyStore::new(key_set("old"));
        store.permit_unknown_refresh_for_test();
        let waiter = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.stop();
        assert_eq!(waiter.await.unwrap(), Some(false));
        assert_eq!(spawn_refresh(&store).await.unwrap(), Some(false));
    }
}
