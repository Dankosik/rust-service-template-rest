# Design It Twice

Triggered from [Technical Design](../technical-design.md#steps) for any
**one-way door** not already fixed by `docs/decisions/` or an existing crate
contract. First architectural ideas anchor fast; parallel contrasting lanes
break anchoring before anything locks in.

## Steps

1. **Frame the neutral problem space with real code grounding.**
   - Summarize hard constraints (`R<n>`, `G<n>`, budgets,
     `quality/architecture.json` rules), [Domain
     Vocabulary](../../../repo-architecture.md#domain-vocabulary), and—when
     Rust Architecture applies—dependency categories and [Rust
     Architecture](rust-architecture.md#vocabulary-and-deep-module-discipline)
     vocabulary.
   - Include **real repository grounding** (exact file paths plus the live call
     site, handler, schema, or key workspace types callers already touch) so
     lanes design against real types rather than inventing abstractions. Keep
     the frame strictly free of any preferred mechanism, new crate, or solution
     hypothesis.
   - When `≥2` one-way doors are tightly coupled (for example, durable schema,
     async recovery topology, and cross-crate seam), frame them together as one
     coherent architectural fork rather than spawning `3 × N` uncoupled lanes.

2. **Dispatch 3 parallel fresh `specialist-agent` lanes with contrasting
   pressures** *(use 2 only for a strictly binary fork between two concrete
   mechanisms)*.
   - Start every lane with a **fresh context window** (zero parent turns or
     parent hypotheses). Pass the identical neutral frame, read-only boundary,
     `Method: docs/spec-first-workflow/phases/technical-design/design-it-twice.md#candidate-proposal`
     (plus matching domain skills), and **strictly that lane's assigned
     pressure** (omit what sibling lanes were assigned and any parent
     hypothesis). For a binary fork, name both admissible mechanisms in the
     common constraints and assign each lane one side.
   - **Derive problem-specific incompatible tensions first** so every lane
     explores a genuinely viable trade-off in *this* problem (for example:
     *minimize DB lock contention* vs *single-transaction atomicity*; *optimize
     primary caller ergonomics* vs *make illegal transitions unrepresentable*;
     *zero new infra/state* vs *fault-isolated async recovery*). Fall back to
     presets only when no sharper domain tension exists:
     - *System / crossings:* **Reuse-first / minimal state** (existing
       `crates/infra-*` and `std`, zero new infra/workers); **Fault-isolated /
       decoupled** (durable transitions, outbox/jobs, bounded async recovery);
       **Storage / protocol-driven** (DB constraints, idempotent wire contracts,
       thin orchestration).
     - *Rust seam:* **Minimal deep facade** (1–3 concrete entry points hiding
       coordination, defaults, and error mapping); **Sans-I/O pure core + thin
       shell** (deterministic kernel + thin I/O driver); **Type-enforced
       protocol** (ordering, capabilities, and lifetimes in types, weighing
       ergonomic cost); **Explicit port & adapters** *(only when `≥2` real
       adapters or a true external seam exist)*.

3. **Falsify, verify, and converge autonomously.**
   - Evaluate every viable same-level candidate independently on the same
     rubric: **depth / leverage**, **locality**, **seam placement**, **failure,
     trust & recovery clarity**, **budgets & unit cost**, and **one-way-door
     reversibility**.
   - **Attack the leading candidate before adopting it:**
     1. Walk the **hardest failure, concurrency, or cancellation scenario from
        `spec.md`** line by line through its `caller_sketch` / flow.
     2. Verify every decisive `anchor` (workspace type, `Cargo.lock` API, trait
        bound, DB/config limit) directly in primary source—subagent reports are
        secondary sources.
     3. Run the **deletion test** against the simplest surviving alternative:
        retain extra machinery or abstraction only when the simpler candidate
        breaks a named `R<n>`, `G<n>`, invariant, or bound.
   - Adopt a **hybrid synthesis** only when combining pieces *removes*
     complexity or an abstraction while keeping the winning invariant.
   - Select under [Decision Ownership](../../../../AGENTS.md#decision-ownership);
     escalate to the user strictly when surviving candidates differ only on a
     user-owned product or risk trade-off.
   - *Done when:* `D<n>` records the falsified winner and its strongest rejected
     same-level candidate as steelman (or returns the single product/risk
     trade-off to the user).

## Candidate Proposal

Build one **idiomatic, genuinely usable** design optimizing strictly your
assigned pressure without picking a winner or comparing against sibling lanes.
If your assigned pressure bends badly on this problem, report that honestly in
`thin_leverage_and_risks` instead of padding artificial complexity:

```text
pressure: <assigned optimization axis>
proposal: <flow or Rust interface: types, methods, invariants, ordering, error modes, lifetimes>
caller_sketch: <3-8 line realistic call-site usage covering happy + hardest failure/cancel path against live workspace types, or entry->finality trace>
hidden_complexity: <what stays inside the module or mechanism behind the seam>
dependencies_and_cost: <dependency category/adapters, failure/cancellation/concurrency profile, budgets & unit cost>
thin_leverage_and_risks: <where this candidate bends, breaks, pays extra cost, or locks in one-way doors>
blocker_or_gap: <hard constraint conflict, infeasible pressure, missing spec rule, or none>
anchors: <source-verified repo and Cargo.lock locators used, or uncached gaps>
```
