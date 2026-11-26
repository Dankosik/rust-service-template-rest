use std::mem::DropGuard;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use operation_context::BoundedContext;
use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::client::{
    QuerySettings, Shared, apply_client_budget_settings, format_execution_timeout_secs,
    within_bounded,
};
use crate::credentials::{ActiveCredentials, CredEpoch};
use crate::error::{InsertError, ObservedFailure, classify_metadata_error};
use crate::observe::InsertGuard;

pub(crate) const MAX_TABLE_SLOTS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchemaGen(u64);

impl SchemaGen {
    pub(crate) const INITIAL: Self = Self(1);

    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LeaderId(u64);

impl LeaderId {
    const INITIAL: Self = Self(0);

    const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    #[cfg(test)]
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoordinatorPhase {
    Cold,
    Priming(LeaderId),
    Warm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrimeRole {
    Warm(SchemaGen),
    LeadPrime {
        generation: SchemaGen,
        leader_id: LeaderId,
    },
}

#[derive(Debug)]
pub(crate) struct MetadataCoordinator {
    generation: SchemaGen,
    phase: CoordinatorPhase,
    next_leader_id: LeaderId,
}

impl MetadataCoordinator {
    pub(crate) const fn new() -> Self {
        Self {
            generation: SchemaGen::INITIAL,
            phase: CoordinatorPhase::Cold,
            next_leader_id: LeaderId::INITIAL,
        }
    }

    #[cfg(test)]
    pub(crate) const fn generation(&self) -> SchemaGen {
        self.generation
    }

    #[cfg(test)]
    pub(crate) const fn phase(&self) -> CoordinatorPhase {
        self.phase
    }

    pub(crate) const fn acquire(&mut self) -> Option<PrimeRole> {
        match self.phase {
            CoordinatorPhase::Cold => {
                let leader_id = self.next_leader_id.next();
                self.next_leader_id = leader_id;
                self.phase = CoordinatorPhase::Priming(leader_id);
                Some(PrimeRole::LeadPrime {
                    generation: self.generation,
                    leader_id,
                })
            }
            CoordinatorPhase::Priming(_) => None,
            CoordinatorPhase::Warm => Some(PrimeRole::Warm(self.generation)),
        }
    }

    pub(crate) fn on_prime_populated(
        &mut self,
        generation: SchemaGen,
        leader_id: LeaderId,
    ) -> bool {
        if self.generation == generation && self.phase == CoordinatorPhase::Priming(leader_id) {
            self.phase = CoordinatorPhase::Warm;
            true
        } else {
            false
        }
    }

    pub(crate) fn on_prime_aborted(&mut self, generation: SchemaGen, leader_id: LeaderId) -> bool {
        if self.generation == generation && self.phase == CoordinatorPhase::Priming(leader_id) {
            self.phase = CoordinatorPhase::Cold;
            true
        } else {
            false
        }
    }

    pub(crate) const fn on_schema_mismatch(&mut self, observed_gen: SchemaGen) -> bool {
        if self.generation.0 == observed_gen.0 {
            self.generation = self.generation.next();
            self.phase = CoordinatorPhase::Cold;
            true
        } else {
            false
        }
    }
}

struct TableSlotState {
    coordinator: MetadataCoordinator,
    client: clickhouse::Client,
    cred_epoch: CredEpoch,
}

pub(crate) struct TableSlot {
    state: Mutex<TableSlotState>,
    notify: Notify,
}

struct PrimeLeaseInner<'a> {
    slot: &'a TableSlot,
    generation: SchemaGen,
    leader_id: LeaderId,
}

impl PrimeLeaseInner<'_> {
    fn abort(self) {
        self.slot.mark_aborted(self.generation, self.leader_id);
    }
}

struct PrimeLease<'a>(DropGuard<PrimeLeaseInner<'a>, fn(PrimeLeaseInner<'a>)>);

impl<'a> PrimeLease<'a> {
    fn new(slot: &'a TableSlot, generation: SchemaGen, leader_id: LeaderId) -> Self {
        Self(DropGuard::new(
            PrimeLeaseInner {
                slot,
                generation,
                leader_id,
            },
            PrimeLeaseInner::abort,
        ))
    }

    fn generation(&self) -> SchemaGen {
        self.0.generation
    }

    fn complete(self) -> SchemaGen {
        let inner = DropGuard::dismiss(self.0);
        inner.slot.mark_populated(inner.generation, inner.leader_id);
        inner.generation
    }
}

enum AcquiredSlot<'a> {
    Warm(SchemaGen),
    Priming(PrimeLease<'a>),
}

impl AcquiredSlot<'_> {
    fn generation(&self) -> SchemaGen {
        match self {
            Self::Warm(generation) => *generation,
            Self::Priming(lease) => lease.generation(),
        }
    }

    const fn is_warm(&self) -> bool {
        matches!(self, Self::Warm(_))
    }

    fn finish(self) -> SchemaGen {
        match self {
            Self::Warm(generation) => generation,
            Self::Priming(lease) => lease.complete(),
        }
    }
}

struct PrepareStep<'a> {
    slot: AcquiredSlot<'a>,
    client: clickhouse::Client,
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

    fn acquire_step(&self, creds: &ActiveCredentials) -> Option<PrepareStep<'_>> {
        let mut state = self.state.lock();
        if state.cred_epoch < creds.cred_epoch {
            if let Some(password) = creds.password.as_ref() {
                state.client = state.client.clone().with_password(password.as_str());
            }
            state.cred_epoch = creds.cred_epoch;
        }
        let slot = match state.coordinator.acquire()? {
            PrimeRole::Warm(generation) => AcquiredSlot::Warm(generation),
            PrimeRole::LeadPrime {
                generation,
                leader_id,
            } => AcquiredSlot::Priming(PrimeLease::new(self, generation, leader_id)),
        };
        Some(PrepareStep {
            slot,
            client: state.client.clone(),
        })
    }

    fn mark_populated(&self, generation: SchemaGen, leader_id: LeaderId) {
        let wake = self
            .state
            .lock()
            .coordinator
            .on_prime_populated(generation, leader_id);
        if wake {
            self.notify.notify_waiters();
        }
    }

    fn mark_aborted(&self, generation: SchemaGen, leader_id: LeaderId) {
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
        observed_gen: SchemaGen,
        creds: &ActiveCredentials,
        database: &str,
    ) {
        let wake = {
            let mut state = self.state.lock();
            if state.coordinator.on_schema_mismatch(observed_gen) {
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
    guard: &InsertGuard<'_>,
    remaining: Duration,
) -> clickhouse::Client {
    if shared.policy.propagate_trace_context {
        client = guard.inject_trace_context(client);
    }
    client = client.with_setting("query_id", query_id);
    if let QuerySettings::Client { max_memory_usage } = shared.policy.query_settings {
        let timeout_secs = format_execution_timeout_secs(remaining);
        client = apply_client_budget_settings(
            client,
            max_memory_usage,
            &timeout_secs,
            |client, key, value| client.with_setting(key, value),
        );
    }
    client
}

async fn acquire_prepare_step<'a>(
    slot: &'a TableSlot,
    shared: &Shared,
    bounded: &BoundedContext,
) -> Result<PrepareStep<'a>, ObservedFailure<InsertError>> {
    loop {
        if let Some(step) = slot.acquire_step(&shared.credentials.load()) {
            return Ok(step);
        }
        let mut notified = pin!(slot.notify.notified());
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
    guard: &InsertGuard<'_>,
    bounded: &BoundedContext,
) -> Result<(clickhouse::insert::Insert<R>, SchemaGen), ObservedFailure<InsertError>> {
    let mut retried_warm_drift = false;
    loop {
        let prepared = acquire_prepare_step(slot, shared, bounded).await?;
        let generation = prepared.slot.generation();
        let call_client = configure_prepare_client(
            prepared.client,
            query_id,
            shared,
            guard,
            bounded.remaining(),
        );
        match within_bounded(bounded, call_client.insert::<R>(table)).await {
            Ok(Ok(insert)) => return Ok((insert, prepared.slot.finish())),
            Ok(Err(error)) => {
                let was_warm = prepared.slot.is_warm();
                drop(prepared.slot);
                let classified = classify_metadata_error(&error);
                if classified.schema_drift {
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
                return Err(classified.failure);
            }
            Err(stop) => {
                drop(prepared.slot);
                return Err(ObservedFailure::pre_dispatch_stop(stop));
            }
        }
    }
}
