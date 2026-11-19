# Design It Twice

Load from [Technical Design](../technical-design.md) only when a
hard-to-reverse mechanism or cross-boundary seam has a genuine open fork where
first-idea anchoring could lock in a costly design. Parallel agents given
identical briefs collapse onto the same median answer; contrasting design
pressures force genuinely distinct architectures. Never run this branch as
ceremony when one obvious reuse-first path already dominates.

## Steps

1. **Frame the neutral problem space.** Summarize the hard constraints from
   `spec.md`, `quality/architecture.json`, `docs/decisions/`, verified code
   anchors, and dependency categories without hinting at a preferred design.

2. **Dispatch 2–3 parallel fresh `specialist-agent` lanes with contrasting
   pressures.** Give every lane the exact same neutral frame, read-only
   evidence boundary, and a fresh context window (never forking parent history),
   plus one distinct, incompatible optimization axis:
   - *For a system or integration fork (select 2–3 fitting axes):*
     - **Reuse-first / minimal state:** push existing workspace owners, `std`,
       and current datastore/transport to their limit with zero new
       infrastructure or background workers.
     - **Fault-isolated / decoupled:** isolate failure domains through explicit
       state transitions, outbox/jobs/queues, or bounded async recovery.
     - **Data-model / protocol-driven:** push invariants into storage
       constraints, idempotent wire contracts, or maintained library
       capabilities so orchestration stays minimal.
   - *For a Rust module or interface fork (select 2–3 fitting axes):*
     - **Minimal deep interface:** 1–3 entry points max, maximum leverage per
       entry point, maximum complexity hidden behind the seam.
     - **Type-enforced invariants:** push ordering, validated states, or
       transaction/resource ownership into Rust types and lifetimes so misuse
       fails to compile.
     - **Explicit seam / ports & adapters** *(only when ≥2 real adapters or
       cross-boundary isolation exists)*: optimize clean adapter swap and test
       surface at the seam.

   Each lane returns [Decision Result
   V1](../../shared/read-only-delegation.md#decision-result-v1), covering its
   proposed flow or interface, what complexity it hides, failure/unit-cost
   behavior, and where its leverage is thin.

3. **Compare and converge autonomously.** Compare only viable same-level
   substitutes against the accepted drivers: **depth / leverage**, **locality
   of change**, **failure & recovery clarity**, **unit cost**, and
   **reversibility**. Inspect primary sources for any decisive claim before
   adopting it.
   - Select the strongest candidate—or synthesize a clean hybrid combining
     compatible strengths—autonomously under [Decision
     Ownership](../../../../AGENTS.md#decision-ownership). Record the selected
     design, decisive constraint, strongest rejected candidate, accepted cost,
     and objective reopen trigger in `specs/<task>/design/design.md` (`D<n>`).
   - Escalate upstream (`status: blocked`, `reopen_owner: Specification` or
     `Intake`) only when surviving candidates differ strictly on a user-owned
     trade-off (cost/budget, external SLA, breaking consumer contract, or
     legal/irreversible external effect)—never ask the user to pick between
     internal architectures.
