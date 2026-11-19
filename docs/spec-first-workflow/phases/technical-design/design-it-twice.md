# Design It Twice

Load from [Technical Design](../technical-design.md) when an open choice hits a
hard-to-reverse fork not settled by `docs/decisions/` or an existing workspace
pattern: a **persisted schema or durable state machine**, a **new cross-crate
public interface with ≥2 consumers**, an **async durability/recovery topology**
(inline tx vs outbox vs job queue vs cache invalidation), or a **new crate,
dependency, or profile boundary**. Parallel lanes with identical briefs
collapse onto one median answer; incompatible pressures force distinct
architectures. Skip when one reuse-first path already dominates.

## Steps

1. **Frame the neutral problem space.** Summarize hard constraints from
   `spec.md`, `quality/architecture.json`, `docs/decisions/`, [Backend Library
   Selection](../../../backend-library-selection.md), affected sections of
   [Production Contract](../../../production-contract.md), dependency
   categories, and canonical domain plus [Rust
   Architecture](rust-architecture.md#vocabulary-and-deep-module-discipline)
   vocabulary without hinting at a solution. Add one short **grounding code
   sketch** of the existing call site or state constraints so parallel lanes
   design against real workspace types.

2. **Dispatch 2–3 parallel fresh `specialist-agent` lanes with contrasting
   pressures.** Give every lane the identical neutral frame, read-only evidence
   boundary, fresh context window (never forking parent history), and
   `Method: docs/spec-first-workflow/phases/technical-design/design-it-twice.md`
   (plus matching domain skills). Brief each lane strictly as a **generative
   candidate lane**—advocating one incompatible optimization axis and returning
   [Candidate Proposal
   V1](../../shared/read-only-delegation.md#candidate-proposal-v1) without
   picking a winner or comparing against siblings:
   - *System / integration fork (2–3 axes):*
     - **Reuse-first / minimal state:** push existing owners, `std`, and
       current datastore/transport to their limit with zero new infrastructure
       or workers.
     - **Fault-isolated / decoupled:** isolate failure domains via explicit
       durable state transitions, outbox/jobs/queues, or bounded async
       recovery.
     - **Data-model / protocol-driven:** push invariants into storage
       constraints, idempotent wire contracts, or maintained library
       capabilities so orchestration stays minimal.
   - *Rust module / interface fork (2–3 axes):*
     - **Minimal deep facade (caller-first):** 1–3 entry points on concrete
       types hiding state, retries, and defaults behind the seam.
     - **Sans-I/O pure core + thin shell:** isolate a pure deterministic state
       machine or decision kernel (zero I/O, trivially testable) from a thin
       imperative I/O/DB shell.
     - **Compile-time protocol / ownership-encoded:** encode transition order,
       validated capabilities, and transaction/resource lifetimes in Rust
       types, typestate, or tokens (weighing generic/ergonomic cost honestly).
     - **Explicit port & adapters** *(only with ≥2 real adapters or a true
       external seam)*: optimize clean adapter swap and isolated seam testing.

3. **Compare, verify, and converge autonomously.** Compare viable same-level
   substitutes—including each lane's `caller_sketch`—on **depth / leverage**,
   **locality**, **failure, trust & recovery clarity**, **budgets & unit
   cost**, and **reversibility**, inspecting primary sources for decisive
   claims. Select the strongest candidate or synthesize a hybrid under
   [Decision Ownership](../../../../AGENTS.md#decision-ownership), recording
   `D<n>` in `design.md` with the strongest rejected candidate as steelman.
   Escalate upstream (`status: blocked`, `reopen_owner: Specification` or
   `Intake`) only when surviving candidates differ strictly on a user-owned
   trade-off.
