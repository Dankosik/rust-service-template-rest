# T3 — Claim statement, claim loop, and engine contracts

## Outcome

`claim.rs` and `engine.rs` have one lease formula, one exhaustion/rescue
computation in the claim statement, one statement-turn scope, and named task,
peer, and completion contracts. The claim statement keeps its access paths,
`SKIP LOCKED` scans, claim ordering, and fenced outcomes.

## Consumes

- [Spec](../spec.md) and [T1](T1-shared-contract.md).
- `crates/infra-jobs/src/claim.rs` and `engine.rs` at current `main`, after T1's
  public-type and limit changes.
- The current claim SQL and its `.sqlx` metadata; `docs/guides/background-jobs.md`
  for the lease wording.

## Provides

- The claim/engine candidate consumed by final validation.

## Boundary

Own `claim.rs`, `engine.rs`, the claim statement's `.sqlx` metadata, and the
lease wording in `docs/guides/background-jobs.md`. Do not edit `attempt.rs`,
`kind.rs`, `operator.rs`, `state.rs`, `enqueue.rs`, or `trace_context.rs`.

## Mutable owners

- `crates/infra-jobs/src/claim.rs`, `crates/infra-jobs/src/engine.rs`.
- `.sqlx/` entries for the changed claim statement.
- `docs/guides/background-jobs.md` only for the lease wording R-025 corrects.

## Exclusive locks

- The `claim` SQL statement and its `.sqlx` metadata.
- `Shared::statement_turn`, `Run`, `Guard`/`SupervisorGuard`, and
  `registered_kinds` contracts in `engine.rs`.

## Work items

- **R-007:** Document on `OperationError` and `impl From<infra_postgres::TxError>
  for OperationError` that `Begin`, `CommitFailed`, and `CommitUnknown` all
  report as `Statement`, and why that is safe for the idempotent retention
  batch. The alternative — a separate `Transaction(TxError)` variant that
  preserves `is_commit_unknown` like `OperatorError` — is a public change and
  stays out of this unit unless the doc cannot state the collapse truthfully.
- **R-008:** Add the literal-state planner invariant to the `claim` doc: the
  literals prove `background_jobs_pending`/`background_jobs_running`;
  `claim_expires_at IS NOT NULL` is the equivalent used by `complete` where the
  primary-key lookup is the desired path.
- **R-011:** Scope the claim loop's `statement_turn` guard to `send_claim` and
  return the round from the block; call `finish_round` after the block. Keep the
  stop check after acquiring the turn and the one-connection rule.
- **R-012:** Give `Run` a `label` method; build `Guard { task: run.label(), .. }`
  in `spawn_guarded`; remove the separate task string argument from `spawn`.
  Keep the exact label strings and log fields.
- **R-013:** Document `SupervisorGuard`'s contract: which token `stop` is, that
  an abnormal/aborted supervisor ends the claim loop, and that the claim loop's
  exit closes `attempt_tracker`, which `Started::drained` waits for. Note why
  `Guard` cancels only `failure`.
- **R-014:** State the sorted, deduplicated contract of `registered_kinds` (or
  replace the `binary_search` with `contains`); keep the canonical union for
  `publish_for_kinds` equality.
- **R-017:** Extract the peer-registration block in `Engine::build` into a named
  sync helper and state in `new`/`beside` docs that construction registers the
  engine's kinds as a process peer and invalidates the published sample. Do not
  move registration to `start`.
- **R-024:** Compute `exhausted` and `rescued` once in the claim CTEs and use
  the flags in the UPDATE. Preserve the two `FOR UPDATE SKIP LOCKED` lateral
  scans, ranking/claim_limit, eligibility re-check, and exhausted/attempt
  semantics.
- **R-025:** Add `lease_offset(timeout) -> i64` and bind one
  `lease_offset_micros` array; the SQL uses `statement_timestamp() + offset`,
  the local deadline uses `sent + Duration::from_micros(offset) -
  CANCEL_MARGIN`. Remove the separate timeout array and reserve bind. Keep the
  handler timeout exact from the registry. Correct the guide's "same
  whole-microsecond timeout" wording if it is now inaccurate.

## Final validation

- Claim: the claim statement's access paths and outcomes are unchanged; the
  lease offset and local deadline keep the local deadline earlier than the
  database expiry.
- Checks: `make lint-changed PKGS="infra-jobs"`, `make sqlx-prepare` for the
  changed statement, and `make test-package PKG=infra-jobs`. If the local
  database needed by `sqlx-prepare` or the jobs integration tests is unavailable,
  record the exact missing input; the final validation/CI owns the assembled
  proof.
- Observable: existing claim/attempt tests and a clean `.sqlx` diff.

## Reopen if

The CTE flags change the planner access path, the lease offset cannot preserve
the local-before-database ordering, or `.sqlx` regeneration needs a schema or
database input outside this packet.
