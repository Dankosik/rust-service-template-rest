# Technical Design Review

Read-only review adapter for one fixed Technical Design candidate
(`specs/<task>/design/design.md` at `status: draft`, linked `design/*.md` and
`rollout.md`, or inline forced-owner evidence) or a standalone design review.
Run statically (skip builds, tests, and live services). Falsify prose and
`Verified anchors` against **primary sources**, record every inspected primary
file, symbol, and `Cargo.lock` source in `evidence_boundary` on every active
axis, and return [Review Result V1](../interfaces/review-result-v1.md) with
classification (`PHASE_DEFECT`, `UPSTREAM_GAP`, or `CONCERN`) and smallest
owner (`Technical Design`, `Specification`, `Intake`, or external authority)
without rewriting `design/` or inventing decisions.

For inline forced-owner evidence, jump to [No-Open-Decisions
Result](#no-open-decisions-result).

## Review Topology

Run only assigned checks, respecting the cold-start boundary (**never open
uncited codebase files before completing Phase A**):

- **Single-reviewer (default, 1 fresh `reviewer-agent`):** Phase A (`1`, cold)
  → Phase B (`2`, `3a` + `3b`, `4`) → Phase C (`5`, `6`).
- **Two-axis parallel (2 fresh `reviewer-agent` lanes, never forking parent
  history):** run strictly the assigned non-overlapping lens; review clears
  only when **both** axes return `PASS` or `CONCERNS` on the same candidate:
  - **Axis 1 — Spec Fidelity, System Mechanics, System Deletion & Rollout:**
    checks **2**, **3a**, **5**, and **6**.
  - **Axis 2 — Cold-Start Handoff, Rust Architecture & Source/Compile Truth:**
    Phase A (**1**, cold — `intent.md`, `spec.md`, `design/`, `rollout.md`, and
    explicitly cited anchors only) → Phase B (**3b** and **4**).

## Adversarial Checks

Report under **explicit per-check headings** (write `none — attacked <witness,
symbol, or crossing>` when clear). Keep findings compact (`anchor`,
`classification`, `falsified claim or constraint`, `concrete witness`,
`smallest owner`). Stop a broken path at its first unsupported edge while
continuing independent paths. **Repo standards override generic taste:**
suppress objections endorsed by [AGENTS.md](../../../AGENTS.md), [Repository
Architecture](../../repo-architecture.md), `docs/decisions/`, or [Backend
Library Selection](../../backend-library-selection.md).

### Phase A — Cold-Start & Handoff *(run first, before opening uncited files)*

1. **Planning cold-start, two-Leads divergence & boundary discipline.** Walk
   `intent.md`, `spec.md`, `design/design.md` (and `rollout.md`), and
   **explicitly cited anchors** with **zero chat history**. `FAIL`
   (`PHASE_DEFECT` or `UPSTREAM_GAP`) on anything observable without opening
   uncited files:
   - Planning or parallel Leads must invent a mechanism, error/retryability
     rule, config rule, owner, or shared identifier (including any unfixed
     `Open to Technical Design:` identifier);
   - `Grounding and planning handoff` violates [Design
     V1](../interfaces/design-v1.md) or [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules) (including non-canonical
     paths or omitted surfaces already evident from cited anchors, missing
     `same-release` vs `multi-release` on `expand -> migrate -> contract`, or
     unqualified `shared-contract`);
   - residual `Blocked decisions` remain in a `draft`, or `design/` prescribes
     function bodies, private `.rs` trees, task slicing, or test cases,
     fixtures, proving layers, commands, or unmandated live gates.

### Phase B — Fidelity, Simplicity & Primary-Source Truth *(open uncited sources now)*

*In single-reviewer mode, if Phase B exposes a missing obligation, invalid
mechanism, or architecture/compile violation, skip Phase C deep attacks on
malformed sections while still reporting upstream gaps found in the same pass.*

2. **Obligation trace, spec fidelity & scope.** Verify `spec.md` is `status:
   ready` (or a valid skipped-Definition fixed external contract), then
   independently map `intent.md` (`I<n>`) + `spec.md` (all `R<n>`, `Open to
   Technical Design:`, `G<n>`, `Terminology`, `Non-goals`,
   `Assumptions/Reopen if`, `Downstream handoff`, `Durable contract update`,
   `Carried concerns`) + `api/` `->` decision/mechanism `->` owner `->`
   proving surface. `FAIL` on any unsupported obligation, tripped `spec.md`
   `Reopen if`, resurrected `intent.md` `Rejected` option without reopening
   Intake, altered behavior, terminology collision, or leaked `Non-goal`.
3. **Simplicity, deletion test, steelman `D<n>` & durable docs** *(split by
   subject across axes; both consult `intent.md`, `docs/decisions/`, and
   `docs/backend-library-selection.md` for their subject and `FAIL` on a
   strawman `D<n>`, missing decisive constraint or objective `Reopen if`, or
   reversing a rejected choice without meeting its reopen condition, reopening
   Intake, or an explicit user request)*:
   - **3a — System mechanisms & contract docs:** try to delete or collapse
     every added system mechanism, queue, cache, worker, fallback, or config
     key onto an existing owner unless forced by `spec.md`. `FAIL` on
     speculative system machinery or omitting required `docs/decisions/` or
     `docs/production-contract.md` `Durable doc updates`.
   - **3b — Rust structure, dependencies & architecture docs:** try to delete
     or collapse every added crate, module, seam, trait, wrapper, dependency,
     or custom helper onto an existing owner, `std`, or a `Cargo.lock` crate.
     `FAIL` on shallow pass-through chains, single-adapter mock-only traits,
     `Clock`/`Rng` traits replaceable by plain parameters, or omitting required
     `docs/decisions/`, `docs/backend-library-selection.md`, or
     `docs/repo-architecture.md` `Durable doc updates`.
4. **Primary-source truth, workspace rules, handoff completeness & compile
   reality.** Inspect real code, exhaustive public workspace type fan-out,
   non-obvious `Cargo.lock` crate sources (`Cargo.lock` or `cargo metadata
   --locked --format-version 1`, checking signatures and semantics),
   [`quality/architecture.json`](../../../quality/architecture.json)
   (`allow_dependents`), `scripts/template/profiles.json`, `Cargo.toml`
   `[workspace.lints]`, `clippy.toml`, and `vendor/` forks. `FAIL` on:
   - any contradicted or uninspected anchor (genuinely uncached crates carried
     in `Assumptions (Reopen if)` are `CONCERNS`);
   - any `quality/architecture.json`, [Repository
     Architecture](../../repo-architecture.md#global-invariants), profile, or
     unaddressed exhaustive-type fan-out violation;
   - any mutated shared wiring/lock file per [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules) omitted from `Shared
     mutated surfaces`, or any superseded code/test or affected generated
     contract (`api/`, `.sqlx/`, `crates/grpc-contracts`) omitted from
     `Superseded paths and generated contracts`;
   - any `S<n>` violating lints or [Rust Architecture's compile-reality
     rules](technical-design/rust-architecture.md#rust-contracts-compile-reality-and-workspace-graph)
     without primary-source trait verification or `Reopen if` +
     `integration-first`, or forcing `pub` internal leaks to test.

### Phase C — System Mechanics & Rollout

5. **System mechanics, failure, trust, concurrency & budget attacks** *(System
   Design).* Attack every real crossing against all applicable probes in
   [System Probes](technical-design/system-probes.md)—verifying cited system
   configs, budgets, middleware, and migrations in primary code—and `FAIL` on
   any unclosed gap, stacked cross-layer retries, or undecided mechanism with a
   concrete witness.
6. **Rollout, mixed-version & required-input closure** *(deployable, worker,
   contract, schema, config, trust material, or managed dependency changes).*
   Attack [Rollout And Mixed-Version
   Compatibility](technical-design/system-design.md#rollout-and-mixed-version-compatibility),
   rollback/roll-forward safety, external input ownership, and [Release
   Closure](../rubrics/release-closure.md) (`rollout.md` when non-trivial).

## No-Open-Decisions Result

Always 1 fresh `reviewer-agent`. Verify preconditions (`spec.md` is `status:
ready` or a valid skipped-Definition fixed external contract with zero
behavioral forks, zero tripped `spec.md` `Reopen if`, and no non-trivial
`rollout.md` required). Inspect primary code, `quality/architecture.json`, and
non-obvious `Cargo.lock` sources for every obligation (`R<n>`, `Open to
Technical Design:`, `G<n>`, `Technical Design` `Downstream handoff`, `Durable
contract update`, `Carried concerns`). Try to construct **one realistic
two-engineer divergence** (mechanism, unpinned identifier, error/retryability,
config rule, rollout path, owner, or seam) or **one primary-source
contradiction**, verify the forced-owner handoff package required by
[No-Open-Decisions
Exit](technical-design/non-standard-paths.md#no-open-decisions-exit), and apply
[Verdict](#verdict).

## Verdict

- `PASS`: every assigned check (or the No-Open-Decisions attack) clears with
  primary-source inspection in `evidence_boundary` and zero downstream
  invention. Never block `PASS` on behind-seam choices, private file splits,
  Planning slicing, Implementation test authoring, deploy-only permissions,
  repo-endorsed patterns, or equivalent valid designs preserving ownership,
  invariants, and bounds.
- `CONCERNS`: strictly a bounded later proof or external risk over an otherwise
  closed design or forced-owner package (e.g., genuinely uncached `Cargo.lock`
  source or optional benchmark). Return `CONCERN` naming **risk, downstream
  owner, and closing action**.
- `FAIL`: any surviving witnessed check. Return `PHASE_DEFECT` or
  `UPSTREAM_GAP` with anchor, concrete witness, and smallest owner (`Technical
  Design`, `Specification`, `Intake`, or external authority).
