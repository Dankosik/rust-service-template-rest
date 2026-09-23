//! Shared, process-owned JWKS refresh coordination.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::Failure;
use crate::jwt::{KeySet, parse_key_set};
use crate::provider::ProviderClient;
use url::Url;

const REFRESH_INTERVAL: Duration = Duration::from_mins(15);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const ATTEMPT_BUDGET: Duration = Duration::from_secs(3);
const RESPONSE_RESERVE: Duration = Duration::from_millis(100);
const REFRESH_METRIC: &str = "authn_jwks_refreshes_total";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnknownKeyResult {
    Refreshed,
    RefreshFailed,
    CooldownSuccess,
    CooldownFailure,
    DeadlineElapsed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchOutcome {
    Success,
    Failure,
}

#[derive(Clone, Copy, Debug)]
struct Reservation {
    generation: u64,
    deadline: Instant,
}

struct RefreshState {
    keys: Arc<KeySet>,
    next_generation: u64,
    in_flight: Option<Reservation>,
    last_started: Option<Instant>,
    last_outcome: FetchOutcome,
}

/// Synchronizes cache replacement and lets unknown-kid callers join a single
/// process-owned fetch. It deliberately owns no task or runtime.
pub(crate) struct SharedRefresh {
    state: Mutex<RefreshState>,
    completed: watch::Sender<(u64, FetchOutcome)>,
    requested: Notify,
}

impl SharedRefresh {
    pub(crate) fn new(keys: Arc<KeySet>) -> Arc<Self> {
        let (completed, _) = watch::channel((0, FetchOutcome::Success));
        Arc::new(Self {
            state: Mutex::new(RefreshState {
                keys,
                next_generation: 1,
                in_flight: None,
                last_started: Some(Instant::now()),
                last_outcome: FetchOutcome::Success,
            }),
            completed,
            requested: Notify::new(),
        })
    }

    pub(crate) async fn keys(&self) -> Arc<KeySet> {
        self.state.lock().await.keys.clone()
    }

    /// Requests at most one permissible shared refresh and waits for just that
    /// generation. The caller's absolute deadline is never extended.
    pub(crate) async fn refresh_unknown(&self, request_deadline: Instant) -> UnknownKeyResult {
        enum Action {
            Join(u64, watch::Receiver<(u64, FetchOutcome)>),
            Cooldown(UnknownKeyResult),
            WaitForParentDeadline,
        }

        let action = {
            let now = Instant::now();
            let mut state = self.state.lock().await;
            if let Some(reservation) = state.in_flight {
                Action::Join(reservation.generation, self.completed.subscribe())
            } else if state
                .last_started
                .is_some_and(|started| now.saturating_duration_since(started) < REFRESH_COOLDOWN)
            {
                Action::Cooldown(match state.last_outcome {
                    FetchOutcome::Success => UnknownKeyResult::CooldownSuccess,
                    FetchOutcome::Failure => UnknownKeyResult::CooldownFailure,
                })
            } else {
                match reserve_deadline(now, request_deadline) {
                    Some(deadline) => {
                        let generation = state.next_generation;
                        state.next_generation = state.next_generation.saturating_add(1);
                        state.last_started = Some(now);
                        state.in_flight = Some(Reservation {
                            generation,
                            deadline,
                        });
                        self.requested.notify_one();
                        Action::Join(generation, self.completed.subscribe())
                    }
                    None => Action::WaitForParentDeadline,
                }
            }
        };
        match action {
            Action::Join(generation, receiver) => {
                wait_for_generation(receiver, generation, request_deadline).await
            }
            Action::Cooldown(result) => result,
            Action::WaitForParentDeadline => wait_for_parent_deadline(request_deadline).await,
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
        let generation = state.next_generation;
        state.next_generation = state.next_generation.saturating_add(1);
        let reservation = Reservation {
            generation,
            deadline: now + ATTEMPT_BUDGET,
        };
        state.last_started = Some(now);
        state.in_flight = Some(reservation);
        Some(reservation)
    }

    async fn complete(&self, reservation: Reservation, replacement: Result<Arc<KeySet>, Failure>) {
        let mut state = self.state.lock().await;
        // A worker can only complete its own current generation. Keeping this
        // check makes a late cancellation/drop path unable to publish stale
        // material over a later reservation.
        if state.in_flight.map(|current| current.generation) != Some(reservation.generation) {
            return;
        }
        state.last_outcome = match replacement {
            Ok(keys) => {
                state.keys = keys;
                FetchOutcome::Success
            }
            Err(_) => FetchOutcome::Failure,
        };
        state.in_flight = None;
        let outcome = state.last_outcome;
        drop(state);
        self.completed
            .send_replace((reservation.generation, outcome));
        metrics::counter!(REFRESH_METRIC, "result" => match outcome {
            FetchOutcome::Success => "success",
            FetchOutcome::Failure => "failure",
        })
        .increment(1);
    }

    async fn cancel_in_flight(&self) {
        let reservation = {
            let state = self.state.lock().await;
            state.in_flight
        };
        if let Some(reservation) = reservation {
            self.complete(reservation, Err(Failure::Unavailable)).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn permit_unknown_refresh_for_test(&self) {
        let mut state = self.state.lock().await;
        state.last_started = Some(Instant::now() - REFRESH_COOLDOWN - Duration::from_secs(1));
    }
}

/// Runs the one worker returned to process bootstrap. It owns all provider I/O
/// for periodic and unknown-kid refreshes.
pub(crate) async fn run_refresh_worker(
    refresh: Arc<SharedRefresh>,
    provider: ProviderClient,
    jwks_uri: Url,
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
        let replacement = fetch_set(&provider, &jwks_uri, reservation.deadline, &cancel).await;
        refresh.complete(reservation, replacement).await;
    }
    refresh.cancel_in_flight().await;
}

async fn fetch_set(
    provider: &ProviderClient,
    jwks_uri: &Url,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<Arc<KeySet>, Failure> {
    if Instant::now() >= deadline {
        return Err(Failure::Unavailable);
    }
    let result = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(Failure::Unavailable),
        result = provider.get_json(jwks_uri, deadline) => result,
    }?;
    parse_key_set(&result).map(Arc::new)
}

fn reserve_deadline(now: Instant, request_deadline: Instant) -> Option<Instant> {
    let reserved_request_deadline = request_deadline.checked_sub(RESPONSE_RESERVE)?;
    let deadline = (now + ATTEMPT_BUDGET).min(reserved_request_deadline);
    (deadline > now).then_some(deadline)
}

async fn wait_for_parent_deadline(deadline: Instant) -> UnknownKeyResult {
    tokio::time::sleep_until(deadline).await;
    UnknownKeyResult::DeadlineElapsed
}

async fn wait_for_generation(
    mut receiver: watch::Receiver<(u64, FetchOutcome)>,
    generation: u64,
    deadline: Instant,
) -> UnknownKeyResult {
    loop {
        let (completed, outcome) = *receiver.borrow_and_update();
        if completed >= generation {
            return match outcome {
                FetchOutcome::Success => UnknownKeyResult::Refreshed,
                FetchOutcome::Failure => UnknownKeyResult::RefreshFailed,
            };
        }
        match tokio::time::timeout_at(deadline, receiver.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return UnknownKeyResult::RefreshFailed,
            Err(_) => return UnknownKeyResult::DeadlineElapsed,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use tokio::time::Instant;

    use super::{FetchOutcome, SharedRefresh, UnknownKeyResult, reserve_deadline};
    use crate::{Failure, jwt::parse_key_set};

    fn key_set(kid: &str) -> Arc<crate::jwt::KeySet> {
        let mut modulus = vec![0_u8; 256];
        modulus[0] = 0x80;
        let n = URL_SAFE_NO_PAD.encode(modulus);
        let jwks = format!(r#"{{"keys":[{{"kty":"RSA","kid":"{kid}","n":"{n}","e":"AQAB"}}]}}"#);
        Arc::new(parse_key_set(jwks.as_bytes()).unwrap())
    }

    #[tokio::test(start_paused = true)]
    async fn replaces_last_good_only_after_a_usable_success() {
        let refresh = SharedRefresh::new(key_set("old"));
        let reservation = refresh.claim(true).await.unwrap();
        refresh
            .complete(reservation, Err(Failure::Unavailable))
            .await;
        assert!(refresh.keys().await.has_kid("old"));

        let reservation = refresh.claim(true).await.unwrap();
        refresh.complete(reservation, Ok(key_set("new"))).await;
        assert!(!refresh.keys().await.has_kid("old"));
        assert!(refresh.keys().await.has_kid("new"));
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_kids_coalesce_and_cooldown_uses_the_last_outcome() {
        let refresh = SharedRefresh::new(key_set("old"));
        let now = Instant::now();
        {
            let mut state = refresh.state.lock().await;
            state.last_started = Some(now - Duration::from_secs(31));
            state.last_outcome = FetchOutcome::Failure;
        }
        let first_refresh = refresh.clone();
        let second_refresh = refresh.clone();
        let first = tokio::spawn(async move {
            first_refresh
                .refresh_unknown(now + Duration::from_secs(2))
                .await
        });
        let second = tokio::spawn(async move {
            second_refresh
                .refresh_unknown(now + Duration::from_secs(2))
                .await
        });
        tokio::task::yield_now().await;
        let reservation = refresh.state.lock().await.in_flight.unwrap();
        assert_eq!(reservation.generation, 1);
        refresh.complete(reservation, Ok(key_set("new"))).await;
        assert_eq!(first.await.unwrap(), UnknownKeyResult::Refreshed);
        assert_eq!(second.await.unwrap(), UnknownKeyResult::Refreshed);
        assert_eq!(
            refresh
                .refresh_unknown(Instant::now() + Duration::from_secs(1))
                .await,
            UnknownKeyResult::CooldownSuccess
        );
    }

    #[test]
    fn reserved_deadline_never_exceeds_the_request_or_attempt_budget() {
        let now = Instant::now();
        assert!(reserve_deadline(now, now + Duration::from_millis(100)).is_none());
        assert_eq!(
            reserve_deadline(now, now + Duration::from_secs(10)).unwrap(),
            now + Duration::from_secs(3)
        );
        assert_eq!(
            reserve_deadline(now, now + Duration::from_secs(1)).unwrap(),
            now + Duration::from_millis(900)
        );
    }
}
