//! Process-owned JWKS refresh shared by every request.
//!
//! One worker performs every fetch, so a request that stops waiting never
//! cancels the fetch other requests wait for.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::{Notify, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    EndpointUrl, JwtAlgorithm,
    jwt::{KeySet, KeySetError, parse_key_set},
    provider::{Document, ProviderClient, ProviderFailure},
};

const REFRESH_INTERVAL: Duration = Duration::from_mins(15);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const REFRESH_METRIC: &str = "authn_jwks_refreshes_total";
const ACQUISITION_METRIC: &str = "authn_jwks_last_successful_acquisition_timestamp_seconds";

/// The outcome of asking for keys a token needs but the installed set lacks.
pub(crate) enum UnknownKeyRefresh {
    /// A newer set than the one the token was checked against is installed:
    /// by a fetch that finished after the request, or by one that finished
    /// between that check and the request.
    Refreshed(Arc<KeySet>),
    /// A fetch succeeded within the cooldown, so the key is really unknown.
    StillUnknown,
    /// The latest fetch failed or the worker stopped.
    Unavailable,
}

#[derive(Clone)]
struct State {
    keys: Arc<KeySet>,
    /// Fetches asked for so far; each is a ticket that waiters compare against.
    requested: u64,
    /// Fetches finished so far, successful or not.
    finished: u64,
    last_started: Instant,
    last_succeeded: bool,
    stopped: bool,
}

/// The installed key set and the bookkeeping that coalesces refreshes.
pub(crate) struct KeyStore {
    state: watch::Sender<State>,
    wake: Notify,
    last_acquisition: metrics::Gauge,
}

impl KeyStore {
    /// Installs the startup key set; that fetch starts the first cooldown.
    pub(crate) fn new(keys: Arc<KeySet>) -> Arc<Self> {
        metrics::describe_counter!(REFRESH_METRIC, "JWKS refresh outcomes by closed reason");
        let state = watch::Sender::new(State {
            keys,
            requested: 0,
            finished: 0,
            last_started: Instant::now(),
            last_succeeded: true,
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

    pub(crate) fn keys(&self) -> Arc<KeySet> {
        self.state.borrow().keys.clone()
    }

    /// Joins the fetch in flight, or starts one outside the cooldown. `checked`
    /// is the set the token was checked against; when the fetch that started
    /// the cooldown has since installed another, that set is the answer.
    pub(crate) async fn refresh_for_unknown_key(&self, checked: &Arc<KeySet>) -> UnknownKeyRefresh {
        let mut ticket = None;
        let mut answer = UnknownKeyRefresh::Unavailable;
        // Inspect the cooldown and claim or join a ticket under the same watch
        // write lock. Returning false can still mean this caller joined a fetch.
        let started = self.state.send_if_modified(|state| {
            if state.stopped {
                return false;
            }
            if state.requested > state.finished {
                ticket = Some(state.requested);
                return false;
            }
            if state.last_started.elapsed() < REFRESH_COOLDOWN {
                if state.last_succeeded {
                    // That fetch may have finished after the token was checked.
                    answer = if Arc::ptr_eq(&state.keys, checked) {
                        UnknownKeyRefresh::StillUnknown
                    } else {
                        UnknownKeyRefresh::Refreshed(state.keys.clone())
                    };
                }
                return false;
            }
            state.requested += 1;
            state.last_started = Instant::now();
            ticket = Some(state.requested);
            true
        });
        let Some(ticket) = ticket else {
            return answer;
        };
        if started {
            self.wake.notify_one();
        }
        // wait_for checks the current snapshot too, covering a fetch that finished
        // before this subscription. Dropping this waiter leaves the worker running.
        let mut state = self.state.subscribe();
        match state
            .wait_for(|state| state.stopped || state.finished >= ticket)
            .await
        {
            Ok(state) if !state.stopped && state.last_succeeded => {
                UnknownKeyRefresh::Refreshed(state.keys.clone())
            }
            _ => UnknownKeyRefresh::Unavailable,
        }
    }

    /// Asks for a periodic fetch unless one is already pending.
    fn request_periodic(&self) {
        self.state.send_if_modified(|state| {
            if state.requested > state.finished {
                return false;
            }
            state.requested += 1;
            state.last_started = Instant::now();
            true
        });
    }

    /// The newest requested ticket, when a fetch is pending.
    pub(crate) fn pending(&self) -> Option<u64> {
        let state = self.state.borrow();
        (state.requested > state.finished).then_some(state.requested)
    }

    /// Records a finished fetch; every ticket up to `ticket` is served by it.
    pub(crate) fn finish(&self, ticket: u64, replacement: Result<Arc<KeySet>, RefreshFailure>) {
        let reason = replacement
            .as_ref()
            .map_or_else(|failure| failure.label(), |_| "success");
        let succeeded = replacement.is_ok();
        match &replacement {
            Ok(_) => {}
            Err(RefreshFailure::Fetch(cause)) => {
                warn!(reason = reason, ?cause, "authn_jwks_refresh_failed");
            }
            Err(_) => warn!(reason = reason, "authn_jwks_refresh_failed"),
        }
        self.state.send_modify(|state| {
            if let Ok(keys) = replacement {
                state.keys = keys;
                self.last_acquisition
                    .set(acquisition_timestamp(SystemTime::now()));
            }
            state.finished = ticket;
            state.last_succeeded = succeeded;
        });
        let result = if succeeded { "success" } else { "failure" };
        metrics::counter!(REFRESH_METRIC, "result" => result, "reason" => reason).increment(1);
    }

    /// Releases every current and future waiter.
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

/// Runs the one bootstrap-owned worker for periodic and unknown-key refreshes.
pub(crate) async fn run_refresh_worker(
    store: Arc<KeyStore>,
    provider: ProviderClient,
    jwks_uri: EndpointUrl,
    algorithms: Vec<JwtAlgorithm>,
    cancel: CancellationToken,
) {
    let mut interval =
        tokio::time::interval_at(Instant::now() + REFRESH_INTERVAL, REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    'worker: loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = store.wake.notified() => {}
            _ = interval.tick() => store.request_periodic(),
        }
        while let Some(ticket) = store.pending() {
            let replacement = tokio::select! {
                biased;
                () = cancel.cancelled() => break 'worker,
                replacement = fetch_key_set(&provider, &jwks_uri, &algorithms) => replacement,
            };
            store.finish(ticket, replacement);
        }
    }
    store.stop();
}

async fn fetch_key_set(
    provider: &ProviderClient,
    jwks_uri: &EndpointUrl,
    algorithms: &[JwtAlgorithm],
) -> Result<Arc<KeySet>, RefreshFailure> {
    let bytes = provider
        .get_json(jwks_uri.url(), Document::Jwks)
        .await
        .map_err(RefreshFailure::Fetch)?;
    parse_key_set(&bytes, algorithms)
        .map(Arc::new)
        .map_err(|error| match error {
            KeySetError::Parse => RefreshFailure::Parse,
            KeySetError::NoUsableKeys => RefreshFailure::NoUsableKeys,
        })
}

#[derive(Clone, Copy)]
pub(crate) enum RefreshFailure {
    Fetch(ProviderFailure),
    Parse,
    NoUsableKeys,
}

impl RefreshFailure {
    fn label(self) -> &'static str {
        match self {
            Self::Fetch(_) => "fetch",
            Self::Parse => "parse",
            Self::NoUsableKeys => "no_usable_keys",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyStore, RefreshFailure, UnknownKeyRefresh, acquisition_timestamp};
    use crate::jwt::parse_key_set;
    use jsonwebtoken::{Algorithm, EncodingKey, crypto::aws_lc::DEFAULT_PROVIDER, jwk::Jwk};
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[derive(Clone, Default)]
    struct Acquisitions(Arc<Mutex<Vec<f64>>>);

    impl metrics::GaugeFn for Acquisitions {
        fn increment(&self, _: f64) {
            panic!("acquisition time must be assigned from the wall clock");
        }

        fn decrement(&self, _: f64) {
            panic!("acquisition time must be assigned from the wall clock");
        }

        fn set(&self, value: f64) {
            self.0.lock().unwrap().push(value);
        }
    }

    impl metrics::Recorder for Acquisitions {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn register_counter(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            metrics::Counter::noop()
        }

        fn register_gauge(&self, key: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            assert_eq!(
                key.name(),
                "authn_jwks_last_successful_acquisition_timestamp_seconds"
            );
            assert_eq!(key.labels().count(), 0);
            metrics::Gauge::from_arc(Arc::new(self.clone()))
        }

        fn register_histogram(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::noop()
        }
    }

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
        let acquisitions = Acquisitions::default();
        let keys = key_set("old");
        assert!(acquisitions.0.lock().unwrap().is_empty());
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let store = metrics::with_local_recorder(&acquisitions, || KeyStore::new(keys.clone()));
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let samples = acquisitions.0.lock().unwrap().clone();
        assert_eq!(samples.len(), 1);
        assert!((before.min(after)..=before.max(after)).contains(&samples[0]));
        assert!(matches!(
            store.refresh_for_unknown_key(&keys).await,
            UnknownKeyRefresh::StillUnknown
        ));
        assert_eq!(acquisitions.0.lock().unwrap().len(), 1);

        for failure in [
            RefreshFailure::Fetch(crate::ProviderFailure::Timeout),
            RefreshFailure::Parse,
            RefreshFailure::NoUsableKeys,
        ] {
            store.request_periodic();
            store.finish(store.pending().unwrap(), Err(failure));
            assert!(Arc::ptr_eq(&store.keys(), &keys));
            assert_eq!(*acquisitions.0.lock().unwrap(), samples);
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
        let samples = acquisitions.0.lock().unwrap();
        assert_eq!(samples.len(), 2);
        assert!((before.min(after)..=before.max(after)).contains(&samples[1]));
        assert!(store.keys().has_kid("old"));
    }

    #[tokio::test]
    async fn cancelling_an_inflight_worker_fetch_preserves_acquisition_time_and_keys() {
        let acquisitions = Acquisitions::default();
        let keys = key_set("old");
        let store = metrics::with_local_recorder(&acquisitions, || KeyStore::new(keys.clone()));
        let initial = acquisitions.0.lock().unwrap().clone();
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
        assert_eq!(*acquisitions.0.lock().unwrap(), initial);
        assert!(Arc::ptr_eq(&store.keys(), &keys));
        assert!(matches!(
            store.refresh_for_unknown_key(&keys).await,
            UnknownKeyRefresh::Unavailable
        ));
    }

    const JWT_SIGNING_DER: &[u8] = include_bytes!("../tests/fixtures/authn-jwt-signing-key.der");

    fn key_set(kid: &str) -> Arc<crate::jwt::KeySet> {
        let _ = DEFAULT_PROVIDER.install_default();
        let mut jwk = Jwk::from_encoding_key(
            &EncodingKey::from_rsa_der(JWT_SIGNING_DER),
            Algorithm::RS256,
        )
        .unwrap();
        jwk.common.key_id = Some(kid.to_owned());
        Arc::new(
            parse_key_set(
                &serde_json::to_vec(&serde_json::json!({"keys": [jwk]})).unwrap(),
                &[crate::JwtAlgorithm::Rs256],
            )
            .unwrap(),
        )
    }

    fn spawn_refresh(store: &Arc<KeyStore>) -> tokio::task::JoinHandle<Option<bool>> {
        let store = Arc::clone(store);
        tokio::spawn(async move {
            match store.refresh_for_unknown_key(&store.keys()).await {
                UnknownKeyRefresh::Refreshed(keys) => Some(keys.has_kid("new")),
                UnknownKeyRefresh::StillUnknown => None,
                UnknownKeyRefresh::Unavailable => Some(false),
            }
        })
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
        assert!(store.keys().has_kid("new"));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_a_waiter_does_not_cancel_the_shared_fetch() {
        let acquisitions = Acquisitions::default();
        let store = metrics::with_local_recorder(&acquisitions, || KeyStore::new(key_set("old")));
        let initial = acquisitions.0.lock().unwrap().clone();
        store.permit_unknown_refresh_for_test();
        let first = spawn_refresh(&store);
        tokio::task::yield_now().await;
        let second = spawn_refresh(&store);
        tokio::task::yield_now().await;
        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        assert_eq!(*acquisitions.0.lock().unwrap(), initial);
        store.finish(1, Ok(key_set("new")));
        assert_eq!(first.await.unwrap(), Some(true));
        assert_eq!(acquisitions.0.lock().unwrap().len(), 2);
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
            Err(super::RefreshFailure::Fetch(
                crate::ProviderFailure::Timeout,
            )),
        );
        assert_eq!(waiter.await.unwrap(), Some(false));
        assert_eq!(spawn_refresh(&store).await.unwrap(), Some(false));
        assert!(store.keys().has_kid("old"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_set_installed_after_the_token_was_checked_is_the_answer_without_a_fetch() {
        let store = KeyStore::new(key_set("old"));
        let checked = store.keys();
        store.permit_unknown_refresh_for_test();
        let rotation = spawn_refresh(&store);
        tokio::task::yield_now().await;
        store.finish(1, Ok(key_set("new")));
        assert_eq!(rotation.await.unwrap(), Some(true));
        // The token missed in the old set while that fetch was finishing.
        let UnknownKeyRefresh::Refreshed(keys) = store.refresh_for_unknown_key(&checked).await
        else {
            panic!("the newer installed set must be offered");
        };
        assert!(keys.has_kid("new"));
        assert_eq!(store.pending(), None);
        // A token checked against the installed set is really unknown.
        assert!(matches!(
            store.refresh_for_unknown_key(&keys).await,
            UnknownKeyRefresh::StillUnknown
        ));
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
