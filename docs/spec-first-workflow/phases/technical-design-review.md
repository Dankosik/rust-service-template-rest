# Technical Design Review

Use when shared [Review](../shared/review.md) routes one fixed completed
technical design or the user explicitly requests that standalone review. This
adapter owns only technical-design falsifiers and threshold.

## Lenses

Reconstruct each material trace:

`behavior/drivers -> viable same-level alternatives or no-fork evidence ->
selected mechanism -> material flow/finality -> contract/truth -> system and
crate owner -> required input -> proving surface`.

The first incompatible or unsupported edge is the finding anchor; continue all
unaffected traces. Activate only lenses exposed by current evidence: flow and
authority, cross-flow coherence, current contract/runtime contradiction,
selection against common drivers, required-input availability, Rust ownership or
panel-receipt compatibility, performance/scale, proof feasibility, release
closure, and necessity of each included component/edge/store/dependency.

Consume current [Rust Ownership Review](../rubrics/rust-ownership-review.md) receipts
without repeating their lenses.

`PASS` requires every reconstructed trace and triggered lens to be supported
without downstream invention. `CONCERNS` may carry only bounded proof/risk.
Any missing behavior, mechanism, authority, ownership, required input, or
coherence edge is `FAIL` and reopens the earliest Specification, System
Design, Rust Ownership, Research, or external owner.
