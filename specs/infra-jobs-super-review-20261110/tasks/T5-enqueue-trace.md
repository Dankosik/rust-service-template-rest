# T5 — Enqueue wake admission and trace carrier

## Outcome

`enqueue.rs` names the wake admission for the mutation it performs and stores
one entry per kind in a map; `trace_context.rs` documents the carrier's actual
stored invariant and names the same-type validation method for what it does.

## Consumes

- [Spec](../spec.md) and [T1](T1-shared-contract.md).
- `crates/infra-jobs/src/enqueue.rs` and `trace_context.rs` at current `main`,
  after T1 moved the delay/summary limits to `kind`.

## Provides

- The enqueue/trace candidate consumed by final validation.

## Boundary

Own `enqueue.rs` and `trace_context.rs` only. Do not edit `kind.rs`, `lib.rs`,
`claim.rs`, `engine.rs`, `attempt.rs`, `operator.rs`, or `state.rs`.

## Mutable owners

- `crates/infra-jobs/src/enqueue.rs`, `crates/infra-jobs/src/trace_context.rs`.

## Exclusive locks

- The private `wake_due`/wake table and the private `Carrier` contract.

## Work items

- **R-005:** Correct the `Carrier` doc to the actual invariant: each present
  value is bounded, ASCII, and control-free; a nonempty `tracestate` must parse;
  a malformed state discards the pair; `traceparent` format validity is decided
  by extraction in `link`; state-only is storable but never links. Do not change
  the stored behavior; the state-only policy decision is unresolved and stays
  out of this unit.
- **R-016:** Rename `wake_due` to a name that states the admission
  (`take_wake_slot` or `reserve_wake_interval`) and replace the `Vec` table with
  a `BTreeMap<&'static str, Instant>` under the same mutex, using `Entry`. Keep
  one lock, the check-and-record atomicity, the per-kind interval, the
  `hotpath` static branch, and the short-circuit order in `enqueue`.
- **R-026:** Rename `Carrier::into_stored` to `validated` (or `normalized`) and
  document that it returns a carrier whose fields are both allowed and bounded,
  or both absent. Keep `Injector`/`Extractor`, `capture`, and `link` behavior.

## Final validation

- Claim: the enqueue wake behavior, payload validation, and trace linking are
  unchanged; the table still stores one entry per kind.
- Checks: `make lint-changed PKGS="infra-jobs"` then
  `make test-package PKG=infra-jobs`. The final validation owns the assembled
  suite.
- Observable: existing enqueue and trace-context unit tests; no runtime
  behavior change.

## Reopen if

The map replacement changes the `hotpath` wrapper's type contract, or the
carrier doc correction would require a behavior change outside the unresolved
state-only decision.
