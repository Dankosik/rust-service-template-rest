//! PostgreSQL durable background jobs.
//!
//! Owns the one jobs table, `background_jobs`, and every statement on it.
//! Enqueue runs on the caller's open transaction. Only this crate names the
//! table.

mod attempt;
mod claim;
mod engine;
mod enqueue;
#[cfg(test)]
mod gauge_recorder;
mod kind;
mod listener;
mod maintenance;
pub mod operator;
mod session;
mod state;
mod trace_context;

pub use attempt::ATTEMPT_DURATION_METRIC;
pub use claim::{CLAIM_DURATION_METRIC, QUEUE_WAIT_METRIC};
pub use engine::{DrainEnd, Engine, LEASE_RESERVE, OperationError, POLL_INTERVAL, Started};
pub use enqueue::{
    EnqueueError, EnqueueOptions, Enqueued, LivePayloadComparison, MAX_PAYLOAD_BYTES,
    compare_live_payload, enqueue, json_payload_len,
};
pub use kind::{
    CompleteError, DEFAULT_TIMEOUT, Handler, InvalidDelay, Job, JobError, JobId, JobKind,
    KindError, Kinds, KindsBuilder, MAX_ATTEMPTS, MAX_DELAY, MAX_TIMEOUT, MIN_TIMEOUT, Policy,
    assert_valid_kind_name,
};
pub use session::StartupError;

/// Every histogram this crate records. A composition root hands it to the
/// Prometheus recorder; a metric left out renders the default buckets.
pub const HISTOGRAMS: &[(&str, &[f64])] = &[
    (ATTEMPT_DURATION_METRIC, attempt::ATTEMPT_DURATION_BUCKETS),
    (CLAIM_DURATION_METRIC, claim::CLAIM_DURATION_BUCKETS),
    (QUEUE_WAIT_METRIC, claim::QUEUE_WAIT_BUCKETS),
];
