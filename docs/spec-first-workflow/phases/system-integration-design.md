# System / Integration Design

Use when Implementation would otherwise choose a runtime boundary, source of
truth, material crossing, failure/recovery behavior, or rollout mechanism. Own
one behaviorally equivalent system mechanism and its material flows.

Consume the ready Specification, current runtime/generated/provider/consumer
authority, relevant repository architecture, accepted risks and proof
obligations, and decision-changing Research. Derive material drivers and hard
constraints, compare viable same-level substitutes only while a real fork
survives, and trace each affected actor or trigger to caller-visible completion
or durable finality through [Material Flow](../rubrics/material-flow.md).

When choosing a custom infrastructure mechanism, state the current requirement
or lifecycle cost that rules out suitable repository, standard-library, or
maintained external solutions, including their supported extension points.
A short rationale in the current design is sufficient. Expand solution research
only while an unresolved alternative could change the decision.

Load only methods exposed by the decision:

- runtime boundary, consistency, or truth owner -> [Repository
  Architecture](../../repo-architecture.md) and its leaves;
- client-visible REST wiring -> `rust-api-contract` and `rust-axum`;
- trust, secrets, authorization, signing, or exact bytes -> `rust-security`;
- numeric scale or budget -> `rust-performance`;
- deadlines, overload, readiness, drain, or dependency failure -> `rust-reliability`;
- task lifetime, cancellation, or channel ownership -> `rust-tokio`;
- CI gates, tool pins, the image, or publication -> `rust-delivery-platform`;
- migration, mixed versions, or managed dependency -> [Release
  Closure](../rubrics/release-closure.md);
- non-mechanical Rust placement -> [Rust Code / Ownership
  Design](rust-code-ownership-design.md).

Durable delivery, replay, and reconciliation have no skill until the
messaging and jobs profiles arrive; decide them here and record the decision.

Use [Read-Only Delegation](../shared/read-only-delegation.md) only for
independent material domain questions. The root validates and synthesizes their
results. A surviving technical disagreement uses [Parent-Owned
Recovery](../shared/transition.md#parent-owned-recovery) before this decision
closes. A signature-sensitive shape fixes exact bytes, algorithm, and one
deterministic non-secret vector.

Trade-off: For each material choice with viable alternatives, state the
decisive constraint, the cost accepted, and what evidence or changed
assumption would reverse the choice.

Enforcement: For each critical invariant, name the mechanism and boundary
that enforce it; account for every effectful path that could bypass it.

Return the selected mechanism, driver and alternative dispositions, material
flows, affected authorities and contracts, measurable proof boundaries, and
reopen conditions. Persist only through [Artifacts](../shared/artifacts.md),
then apply [Technical Design Review](technical-design-review.md) through shared
[Review](../shared/review.md).

Ready when every material flow and owner is closed without downstream invention.
Reopen Specification for behavior, Research for evidence, or the named external
owner for a required input.
