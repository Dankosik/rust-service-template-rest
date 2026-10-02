//! Process-owned JWKS refresh shared by every request.
//!
//! One worker performs every fetch, so a request that stops waiting never
//! cancels the fetch other requests wait for.

use std::{sync::Arc, time::Duration};

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
}

impl KeyStore {
    /// Installs the startup key set; that fetch starts the first cooldown.
    pub(crate) fn new(keys: Arc<KeySet>) -> Arc<Self> {
        metrics::describe_counter!(REFRESH_METRIC, "JWKS refresh outcomes by closed reason");
        Arc::new(Self {
            state: watch::Sender::new(State {
                keys,
                requested: 0,
                finished: 0,
                last_started: Instant::now(),
                last_succeeded: true,
                stopped: false,
            }),
            wake: Notify::new(),
        })
    }

    pub(crate) fn keys(&self) -> Arc<KeySet> {
        self.state.borrow().keys.clone()
    }

    /// Joins the fetch in flight, or starts one outside the cooldown. `checked`
    /// is the set the token was checked against; when a newer one is already
    /// installed, that set is the answer and no fetch is needed.
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
            if !Arc::ptr_eq(&state.keys, checked) {
                answer = UnknownKeyRefresh::Refreshed(state.keys.clone());
                return false;
            }
            if state.last_started.elapsed() < REFRESH_COOLDOWN {
                if state.last_succeeded {
                    answer = UnknownKeyRefresh::StillUnknown;
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
    use super::{KeyStore, UnknownKeyRefresh};
    use crate::jwt::parse_key_set;
    use jsonwebtoken::{Algorithm, EncodingKey, crypto::aws_lc::DEFAULT_PROVIDER, jwk::Jwk};
    use std::sync::Arc;

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
        let store = KeyStore::new(key_set("old"));
        store.permit_unknown_refresh_for_test();
        let first = spawn_refresh(&store);
        tokio::task::yield_now().await;
        let second = spawn_refresh(&store);
        tokio::task::yield_now().await;
        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        store.finish(1, Ok(key_set("new")));
        assert_eq!(first.await.unwrap(), Some(true));
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
