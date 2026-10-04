# T2 — Acquisition pressure and recovery diagnostics

Outcome:
Replace transaction-only acquisition visibility with truthful slow-success and
timeout observation across the named template operations, preserving native
acquisition, error identity, deadlines and the existing readiness recovery policy.

Consumes:
- [R2](../spec.md#r2-acquisition-pressure-is-visible-outside-transactions) and [R4](../spec.md#r4-overload-and-readiness-recovery-are-demonstrated-locally) — required diagnostic behavior and bounded local recovery evidence.
- [Ordinary acquisition observation](../design/design.md#ordinary-acquisition-observation) and its caller table — exact helper, finite labels/events, threshold, cancellation semantics and native-log policy.
- [Ownership map](../design/ownership.md#files) — existing producer/consumer placement.
- T1's assembled native return output — consumed only by final combined silence/recovery acceptance; implementation uses the already-closed native SQLx interface.

Provides:
- One ordinary acquisition helper with native SQLx return/error types and shared observation for connect.
- All named caller wiring, truthful coverage documentation, and authored focused diagnostic and responsive saturation/recovery coverage.

Boundary:
Implement the helper before its consumers. Keep the transaction wait histogram
and statement-duration boundaries unchanged. Consumers already inside a
transaction or acquired connection do not acquire again. Acquisition observation
does not own return, permit accounting or cancellation. No new metric, operator
knob, application pool facade, request-derived field or broader interception
contract. R4 supplies unchanged-policy proof; it does not authorize capacity or
readiness tuning. Reuse existing adequate tests/fixtures; concrete cases and
commands are chosen during implementation.

Mutable owners:
- `crates/infra-postgres/src/observe.rs`, `pool.rs`, `lib.rs`, `transaction.rs`, `probe.rs`: observer, threshold/native logging, exports and named adapter callers.
- `crates/migrate/src/lib.rs`; `crates/infra-jobs/src/claim.rs`, `attempt.rs`, `maintenance.rs`; `crates/infra-idempotency-store/src/maintenance.rs`: only the named acquisition placements from Design.
- Focused observation tests with their existing owner, primary `test/tests/postgres.rs` and existing relay support if required for the accepted combined observations. Existing health-policy proof is reused; no readiness-policy edit is planned.
- Diagnostic coverage and recovery-observation guidance in `docs/architecture/persistence.md` and `docs/validation/postgres.md`, preserving T1 runtime/custody and T3 sizing sections.

Exclusive locks:
- Adapter acquisition interface and the named consumer files while changed.
- Primary PostgreSQL test/relay files and the two guide files while mutated; acquire only after any T1 writer releases them, and serialize overlapping T3 documentation writes.

Final validation:
- Claim: Named transaction/direct/readiness acquisitions distinguish slow success and native PoolTimedOut from PoolClosed/execution failures; cancelled unfinished acquisition emits neither success nor an invented timeout. Current-policy readiness and useful work recover after responsive saturation is released.
- Checks: Matching assembled build/unit tests and accepted bounded local R2/R4 real-database observations under the existing validation owner, sharing evidence with T1 where appropriate. Existing sufficient health-policy proof is reused. Concrete cases, fixture control and commands remain executor-owned, without one repeated recovery suite per caller.
- Observable: The finite documented events supply elapsed acquisition and operation context without sensitive values or duplicate attempt accounting; the transaction histogram retains its meaning. Saturation and silence remain distinguishable, and recorded recovery timing is interpreted under current policy without restart or capacity growth.

Reopen if:
Technical Design for missing named observation ownership or a required new
runtime/lifecycle boundary; Specification for changed caller-visible error,
budget, readiness, finality or support policy. Missing test cases and mechanical
caller/fixture repairs remain implementation work.

## Implementation dependency note

Primary real-database assertions must inspect emitted acquisition and readiness
events, alongside native outcomes. Reuse the existing workspace `tracing`
0.1.44 declaration (`default-features = false`) as a PostgreSQL-owned
`integration-tests` dev-dependency, with no additional feature. Direct subscriber
capture avoids a test-only production export or a new capture library.

This explicitly adds only the `integration-tests -> tracing` test edge. It is
separate from T1's already verified source-only SQLx lock projection; the
backport itself still changes no package version, feature or dependency edge.
The lock projection was parsed before/after and checked against that one
expected edge. `cargo metadata --locked --offline --format-version 1` before
and after confirmed 588 identical package identities and feature sets, with
only that one dev edge added. Any additional
resolution delta returns to the dependency/design owner.
