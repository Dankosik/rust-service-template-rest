use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use arc_swap::ArcSwap;
use operation_context::BoundedContext;
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::credentials::ActiveCredentials;
use crate::error::{InsertError, classify_metadata_error, extract_exception_code};
use crate::insert::{WriteRow, pre_dispatch_stop};
use crate::observe::OperationGuard;
use crate::within_bounded;

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
    database: String,
}

enum SlotStep {
    Ready {
        generation: u64,
        client: clickhouse::Client,
    },
    LeadPrime {
        generation: u64,
        leader_id: u64,
        client: clickhouse::Client,
    },
    WaitPrime,
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
            database: database.to_owned(),
        }
    }

    fn acquire_step(&self, creds: &ActiveCredentials) -> SlotStep {
        let mut state = self.state.lock();
        if state.cred_epoch < creds.cred_epoch {
            if let Some(password) = creds.password.as_ref() {
                state.client = state.client.clone().with_password(password.as_str());
            }
            state.cred_epoch = creds.cred_epoch;
        }
        match state.coordinator.acquire() {
            AcquireDecision::Ready { generation } => SlotStep::Ready {
                generation,
                client: state.client.clone(),
            },
            AcquireDecision::LeadPrime {
                generation,
                leader_id,
            } => SlotStep::LeadPrime {
                generation,
                leader_id,
                client: state.client.clone(),
            },
            AcquireDecision::WaitPrime { .. } => SlotStep::WaitPrime,
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

    pub(crate) fn invalidate_if_gen(&self, observed_gen: u64, creds: &ActiveCredentials) {
        let wake = {
            let mut state = self.state.lock();
            if matches!(
                state.coordinator.on_schema_mismatch(observed_gen),
                InvalidateOutcome::Invalidated { .. }
            ) {
                state.client = creds.read_client.clone().with_database(&self.database);
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

struct RegistryInner {
    slots: HashMap<String, Arc<TableSlot>>,
    lru: VecDeque<String>,
}

pub(crate) struct TableSlotRegistry {
    inner: Mutex<RegistryInner>,
}

impl TableSlotRegistry {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                slots: HashMap::new(),
                lru: VecDeque::new(),
            }),
        }
    }

    pub(crate) fn get_or_create(
        &self,
        table: &str,
        creds: &ActiveCredentials,
        database: &str,
    ) -> Arc<TableSlot> {
        let mut inner = self.inner.lock();
        if let Some(slot) = inner.slots.get(table).cloned() {
            if let Some(pos) = inner.lru.iter().position(|entry| entry == table) {
                inner.lru.remove(pos);
                inner.lru.push_back(table.to_owned());
            }
            return slot;
        }
        if inner.slots.len() >= MAX_TABLE_SLOTS
            && let Some(evict_pos) = inner.lru.iter().position(|entry| {
                inner
                    .slots
                    .get(entry)
                    .is_some_and(|slot| Arc::strong_count(slot) == 1)
            })
            && let Some(evicted) = inner.lru.remove(evict_pos)
        {
            inner.slots.remove(&evicted);
        }
        let slot = Arc::new(TableSlot::new(creds, database));
        if inner.slots.len() < MAX_TABLE_SLOTS {
            let key = table.to_owned();
            inner.slots.insert(key.clone(), Arc::clone(&slot));
            inner.lru.push_back(key);
        }
        slot
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().slots.len()
    }
}

struct PrimeGuard {
    slot: Arc<TableSlot>,
    generation: u64,
    leader_id: u64,
    armed: bool,
}

impl PrimeGuard {
    const fn new(slot: Arc<TableSlot>, generation: u64, leader_id: u64) -> Self {
        Self {
            slot,
            generation,
            leader_id,
            armed: true,
        }
    }

    fn mark_populated(mut self) {
        self.armed = false;
        self.slot.mark_populated(self.generation, self.leader_id);
    }
}

impl Drop for PrimeGuard {
    fn drop(&mut self) {
        if self.armed {
            self.slot.mark_aborted(self.generation, self.leader_id);
        }
    }
}

pub(crate) async fn prepare_insert<R: WriteRow>(
    slot: &Arc<TableSlot>,
    table: &str,
    credentials: &ArcSwap<ActiveCredentials>,
    guard: &OperationGuard<'_>,
    propagate_trace_context: bool,
    bounded: &BoundedContext,
) -> Result<(clickhouse::insert::Insert<R>, u64), InsertError> {
    loop {
        let notified = slot.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        let creds = credentials.load();
        match slot.acquire_step(&creds) {
            SlotStep::Ready { generation, client } => {
                let call_client = if propagate_trace_context {
                    guard.inject_trace_context(client)
                } else {
                    client
                };
                match within_bounded(bounded, call_client.insert::<R>(table)).await {
                    Ok(Ok(insert)) => return Ok((insert, generation)),
                    Ok(Err(error)) => {
                        if is_schema_drift_error(&error) {
                            slot.invalidate_if_gen(generation, &credentials.load());
                        }
                        return Err(InsertError::NotApplied(classify_metadata_error(&error)));
                    }
                    Err(stop) => return Err(pre_dispatch_stop(stop)),
                }
            }
            SlotStep::LeadPrime {
                generation,
                leader_id,
                client,
            } => {
                let prime_guard = PrimeGuard::new(Arc::clone(slot), generation, leader_id);
                let call_client = if propagate_trace_context {
                    guard.inject_trace_context(client)
                } else {
                    client
                };
                match within_bounded(bounded, call_client.insert::<R>(table)).await {
                    Ok(Ok(insert)) => {
                        prime_guard.mark_populated();
                        return Ok((insert, generation));
                    }
                    Ok(Err(error)) => {
                        drop(prime_guard);
                        if is_schema_drift_error(&error) {
                            slot.invalidate_if_gen(generation, &credentials.load());
                        }
                        return Err(InsertError::NotApplied(classify_metadata_error(&error)));
                    }
                    Err(stop) => {
                        drop(prime_guard);
                        return Err(pre_dispatch_stop(stop));
                    }
                }
            }
            SlotStep::WaitPrime => {
                within_bounded(bounded, notified)
                    .await
                    .map_err(pre_dispatch_stop)?;
            }
        }
    }
}

pub(crate) fn is_schema_drift_error(error: &clickhouse::error::Error) -> bool {
    matches!(error, clickhouse::error::Error::SchemaMismatch(_))
        || matches!(
            error,
            clickhouse::error::Error::BadResponse(message)
                if matches!(extract_exception_code(message), Some(16 | 47 | 53))
        )
}
