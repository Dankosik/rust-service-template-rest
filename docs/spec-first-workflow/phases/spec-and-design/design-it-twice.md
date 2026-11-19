# Design It Twice

Triggered from [Spec And Design](../spec-and-design.md#steps) for a qualifying
**one-way door** open fork with competing topologies or interfaces.

## Steps

1. **Frame the neutral problem space.** Summarize hard constraints, `Domain
   Vocabulary`, and—when Rust Architecture triggered—dependency categories and
   [Rust
   Architecture](rust-architecture.md#vocabulary-and-deep-module-discipline)
   vocabulary. Describe **only the problem**—constraints, existing call site or
   runtime entry, and consumer need via a short grounding code sketch or flow
   context—keeping the frame free of a preferred solution. For an open-ended
   fork, leave mechanisms, new types, and crates to the lanes.

2. **Dispatch 3 parallel fresh `specialist-agent` lanes with contrasting
   pressures** *(use 2 only for a strictly binary fork between two concrete
   mechanisms)*. Give every lane the identical neutral frame, read-only
   boundary, `Method:
   docs/spec-first-workflow/phases/spec-and-design/design-it-twice.md#candidate-proposal`
   (plus matching domain skills), and **strictly its own pressure** (omit
   sibling pressures and any parent hypothesis). For a binary fork, name the two
   admissible mechanisms in the common constraints and let each lane develop its
   assigned side. Each lane pushes its pressure to its honest extreme and
   returns [Candidate Proposal](#candidate-proposal).
   - **Derive problem-specific incompatible pressures first** when concrete
     alternatives solve the same problem (for example, minimize lock wait
     versus minimize retry work under contention). Otherwise select presets:
     - *System / integration:* **Reuse-first / minimal state** (existing owners
       and `std`, zero new infra/workers); **Fault-isolated / decoupled**
       (durable transitions, outbox/jobs/queues, bounded async recovery);
       **Data-model / protocol-driven** (storage constraints, idempotent wire
       contracts, or library capabilities keeping orchestration thin).
     - *Rust module / interface:* **Minimal deep facade** (1–3 concrete entry
       points optimizing the primary caller and hiding state/retries/defaults);
       **Sans-I/O pure core + thin shell** (pure deterministic kernel + thin
       I/O shell); **Compile-time protocol** (transition order, capabilities,
       lifetimes in types, weighing ergonomics); **Explicit port & adapters**
       *(only with ≥2 real adapters or a true external seam)*.

3. **Compare, verify, and converge autonomously.** Digest each viable same-level
   substitute sequentially—walking its `caller_sketch` against the worst case
   and attacking its `thin_leverage_and_risks`—on **depth / leverage**,
   **locality**, **seam placement**, **failure, trust & recovery clarity**,
   **budgets & unit cost**, and **one-way-door reversibility**, verifying
   decisive claims and crate/trait assumptions against primary sources. Adopt a
   **hybrid synthesis** only when combining pieces removes complexity or an
   abstraction while keeping the winning invariant. Select under [Decision
   Ownership](../../../../AGENTS.md#decision-ownership) and escalate to the user
   only when surviving candidates differ strictly on a user-owned product or
   risk trade-off.
   - *Done when:* `D<n>` records the chosen architecture and its strongest
     rejected candidate as steelman (or the single product/risk trade-off is
     returned to the user).

## Candidate Proposal

Return one contrasting design candidate without picking a winner or comparing
against sibling lanes:

```text
pressure: <assigned optimization axis>
proposal: <flow or Rust interface: types, methods, invariants, ordering, error modes, lifetimes>
caller_sketch: <3-8 line realistic call-site usage against existing workspace types, or entry->finality trace>
hidden_complexity: <what stays inside the module or mechanism behind the seam>
dependencies_and_cost: <dependency category/adapters, failure/cancellation/concurrency profile, budgets & unit cost>
thin_leverage_and_risks: <where this candidate bends, breaks, pays extra cost, or locks in one-way doors>
blocker_or_gap: <hard constraint conflict, infeasible pressure, missing spec rule, or none>
anchors: <source-verified repo and Cargo.lock locators used, or uncached gaps>
```
