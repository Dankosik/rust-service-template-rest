# Design It Twice

Load from [Technical Design](../technical-design.md) for an unsettled
**one-way door** when no reuse-first path already dominates. Parallel lanes
with identical briefs collapse onto one median answer; incompatible pressures
force **radically different** architectures.

## Steps

1. **Frame the neutral problem space.** Summarize hard constraints from
   `spec.md` (including `Terminology`), [Domain
   Vocabulary](../../../repo-architecture.md#domain-vocabulary),
   `quality/architecture.json`, `docs/decisions/`, [Backend Library
   Selection](../../../backend-library-selection.md), affected [Production
   Contract](../../../production-contract.md) sections, dependency categories,
   and [Rust
   Architecture](rust-architecture.md#vocabulary-and-deep-module-discipline)
   vocabulary. Name zero proposed mechanisms, new types, or new crates; add a
   short **grounding code sketch** showing only the existing call site, state
   constraints, or consumer need (never a solution proposal).

2. **Dispatch 2–3 parallel fresh `specialist-agent` lanes with contrasting
   pressures.** Give every lane the identical neutral frame, read-only
   boundary, fresh context window (never forking parent history), and `Method:
   docs/spec-first-workflow/phases/technical-design/design-it-twice.md` (plus
   matching domain skills). Brief each lane strictly as a **generative
   candidate lane** pushing its pressure to its honest extreme and returning
   [Candidate Proposal
   V1](../../shared/read-only-delegation.md#candidate-proposal-v1) without
   picking a winner or comparing siblings.
   - **Derive 2–3 problem-specific incompatible pressures first** when concrete
     competing topologies exist (e.g. advisory lock vs unique-upsert + retry vs
     transactional outbox). Fall back to these presets otherwise:
     - *System / integration:* **Reuse-first / minimal state** (existing owners
       and `std`, zero new infra/workers); **Fault-isolated / decoupled**
       (durable state transitions, outbox/jobs/queues, bounded async recovery);
       **Data-model / protocol-driven** (storage constraints, idempotent wire
       contracts, or library capabilities keeping orchestration thin).
     - *Rust module / interface:* **Minimal deep facade** (1–3 concrete entry
       points hiding state, retries, defaults); **Sans-I/O pure core + thin
       shell** (pure deterministic kernel + thin I/O shell); **Compile-time
       protocol** (transition order, capabilities, and lifetimes in Rust types,
       weighing ergonomics); **Explicit port & adapters** *(only with ≥2 real
       adapters or a true external seam)*.

3. **Compare, verify, and converge autonomously.** Digest each viable
   same-level substitute sequentially—walking its `caller_sketch` against the
   worst failure/usage case—on **depth / leverage**, **locality**, **seam
   placement** (highest clean seam), **failure, trust & recovery clarity**,
   **budgets & unit cost**, and **one-way-door reversibility**, checking
   primary sources for decisive claims. Test **hybrid synthesis** before
   selecting under [Decision
   Ownership](../../../../AGENTS.md#decision-ownership), recording `D<n>` in
   `design.md` with the strongest rejected candidate as steelman. Escalate via
   [Blocked Exit](../technical-design.md#blocked-exit) only when surviving
   candidates differ strictly on a user-owned trade-off.
