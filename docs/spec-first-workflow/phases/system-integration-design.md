# System / Integration Design

Use when Implementation would otherwise choose a runtime boundary, source of
truth, material crossing, failure/recovery behavior, or rollout mechanism. Own
one behaviorally equivalent system mechanism and its material flows.

Consume the ready Specification, current runtime/generated/provider/consumer
authority, relevant repository architecture, accepted risks and proof
obligations, and decision-changing Research. Derive material drivers and hard
constraints from those inputs.

When several mechanisms could meet the same obligation, compare them using
the same concrete scenario. Include the current mechanism when viable. Show
where their consequences differ on a material driver, then weigh the choice
against all accepted drivers. Name the cost accepted and what evidence would
reverse the decision. If the options are equivalent on those drivers, choose
the simplest. If only one is viable, name the fact that settles the choice.
For a hard-to-reverse choice where independent designs could reduce anchoring,
use [Generative Candidate Lanes](../shared/read-only-delegation.md#generative-candidate-lanes).

When choosing a custom infrastructure mechanism, state the current requirement
or lifecycle cost that rules out suitable repository, standard-library, or
maintained external solutions, including their supported extension points.
A short rationale in the current design is sufficient. Expand solution research
only while an unresolved alternative could change the decision.

Load only methods exposed by the decision:

- runtime boundary, consistency, or truth owner -> [Repository
  Architecture](../../repo-architecture.md);
- client-visible REST wiring -> `rust-api-contract` and `rust-axum`;
- PostgreSQL pool, transactions, or migrations -> `rust-sqlx`;
- trust, secrets, authorization, signing, or exact bytes -> `rust-security`;
- numeric scale or budget, or a flow run per request, message, or job ->
  `rust-performance`;
- deadlines, overload, readiness, drain, or dependency failure -> `rust-reliability`;
- a failure, degradation, or recovery path an operator must tell apart ->
  `rust-observability`;
- task lifetime, cancellation, or channel ownership -> `rust-tokio`;
- CI gates, tool pins, the image, or publication -> `rust-delivery-platform`;
- potential impact on deployment, operation, or recovery, including changes to
  runtime configuration or trust material -> [Release Closure](../rubrics/release-closure.md);
- open Rust placement, invariant ownership, caller obligations, or dependency
  direction between modules -> [Rust Code / Ownership Design](rust-code-ownership-design.md).

Durable delivery, replay, and reconciliation have no dedicated skill; decide
them here from [Repository Architecture](../../repo-architecture.md) and record
the decision.

Use [Read-Only Delegation](../shared/read-only-delegation.md) only for
independent material domain questions. The root validates and synthesizes their
results. A surviving technical disagreement uses [Parent-Owned
Recovery](../shared/transition.md#parent-owned-recovery) before this decision
closes. A signature-sensitive shape fixes exact bytes, algorithm, and one
deterministic non-secret vector.

Enforcement: For each critical invariant, name the mechanism and boundary
that enforce it; account for every effectful path that could bypass it.

Operator signal: For each failure, degradation, or recovery path, name the log
event, metric label, span outcome, or probe state that tells an operator this
path apart from its neighbours and from success.

Unit cost: For each flow run per request, message, or job, or over
caller-controlled input, state its cost per unit in round trips, queries, held
locks, and buffered bytes, and what bounds that cost. Derive it from the
selected mechanism; measure only when an accepted budget or Research requires
it.

## Material Flow

Trace each actor or trigger to caller-visible completion or durable finality.
At every material crossing record only what Implementation must not invent:

- initiating and receiving owners, transferred responsibility, contract, and
  produced state/effect;
- canonical representation, transformation, source of truth, identifiers,
  units, absence, consistency, visibility, freshness, ordering, and finality;
- failure point, cancellation/timeout, possibly committed effect, retry and
  exhaustion owner, idempotency/replay/reconciliation, degradation, and
  restoration boundary;
- when scale-sensitive, workload envelope, critical path, amplification,
  serialization/parallelism/queueing, structural ceiling, and proving boundary.

When flows can affect the same guarantee, try the smallest ordering,
interruption, or replay that could break it. At the decisive step, identify
what happened, what the next actor can know, and what the design permits it
to do. Resolve any case where an allowed action breaks the guarantee or the
actor cannot carry out required recovery.

Show current and target flows only when their difference changes a decision.
Use a diagram only when compact text cannot make ordering, fan-out,
transformation, or recovery reviewable; normative text and canonical contracts
remain authoritative.

Return the selected mechanism, driver and alternative dispositions, material
flows, operator signals, unit costs, affected authorities and contracts,
measurable proof boundaries, and reopen conditions. Persist only through
[Artifacts](../shared/artifacts.md). Include this result in the phase candidate
for [Technical Design Review](technical-design-review.md).

Ready when every material flow and owner is closed without downstream invention.
Reopen Specification for behavior, Research for evidence, or the named external
owner for a required input.
