# T2 — Attempt lifecycle and state

## Outcome

`attempt.rs` has no derived completion arrays, no unreachable completion arm,
and one owner for each metric label, local-deadline clamp, gauge name, and
attempt-count log key. The attempt lifecycle still retires completions before
replies, caps waits at the supervisor deadline, and maps outcomes as before.

## Consumes

- [Spec](../spec.md) and [T1](T1-shared-contract.md).
- `crates/infra-jobs/src/attempt.rs` at current `main`, after T1's
  `attempt_deadline`, `Disposition`, and `MAX_ERROR_SUMMARY_BYTES` changes.

## Provides

- The attempt lifecycle candidate consumed by final validation and by T3's
  engine/claim candidate.

## Boundary

Own `attempt.rs` only. Do not edit `kind.rs`, `claim.rs`, `engine.rs`,
`operator.rs`, `enqueue.rs`, `trace_context.rs`, or the public surface. If a
needed helper or doc belongs in another file, record the gap instead of editing
outside the boundary.

## Mutable owners

- `crates/infra-jobs/src/attempt.rs` and its unit-test module.

## Exclusive locks

- The private `Transition`, `Written`, `CompletionBatch`, and metric-handle
  types in `attempt.rs`.

## Work items

- **R-009:** Document `COOPERATIVE_GRACE`: the handler gets this window after
  cancellation, `drive` caps it at the supervisor deadline, and the value's
  contract/tuning status is stated rather than implied.
- **R-010 (attempt part):** Document `payload_summary`'s non-disclosure reason
  (category/line/column instead of the rejected payload) and
  `sanitize_summary`/`MAX_ERROR_SUMMARY_BYTES` as the final stored bound with
  control replacement.
- **R-015:** Remove `CompletionBatch.ids` and `.generations`; build the two
  `Vec`s locally at the bind site from `batch.queued`, in the same order.
  `written_by_position` keeps using `queued` order. Keep the arrays alive until
  the statement future ends and preserve retire-before-reply.
- **R-018:** Remove the `Transition::Complete` arm from `execute`. Give
  `send_outcome`/`execute` a write-only view (`Retry`, `Snooze`, `Fail`,
  `Release`) built by an exhaustive match in `persist`; `complete_batched`
  remains the only engine completion path and `complete()` remains for
  `complete_in_tx`.
- **R-021:** Use `Transition::Complete.label()` and a `Written` label helper in
  `KindMetrics::new` and `acknowledged`, instead of the `"completed"`/`"applied"`
  literals. Keep the same metric names and pre-registered series.
- **R-022:** Add one `clamped_deadline(base, wait, limit)` helper and use it in
  `run_attempt`, `drive`, and the retry sleep. Preserve `within`'s cleanup
  selection and the grace/retry durations.
- **R-028:** Rename the `Ownership` gauge guard to `GaugeLease` or
  `MetricMembership`; rename its fields to say metric membership, and document
  that it holds one gauge unit. Keep the gauge names and drop order.
- **R-032:** Change the `job_failed` record's `job.attempts` field to
  `job.attempt` so it matches the attempt span and the other attempt records.

## Final validation

- Claim: the attempt lifecycle compiles and its existing unit tests pass; no
  completion or cancellation ordering changes.
- Checks: `make lint-changed PKGS="infra-jobs"` then
  `make test-package PKG=infra-jobs`. The final validation owns the assembled
  suite and any cross-crate test.
- Observable: existing unit tests; no runtime behavior change.

## Reopen if

The write-only view cannot be expressed without changing public types, or
removing the batch arrays conflicts with the statement's bind or reply ordering.
