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
mod lease;
mod maintenance;
mod traceparent;

pub use attempt::{
    ATTEMPT_DURATION_BUCKETS, ATTEMPT_DURATION_METRIC, ATTEMPTS_METRIC, ERROR_SUMMARY_MAX_BYTES,
};
pub use engine::{
    CANCEL_MARGIN, CLAIM_TTL, DrainEnd, Engine, OPERATION_BACKSTOP, OPERATION_FAILURES_METRIC,
    OperationError, POLL_INTERVAL, RECORD_RETRY_INTERVAL, Started, StartupError, UPKEEP_INTERVAL,
};
pub use enqueue::{
    EnqueueError, EnqueueOptions, Enqueued, MAX_DELAY, MAX_PAYLOAD_BYTES, MAX_UNIQUE_KEY_BYTES,
    enqueue,
};
pub use kind::{
    DEFAULT_MAX_ATTEMPTS, DEFAULT_TIMEOUT, Handler, Job, JobError, JobId, JobKind, KindError,
    Kinds, MAX_ATTEMPTS, MAX_KIND_LEN, MAX_TIMEOUT, MIN_TIMEOUT, Policy, Registry,
};
pub use maintenance::{
    LIVE_JOBS_METRIC, OLDEST_AVAILABLE_AGE_METRIC, RETAIN_COMPLETED_FOR, RETAIN_FAILED_FOR,
    RETENTION_BATCH_ROWS, RETENTION_INTERVAL, SAMPLE_INTERVAL, STARTUP_CHECK_BUDGET,
    UNREGISTERED_KIND,
};
