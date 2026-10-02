//! PostgreSQL durable background jobs.
//!
//! Owns the one jobs table, `background_jobs`, and every statement on it.
//! Enqueue runs on the caller's open transaction. Only this crate names the
//! table.

mod attempt;
mod claim;
mod engine;
mod enqueue;
mod kind;
mod maintenance;
pub mod operator;
mod trace_context;

pub use attempt::{ATTEMPT_DURATION_BUCKETS, ATTEMPT_DURATION_METRIC};
pub use claim::{
    CLAIM_DURATION_BUCKETS, CLAIM_DURATION_METRIC, QUEUE_WAIT_BUCKETS, QUEUE_WAIT_METRIC,
};
pub use engine::{
    DrainEnd, Engine, LEASE_RESERVE, OperationError, POLL_INTERVAL, Started, StartupError,
};
pub use enqueue::{
    EnqueueError, EnqueueOptions, Enqueued, InvalidDelay, LivePayloadComparison, MAX_DELAY,
    MAX_PAYLOAD_BYTES, compare_live_payload, enqueue,
};
pub use kind::{
    CompleteError, DEFAULT_TIMEOUT, Handler, Job, JobError, JobId, JobKind, KindError, Kinds,
    MIN_TIMEOUT, Policy, Registry, assert_valid_kind_name,
};
