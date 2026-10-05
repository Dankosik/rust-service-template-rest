//! Process-owned JWKS refresh shared by every request.
//!
//! One worker performs every fetch, so a request that stops waiting never
//! cancels the fetch other requests wait for.

use std::{sync::Arc, time::Duration};

use tokio::sync::{Notify, watch};
use tokio::time::Instant;
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
                replacement = fetch_key_set(&provider, &jwks_uri, &algorithms) => replacement,
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
    // At most 90 seconds of spread: the product fits in u64 before division.
    let spread_ns = 90_000_000_000_u64 * u64::from(sample.unwrap_or(0)) / u64::from(u16::MAX);
    REFRESH_INTERVAL
        .checked_sub(Duration::from_nanos(spread_ns))
        .unwrap_or(REFRESH_INTERVAL)
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
    use std::{future::Future, pin::Pin, sync::Arc, task::Poll, time::Duration};
    use tokio_util::sync::CancellationToken;

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

    #[test]
    fn periodic_spread_stays_bounded_and_rng_failure_preserves_the_original_wait() {
        for (sample, expected) in [
            (Ok(0), Duration::from_mins(15)),
            (Ok(u16::MAX), Duration::from_secs(810)),
            (Err(aws_lc_rs::error::Unspecified), Duration::from_mins(15)),
        ] {
            assert_eq!(super::refresh_period_for_sample(sample), expected);
        }
        for sample in 0..=u16::MAX {
            let period = super::refresh_period_for_sample(Ok(sample));
            assert!((Duration::from_secs(810)..=Duration::from_mins(15)).contains(&period));
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

            let checked = store.keys();
            let mut unknown = std::pin::pin!(store.refresh_for_unknown_key(&checked));
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(unknown.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            finish_worker_fetch(worker.as_mut(), &store, period * 2 + 1).await;
            assert!(matches!(unknown.await, UnknownKeyRefresh::Unavailable));

            let due = started + Duration::from_secs(901);
            tokio::time::advance(due - tokio::time::Instant::now()).await;
            finish_worker_fetch(worker.as_mut(), &store, period * 2 + 2).await;
            assert!(store.keys().has_kid("old"));
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
