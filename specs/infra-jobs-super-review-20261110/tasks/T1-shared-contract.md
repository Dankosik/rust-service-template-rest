# T1 — Shared public job contract

## Outcome

`crates/infra-jobs` exposes truthful public contracts: the attempt deadline is
documented for what it is, `CompleteError` keeps one cause, `JobError::from`
names its pool-timeout exception, policy bounds are public and used by their
docs, `Disposition` names its units, the validated kind set is `Kinds` with a
`KindsBuilder`, the shared limits live in `kind`, and `KindsBuilder` has one
empty-construction owner. In-repo consumers and tests compile against the new
public paths.

## Consumes

- [Spec](../spec.md) and [Intent](../intent.md).
- `crates/infra-jobs/src/kind.rs`, `lib.rs`, `attempt.rs`, `enqueue.rs`,
  `engine.rs`, `claim.rs`, `maintenance.rs` at current `main` (`7edc91ab`).
- The review report was pinned at `f77a3534`; current `main` differs only in
  documentation paths (now `docs/guides/background-jobs.md`) and the removed
  retention budget. Re-read the current declarations before editing.

## Provides

- Public `infra_jobs::Kinds` (validated) and `infra_jobs::KindsBuilder`
  (unvalidated).
- Public `MAX_ATTEMPTS` and `MAX_TIMEOUT`, joined with the existing
  `MIN_TIMEOUT`/`DEFAULT_TIMEOUT` exports.
- `kind` as the owner of `MAX_DELAY`, `MAX_DELAY_DAYS`, `InvalidDelay`,
  `checked_delay_micros`, and `MAX_ERROR_SUMMARY_BYTES`, with crate-root
  re-exports preserved.
- The T2/T3/T5 starting tree for their file-local work.

## Boundary

Own `kind.rs` and `lib.rs`. In `attempt.rs`, own only the `Attempt.deadline`
rename, the `Disposition` construction/match updates, and the
`MAX_ERROR_SUMMARY_BYTES` import. In `enqueue.rs`, `engine.rs`, `claim.rs`, and
`maintenance.rs`, own only the mechanical type/import updates forced by R-029
and R-030. Update in-repo consumers and tests for those public paths. Do not
implement T2's batch/arm/label changes, T3's claim/engine structure, T4's
operator changes, or T5's wake/carrier changes.

## Mutable owners

- `crates/infra-jobs/src/kind.rs`, `crates/infra-jobs/src/lib.rs`.
- `crates/infra-jobs/src/attempt.rs` (only R-001/R-027/R-030 touch points).
- `crates/infra-jobs/src/enqueue.rs` (R-030 imports and moved definitions).
- `crates/infra-jobs/src/engine.rs`, `claim.rs`, `maintenance.rs` (R-029/R-030
  mechanical updates only).
- `crates/jobs-worker/src/`, `test/src/`, `test/tests/`, `test/examples/` where
  they name `Kinds`/`Registry` or import a moved limit.
- `docs/guides/background-jobs.md` only if it names a changed public path.

## Exclusive locks

- Public type names `Kinds` and `Registry` in `infra-jobs`.
- Crate-root re-export list in `lib.rs`.
- `MAX_DELAY`/`InvalidDelay`/`checked_delay_micros`/`MAX_ERROR_SUMMARY_BYTES`
  definitions.

## Work items

- **R-001:** Rename the private `Attempt.deadline` field to
  `attempt_deadline`; update `Job::deadline` and `Job::context`; the public doc
  says the value is the attempt's local deadline (registered timeout capped by
  the supervisor lease deadline), not the later supervisor deadline.
- **R-002:** `CompleteError::Database` uses `#[error("complete job")]` and keeps
  `Database(#[from] sqlx::Error)` as the source. `with_sources` still composes
  `"complete job: <sqlx>"` for `JobError`.
- **R-003:** Extend the `From<E> for JobError` doc: retryable, except a
  `sqlx::Error::PoolTimedOut` anywhere in the source chain, which becomes
  `unavailable`/snooze.
- **R-004 (kind/lib part):** Make `MAX_ATTEMPTS` and `MAX_TIMEOUT` public and
  re-export them; use them in the `Policy` field docs. In `InvalidName`, use
  `max = MAX_KIND_LEN` and say "lowercase letter". Leave the numeric
  `InvalidPolicy` reason strings as they are. The `InputError::Cursor` wording
  belongs to T4.
- **R-010 (kind part):** Qualify `with_sources`' promise: a source is appended
  while the summary is below `MAX_ERROR_SUMMARY_BYTES`; the final stored bound
  and control replacement belong to `sanitize_summary` (T2).
- **R-027:** Turn `Disposition::RetryAfterAtLeast(i64)` and `Disposition::Snooze(i64)`
  into struct variants `{ floor_micros }` and `{ delay_micros }`; update the
  constructors and the `attempt.rs` match.
- **R-029:** Rename the current `Kinds` builder to `KindsBuilder` and the current
  validated `Registry` to `Kinds`. `KindsBuilder::validate(self) ->
  Result<Kinds, KindError>`. Update `Engine::new`/`beside`, `Shared` fields and
  every internal use, plus `jobs-worker`, the `test` crate, and examples. Do not
  touch `infra-messaging::Registry`; only the jobs type changes.
- **R-030:** Move `MAX_DELAY`, `MAX_DELAY_DAYS`, `InvalidDelay`,
  `checked_delay_micros` from `enqueue.rs` into `kind.rs`; move
  `MAX_ERROR_SUMMARY_BYTES` from `attempt.rs` into `kind.rs`. `lib.rs` continues
  to export `InvalidDelay` and `MAX_DELAY` from the crate root. `enqueue.rs`
  keeps its `EnqueueError::InvalidDelay` variant and `From<InvalidDelay>` impl.
  Update tests that import the moved items.
- **R-031:** Remove the `Default` derive from the builder and write
  `impl Default for KindsBuilder { fn default() -> Self { Self::new() } }`;
  keep `new` const.

## Final validation

- Claim: the public paths and moved limits compile across `infra-jobs`,
  `jobs-worker`, and the `test` crate; no T2–T5 behavior is included.
- Checks: `make lint-changed PKGS="infra-jobs jobs-worker"` and a compile check
  for the `test` crate if it is not covered; `make test-package PKG=infra-jobs`
  may run after the type check. The final validation owns the assembled suite.
- Observable: clean type check and the existing `infra-jobs` unit tests; no
  runtime behavior change.

## Reopen if

The rename or limit move forces a change outside the listed consumers, or an
in-repo consumer relies on the old public path in a way the packet does not
cover.
