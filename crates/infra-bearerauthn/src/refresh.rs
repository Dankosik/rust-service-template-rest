//! Shared, process-owned JWKS refresh coordination.

use std::{sync::Arc, time::Duration};

use tokio::sync::{Mutex, Notify, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::{
    ProviderUrl,
    jwt::{KeySet, KeySetError, parse_key_set},
    provider::{ProviderClient, ProviderDeadline},
};

const REFRESH_INTERVAL: Duration = Duration::from_mins(15);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const REFRESH_METRIC: &str = "authn_jwks_refreshes_total";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnknownKeyResult {
    Refreshed,
    RefreshFailed,
    CooldownSuccess,
    CooldownFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchOutcome {
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug)]
struct Reservation {
    generation: u64,
}

struct RefreshState {
    next_generation: u64,
    in_flight: Option<Reservation>,
    last_started: Instant,
    last_outcome: FetchOutcome,
}

/// Coordinates one worker-owned snapshot publication and request-local waits.
pub(crate) struct SharedRefresh {
    snapshot: watch::Sender<Arc<KeySet>>,
    snapshot_reader: watch::Receiver<Arc<KeySet>>,
    state: Mutex<RefreshState>,
    completed: watch::Sender<(u64, FetchOutcome)>,
    requested: Notify,
    stopped: CancellationToken,
}

impl SharedRefresh {
    pub(crate) fn new(keys: Arc<KeySet>) -> Arc<Self> {
        metrics::describe_counter!(REFRESH_METRIC, "JWKS refresh outcomes by closed reason");
        let (snapshot, snapshot_reader) = watch::channel(keys);
        let (completed, _) = watch::channel((0, FetchOutcome::Success));
        Arc::new(Self {
            snapshot,
            snapshot_reader,
            completed,
            requested: Notify::new(),
            stopped: CancellationToken::new(),
            state: Mutex::new(RefreshState {
                next_generation: 1,
                in_flight: None,
                last_started: Instant::now(),
                last_outcome: FetchOutcome::Success,
            }),
        })
    }

    /// Clones the current immutable key snapshot without taking the control lock.
    pub(crate) fn keys(&self) -> Arc<KeySet> {
        self.snapshot_reader.borrow().clone()
    }

    pub(crate) async fn refresh_unknown(&self) -> UnknownKeyResult {
        enum Action {
            Wait(u64, watch::Receiver<(u64, FetchOutcome)>),
            Cooldown(UnknownKeyResult),
        }
        let action = {
            let mut state = tokio::select! {
                biased;
                () = self.stopped.cancelled() => return UnknownKeyResult::RefreshFailed,
                state = self.state.lock() => state,
            };
            let now = Instant::now();
            let receiver = self.completed.subscribe();
            if let Some(reservation) = state.in_flight {
                Action::Wait(reservation.generation, receiver)
            } else if now.saturating_duration_since(state.last_started) < REFRESH_COOLDOWN {
                Action::Cooldown(match state.last_outcome {
                    FetchOutcome::Success => UnknownKeyResult::CooldownSuccess,
                    FetchOutcome::Failure => UnknownKeyResult::CooldownFailure,
                })
            } else {
                let reservation = Reservation {
                    generation: state.next_generation,
                };
                state.next_generation = state.next_generation.saturating_add(1);
                state.last_started = now;
                state.in_flight = Some(reservation);
                self.requested.notify_one();
                Action::Wait(reservation.generation, receiver)
            }
        };
        match action {
            Action::Wait(generation, receiver) => {
                wait_for_generation(receiver, generation, &self.stopped).await
            }
            Action::Cooldown(result) => result,
        }
    }

    async fn claim(&self, periodic: bool) -> Option<Reservation> {
        let now = Instant::now();
        let mut state = self.state.lock().await;
        if let Some(reservation) = state.in_flight {
            return Some(reservation);
        }
        if !periodic {
            return None;
        }
        let reservation = Reservation {
            generation: state.next_generation,
        };
        state.next_generation = state.next_generation.saturating_add(1);
        state.last_started = now;
        state.in_flight = Some(reservation);
        Some(reservation)
    }

    async fn complete(
        &self,
        reservation: Reservation,
        replacement: Result<Arc<KeySet>, RefreshFailure>,
    ) {
        let mut state = self.state.lock().await;
        if state.in_flight.map(|current| current.generation) != Some(reservation.generation) {
            return;
        }
        let reason = replacement
            .as_ref()
            .map_or_else(|failure| failure.label(), |_| "success");
        let outcome = match replacement {
            Ok(keys) => {
                self.snapshot.send_replace(keys);
                FetchOutcome::Success
            }
            Err(failure) => {
                warn!(reason = failure.label(), "authn_jwks_refresh_failed");
                FetchOutcome::Failure
            }
        };
        state.last_outcome = outcome;
        state.in_flight = None;
        drop(state);
        self.completed
            .send_replace((reservation.generation, outcome));
        metrics::counter!(REFRESH_METRIC, "result" => match outcome { FetchOutcome::Success => "success", FetchOutcome::Failure => "failure" }, "reason" => reason).increment(1);
    }

    async fn cancel_in_flight(&self) {
        self.stopped.cancel();
        let reservation = self.state.lock().await.in_flight;
        if let Some(reservation) = reservation {
            self.complete(reservation, Err(RefreshFailure::Cancelled))
                .await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn permit_unknown_refresh_for_test(&self) {
        self.state.lock().await.last_started =
            Instant::now() - REFRESH_COOLDOWN - Duration::from_secs(1);
    }
}

/// Runs the one bootstrap-owned worker for periodic and unknown-kid refreshes.
pub(crate) async fn run_refresh_worker(
    refresh: Arc<SharedRefresh>,
    provider: ProviderClient,
    jwks_uri: ProviderUrl,
    algorithms: Vec<crate::JwtAlgorithm>,
    cancel: CancellationToken,
) {
    let mut interval =
        tokio::time::interval_at(Instant::now() + REFRESH_INTERVAL, REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let periodic = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            () = refresh.requested.notified() => false,
            _ = interval.tick() => true,
        };
        let Some(reservation) = refresh.claim(periodic).await else {
            continue;
        };
        let replacement = fetch_set(&provider, &jwks_uri, &algorithms, &cancel).await;
        refresh.complete(reservation, replacement).await;
    }
    refresh.cancel_in_flight().await;
}

async fn fetch_set(
    provider: &ProviderClient,
    jwks_uri: &ProviderUrl,
    algorithms: &[crate::JwtAlgorithm],
    cancel: &CancellationToken,
) -> Result<Arc<KeySet>, RefreshFailure> {
    let deadline = ProviderDeadline::independent(Instant::now());
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(RefreshFailure::Cancelled),
        result = provider.get_json(jwks_uri.url(), deadline) => result,
    }
    .map_err(|_| RefreshFailure::Fetch)?;
    parse_key_set(&result, algorithms)
        .map(Arc::new)
        .map_err(|error| match error {
            KeySetError::Parse => RefreshFailure::Parse,
            KeySetError::NoUsableKeys => RefreshFailure::NoUsableKeys,
        })
}

#[derive(Clone, Copy)]
enum RefreshFailure {
    Fetch,
    Parse,
    NoUsableKeys,
    Cancelled,
}
impl RefreshFailure {
    fn label(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Parse => "parse",
            Self::NoUsableKeys => "no_usable_keys",
            Self::Cancelled => "cancelled",
        }
    }
}

async fn wait_for_generation(
    mut receiver: watch::Receiver<(u64, FetchOutcome)>,
    generation: u64,
    stopped: &CancellationToken,
) -> UnknownKeyResult {
    loop {
        if stopped.is_cancelled() {
            return UnknownKeyResult::RefreshFailed;
        }
        let (completed, outcome) = *receiver.borrow_and_update();
        if completed >= generation {
            return match outcome {
                FetchOutcome::Success => UnknownKeyResult::Refreshed,
                FetchOutcome::Failure => UnknownKeyResult::RefreshFailed,
            };
        }
        tokio::select! {
            biased;
            () = stopped.cancelled() => return UnknownKeyResult::RefreshFailed,
            result = receiver.changed() => {
                if result.is_err() {
                    return UnknownKeyResult::RefreshFailed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SharedRefresh, UnknownKeyResult};
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

    #[tokio::test(start_paused = true)]
    async fn refresh_replaces_a_snapshot_only_after_a_usable_generation() {
        let refresh = SharedRefresh::new(key_set("old"));
        refresh.permit_unknown_refresh_for_test().await;
        let call = {
            let refresh = refresh.clone();
            tokio::spawn(async move { refresh.refresh_unknown().await })
        };
        tokio::task::yield_now().await;
        let reservation = refresh.state.lock().await.in_flight.unwrap();
        refresh.complete(reservation, Ok(key_set("new"))).await;
        assert_eq!(call.await.unwrap(), UnknownKeyResult::Refreshed);
        assert!(refresh.keys().has_kid("new"));
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_a_waiter_does_not_cancel_the_shared_generation() {
        let refresh = SharedRefresh::new(key_set("old"));
        refresh.permit_unknown_refresh_for_test().await;
        let first = {
            let refresh = refresh.clone();
            tokio::spawn(async move { refresh.refresh_unknown().await })
        };
        tokio::task::yield_now().await;
        let reservation = refresh.state.lock().await.in_flight.unwrap();
        let second = {
            let refresh = refresh.clone();
            tokio::spawn(async move { refresh.refresh_unknown().await })
        };
        tokio::task::yield_now().await;
        second.abort();
        assert!(second.await.unwrap_err().is_cancelled());
        refresh.complete(reservation, Ok(key_set("new"))).await;
        assert_eq!(first.await.unwrap(), UnknownKeyResult::Refreshed);
    }
    #[tokio::test(start_paused = true)]
    async fn waiter_remains_until_shared_generation_completes() {
        let refresh = SharedRefresh::new(key_set("old"));
        refresh.permit_unknown_refresh_for_test().await;
        let call = {
            let refresh = refresh.clone();
            tokio::spawn(async move { refresh.refresh_unknown().await })
        };
        tokio::task::yield_now().await;
        let reservation = refresh.state.lock().await.in_flight.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(4)).await;
        tokio::task::yield_now().await;
        refresh.complete(reservation, Ok(key_set("new"))).await;
        assert_eq!(call.await.unwrap(), UnknownKeyResult::Refreshed);
    }

    #[tokio::test]
    async fn worker_shutdown_releases_current_and_future_waiters() {
        let refresh = SharedRefresh::new(key_set("old"));
        refresh.permit_unknown_refresh_for_test().await;
        let call = {
            let refresh = refresh.clone();
            tokio::spawn(async move { refresh.refresh_unknown().await })
        };
        tokio::task::yield_now().await;
        refresh.cancel_in_flight().await;
        assert_eq!(call.await.unwrap(), UnknownKeyResult::RefreshFailed);
        assert_eq!(
            refresh.refresh_unknown().await,
            UnknownKeyResult::RefreshFailed
        );
    }
}
