# Design It Twice

Load from [Technical Design](../technical-design.md) for any **one-way door**
open fork unless existing repository conventions (`docs/decisions/`, sibling
subsystem, `quality/architecture.json`) fix one concrete shape with no viable
competing topology or interface. Parallel lanes with identical briefs collapse
onto one median answer; incompatible pressures force **radically different**
architectures.

## Steps

1. **Frame the neutral problem space.** Summarize hard constraints, `Domain
   Vocabulary`, dependency categories, and [Rust
   Architecture](rust-architecture.md#vocabulary-and-deep-module-discipline)
   vocabulary from step 1. Describe **only the problem**—constraints, existing
   call site, and consumer need via a short **grounding code sketch**—keeping
   the brief free of proposed mechanisms, new types, or new crates so each lane
   invents from scratch.

2. **Dispatch 3 parallel fresh `specialist-agent` lanes with contrasting
   pressures** *(use 2 only for a strictly binary fork between two concrete
   mechanisms)*. Give every lane the identical neutral frame, read-only
   boundary, fresh context window (never forking parent history), `Method:
   docs/spec-first-workflow/shared/read-only-delegation.md#candidate-proposal-v1`
   (plus matching domain skills), and **strictly its own pressure** (omit
   sibling pressures and any parent hypothesis). Each lane runs as a
   **generative candidate lane** pushing its pressure to its honest extreme and
   returning [Candidate Proposal
   V1](../../shared/read-only-delegation.md#candidate-proposal-v1) without
   picking a winner or comparing siblings.
   - **Derive problem-specific incompatible pressures first** when concrete
     competing topologies exist (e.g., advisory lock vs unique-upsert + retry vs
     transactional outbox). Otherwise select presets:
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
   Ownership](../../../../AGENTS.md#decision-ownership), recording `D<n>` with
   the strongest rejected candidate as steelman, or take [Blocked
   Exit](non-standard-paths.md#blocked-exit) only when surviving candidates
   differ strictly on a user-owned trade-off.
