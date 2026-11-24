use std::mem::DropGuard;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use operation_context::BoundedContext;
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::credentials::ActiveCredentials;
use crate::error::{InsertError, ObservedFailure, classify_metadata_error, is_schema_drift_error};
use crate::observe::OperationGuard;
use crate::{Shared, within_bounded};

pub(crate) const MAX_TABLE_SLOTS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoordinatorState {
    Cold { generation: u64 },
    Priming { generation: u64, leader_id: u64 },
    Warm { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AcquireDecision {
    Ready { generation: u64 },
    LeadPrime { generation: u64, leader_id: u64 },
    WaitPrime { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvalidateOutcome {
    Invalidated { new_generation: u64 },
    AlreadyInvalidated,
}

#[derive(Debug)]
pub(crate) struct MetadataCoordinator {
    state: CoordinatorState,
    next_leader_id: u64,
}

impl MetadataCoordinator {
    pub(crate) const fn new() -> Self {
        Self {
            state: CoordinatorState::Cold { generation: 1 },
            next_leader_id: 0,
        }
    }

    #[cfg(test)]
    pub(crate) const fn state(&self) -> CoordinatorState {
        self.state
    }

    pub(crate) const fn acquire(&mut self) -> AcquireDecision {
        match self.state {
            CoordinatorState::Cold { generation } => {
                self.next_leader_id = self.next_leader_id.saturating_add(1);
                let leader_id = self.next_leader_id;
                self.state = CoordinatorState::Priming {
                    generation,
                    leader_id,
                };
                AcquireDecision::LeadPrime {
                    generation,
                    leader_id,
                }
            }
            CoordinatorState::Priming { generation, .. } => {
                AcquireDecision::WaitPrime { generation }
            }
            CoordinatorState::Warm { generation } => AcquireDecision::Ready { generation },
        }
    }

    pub(crate) fn on_prime_populated(&mut self, generation: u64, leader_id: u64) -> bool {
        if self.state
            == (CoordinatorState::Priming {
                generation,
                leader_id,
            })
        {
            self.state = CoordinatorState::Warm { generation };
            true
        } else {
            false
        }
    }

    pub(crate) fn on_prime_aborted(&mut self, generation: u64, leader_id: u64) -> bool {
        if self.state
            == (CoordinatorState::Priming {
                generation,
                leader_id,
            })
        {
            self.state = CoordinatorState::Cold { generation };
            true
        } else {
            false
        }
    }

    pub(crate) const fn on_schema_mismatch(&mut self, observed_gen: u64) -> InvalidateOutcome {
        let current_gen = match self.state {
            CoordinatorState::Cold { generation }
            | CoordinatorState::Priming { generation, .. }
            | CoordinatorState::Warm { generation } => generation,
        };
        if current_gen == observed_gen {
            let new_generation = current_gen.saturating_add(1);
            self.state = CoordinatorState::Cold {
                generation: new_generation,
            };
            InvalidateOutcome::Invalidated { new_generation }
        } else {
            InvalidateOutcome::AlreadyInvalidated
        }
    }
}

struct TableSlotState {
    coordinator: MetadataCoordinator,
    client: clickhouse::Client,
    cred_epoch: u64,
}

pub(crate) struct TableSlot {
    state: Mutex<TableSlotState>,
    notify: Notify,
}

enum PrepareStep {
    Ready {
        generation: u64,
        client: clickhouse::Client,
    },
    LeadPrime {
        generation: u64,
        leader_id: u64,
        client: clickhouse::Client,
    },
}

impl TableSlot {
    fn new(creds: &ActiveCredentials, database: &str) -> Self {
        Self {
            state: Mutex::new(TableSlotState {
                coordinator: MetadataCoordinator::new(),
                client: creds.read_client.clone().with_database(database),
                cred_epoch: creds.cred_epoch,
            }),
            notify: Notify::new(),
        }
    }

    fn acquire_step(&self, creds: &ActiveCredentials) -> Option<PrepareStep> {
        let mut state = self.state.lock();
        if state.cred_epoch < creds.cred_epoch {
            if let Some(password) = creds.password.as_ref() {
                state.client = state.client.clone().with_password(password.as_str());
            }
            state.cred_epoch = creds.cred_epoch;
        }
        match state.coordinator.acquire() {
            AcquireDecision::Ready { generation } => Some(PrepareStep::Ready {
                generation,
                client: state.client.clone(),
            }),
            AcquireDecision::LeadPrime {
                generation,
                leader_id,
            } => Some(PrepareStep::LeadPrime {
                generation,
                leader_id,
                client: state.client.clone(),
            }),
            AcquireDecision::WaitPrime { .. } => None,
        }
    }

    fn mark_populated(&self, generation: u64, leader_id: u64) {
        let wake = self
            .state
            .lock()
            .coordinator
            .on_prime_populated(generation, leader_id);
        if wake {
            self.notify.notify_waiters();
        }
    }

    fn mark_aborted(&self, generation: u64, leader_id: u64) {
        let wake = self
            .state
            .lock()
            .coordinator
            .on_prime_aborted(generation, leader_id);
        if wake {
            self.notify.notify_waiters();
        }
    }

    pub(crate) fn invalidate_if_gen(
        &self,
        observed_gen: u64,
        creds: &ActiveCredentials,
        database: &str,
    ) {
        let wake = {
            let mut state = self.state.lock();
            if matches!(
                state.coordinator.on_schema_mismatch(observed_gen),
                InvalidateOutcome::Invalidated { .. }
            ) {
                state.client = creds.read_client.clone().with_database(database);
                state.cred_epoch = creds.cred_epoch;
                true
            } else {
                false
            }
        };
        if wake {
            self.notify.notify_waiters();
        }
    }
}

pub(crate) struct TableSlotRegistry {
    slots: Mutex<IndexMap<String, Arc<TableSlot>>>,
}

impl TableSlotRegistry {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(IndexMap::new()),
        }
    }

    pub(crate) fn get_or_create(
        &self,
        table: &str,
        creds: &ActiveCredentials,
        database: &str,
    ) -> Arc<TableSlot> {
        let mut slots = self.slots.lock();
        if let Some((idx, _, slot)) = slots.get_full(table) {
            let slot = Arc::clone(slot);
            let last = slots.len() - 1;
            if idx != last {
                slots.move_index(idx, last);
            }
            return slot;
        }
        if slots.len() >= MAX_TABLE_SLOTS
            && let Some(evict_idx) = slots.values().position(|slot| Arc::strong_count(slot) == 1)
        {
            slots.shift_remove_index(evict_idx);
        }
        let slot = Arc::new(TableSlot::new(creds, database));
        if slots.len() < MAX_TABLE_SLOTS {
            slots.insert(table.to_owned(), Arc::clone(&slot));
        }
        slot
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.lock().len()
    }
}

fn configure_prepare_client(
    mut client: clickhouse::Client,
    query_id: &str,
    shared: &Shared,
    guard: &OperationGuard<'_>,
    remaining: Duration,
) -> clickhouse::Client {
    if shared.policy.propagate_trace_context {
        client = guard.inject_trace_context(client);
    }
    client = client.with_setting("query_id", query_id);
    shared.apply_client_budget(client, remaining, |client, key, value| {
        client.with_setting(key, value)
    })
}

async fn acquire_prepare_step(
    slot: &TableSlot,
    shared: &Shared,
    bounded: &BoundedContext,
) -> Result<PrepareStep, ObservedFailure<InsertError>> {
    loop {
        if let Some(step) = slot.acquire_step(&shared.credentials.load()) {
            return Ok(step);
        }
        let notified = slot.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(step) = slot.acquire_step(&shared.credentials.load()) {
            return Ok(step);
        }
        within_bounded(bounded, notified)
            .await
            .map_err(ObservedFailure::pre_dispatch_stop)?;
    }
}

pub(crate) async fn prepare_insert<R: clickhouse::Row>(
    slot: &TableSlot,
    table: &str,
    query_id: &str,
    shared: &Shared,
    guard: &OperationGuard<'_>,
    bounded: &BoundedContext,
) -> Result<(clickhouse::insert::Insert<R>, u64), ObservedFailure<InsertError>> {
    let mut retried_warm_drift = false;
    loop {
        let (generation, prime_guard, client) =
            match acquire_prepare_step(slot, shared, bounded).await? {
                PrepareStep::Ready { generation, client } => (generation, None, client),
                PrepareStep::LeadPrime {
                    generation,
                    leader_id,
                    client,
                } => {
                    let abort_guard =
                        DropGuard::new((slot, generation, leader_id), |(slot, generation, id)| {
                            slot.mark_aborted(generation, id);
                        });
                    (generation, Some(abort_guard), client)
                }
            };
        let call_client =
            configure_prepare_client(client, query_id, shared, guard, bounded.remaining());
        match within_bounded(bounded, call_client.insert::<R>(table)).await {
            Ok(Ok(insert)) => {
                if let Some(abort_guard) = prime_guard {
                    let (slot, generation, leader_id) = DropGuard::dismiss(abort_guard);
                    slot.mark_populated(generation, leader_id);
                }
                return Ok((insert, generation));
            }
            Ok(Err(error)) => {
                let was_warm = prime_guard.is_none();
                drop(prime_guard);
                if is_schema_drift_error(&error) {
                    slot.invalidate_if_gen(
                        generation,
                        &shared.credentials.load(),
                        &shared.endpoint.database,
                    );
                    if was_warm && !retried_warm_drift {
                        retried_warm_drift = true;
                        continue;
                    }
                }
                return Err(classify_metadata_error(&error));
            }
            Err(stop) => {
                drop(prime_guard);
                return Err(ObservedFailure::pre_dispatch_stop(stop));
            }
        }
    }
}
