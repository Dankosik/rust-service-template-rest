# T4 — Operator and stored-state vocabulary

## Outcome

`operator.rs` and `state.rs` have one owner for the stored state vocabulary, one
identity/failed check in recovery, one named conflict classifier, truthful
snapshot documentation, a named statement-timeout policy, and a single empty
case in `parse_handled_kinds`.

## Consumes

- [Spec](../spec.md).
- `crates/infra-jobs/src/operator.rs` and `state.rs` at current `main`.
- The public kind-name and cursor diagnostics shared with R-004.

## Provides

- The operator/state candidate consumed by final validation.

## Boundary

Own `operator.rs` and `state.rs`. Do not edit `kind.rs`, `lib.rs`, `claim.rs`,
`engine.rs`, `attempt.rs`, `enqueue.rs`, or `trace_context.rs`. If a shared
constant must move, record the gap; T1 owns the shared contract.

## Mutable owners

- `crates/infra-jobs/src/operator.rs`, `crates/infra-jobs/src/state.rs`.

## Exclusive locks

- The public `OperatorError`, `JobSnapshot`, and `RecoveryTarget` contracts.
- The `JobState`/`FailureReason` stored strings and their reverse mappings.

## Work items

- **R-004 (operator wording):** In `InputError::Cursor`, use
  `prefix = CURSOR_PREFIX` instead of the hardcoded `v1`. Do not change the
  public kind-name bounds; T1 owns those.
- **R-006:** Document `JobSnapshot`'s timestamp format (UTC RFC3339 with six
  fractional digits) and that `attempts` are used attempts including the current
  one. Keep the string JSON shape.
- **R-019:** Make `lock_failed_job` lock an existing row by id only; keep the
  mutation's kind/version/state predicate as the single authoritative check and
  map a zero-row/missing result to `Stale`. Extract
  `fn conflict_or_database(error: sqlx::Error) -> OperatorError` for the
  `23505` + `background_jobs_live_unique_key` rule. Preserve `Missing`/`Stale`,
  the row lock, the transaction boundary, the archive, and `new_version`.
- **R-020:** Give `JobState` one list/table that drives both `as_str` and
  `parse` (a `const ALL` or a macro); make `FailureReason` follow the same
  pattern. In `Filter::admits`, use an exhaustive match over `JobState` for the
  live/terminal decision. In `lock_failed_job`, compare through the enum
  (`JobState::Failed.as_str()` or `parse`) instead of the `"failed"` literal.
  Keep the stored strings and `None` for unknown values.
- **R-023:** Add `const STATEMENT_TIMEOUT: &str = "2000ms"` and use
  `set_config('statement_timeout', $1, true)` in `inspect` and
  `lock_failed_job`. Keep the transaction-local restore and the 12 s outer
  bound. If sampling must share the policy, record that decision instead of
  editing `maintenance.rs`.
- **R-033:** Return `Ok(BTreeSet::new())` for an empty `handled_kinds` string
  before the bounds check and loop; keep the byte/comma bounds and per-entry
  grammar for non-empty input.

## Final validation

- Claim: operator outcomes and stored-state decoding are unchanged; the
  recovery SQL still serializes concurrent operators and archives the same
  failed cycle.
- Checks: `make lint-changed PKGS="infra-jobs"` then
  `make test-package PKG=infra-jobs`. The final validation owns the assembled
  suite.
- Observable: existing operator tests and the `JobState`/`FailureReason` unit
  tests; no public behavior change.

## Reopen if

The existence-only lock cannot preserve `Missing`/`Stale` without changing the
public error contract, or the state table/macro would require a new dependency.
