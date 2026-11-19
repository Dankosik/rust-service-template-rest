# Technical Design Review

Read-only review adapter for one fixed Technical Design candidate
(`specs/<task>/design/design.md` at `status: draft`, linked `design/*.md` and
`rollout.md`, or inline forced-owner evidence) or a standalone design review.
Run statically (skip builds, tests, and live services). Falsify prose and
`Verified anchors` against **primary sources**, record every inspected primary
file, symbol, and `Cargo.lock` source in `evidence_boundary` on every active
axis, and return [Review Result V1](../interfaces/review-result-v1.md) per
[Verdict](#verdict) without rewriting `design/` or inventing decisions.

For inline forced-owner evidence, jump to [No-Open-Decisions
Result](#no-open-decisions-result).

## Review Topology

For a draft design, dispatch two fresh reviewers when both System Design and
Rust Architecture apply and the design spans multiple crates or crossings,
closes a one-way-door fork, or requires a non-trivial rollout. Otherwise
dispatch one fresh reviewer. Apply the [Review Lifecycle](../shared/review.md#lifecycle)
to the fixed candidate.

- **Single reviewer:** check 1 first, then 2, applicable parts of 3, 4, and
  applicable checks 5–6.
- **Axis 1 — specification, system and rollout:** checks 2, 3a, 5 and 6 as
  applicable. Include prior task research for the fidelity check.
- **Axis 2 — cold-start handoff and Rust architecture:** check 1 first, then
  3b and 4.

For check 1, brief the reviewer with the method, assigned checks, fixed
candidate (including linked siblings and rollout), accepted baseline and
explicitly cited anchors only. A single reviewer may open uncited task
research in Phase B. Review clears once every active lane clears on the same
candidate under the shared lifecycle.

## Adversarial Checks

Report under **explicit per-check headings** (write `none — attacked <witness,
symbol, or crossing>` when clear). Keep findings compact (`anchor`,
`classification`, `falsified claim or constraint`, `concrete witness`,
`smallest owner`). Stop a broken path at its first unsupported edge while
continuing independent paths. **Repo standards override generic taste:**
suppress taste objections to choices endorsed by [AGENTS.md](../../../AGENTS.md), [Repository
Architecture](../../repo-architecture.md), `docs/decisions/`, or [Backend
Library Selection](../../backend-library-selection.md).

### Phase A — Cold-Start & Handoff *(run first, before opening uncited files)*

1. **Planning cold-start, two-Leads divergence & boundary discipline.** Walk the check 1 brief with **zero chat history**. `FAIL`
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

2. **Obligation trace, spec fidelity & scope.** Verify the accepted baseline under [Technical Design](technical-design.md#steps),
   including the cited contract when Definition was skipped. Independently
   map the applicable obligations from `intent.md` (`I<n>`
   including `Binding` strength) + `spec.md` (all `R<n>`, `Open to
   Technical Design:`, `G<n>`, `Terminology`, `Non-goals`, `Decisions,
   assumptions, and reopen triggers`, `Downstream handoff`, `Durable contract
   update`, `Carried concerns`) + `api/` `->` decision/mechanism `->` owner `->`
   proving surface. `FAIL` on any unsupported obligation, tripped `spec.md`
   `Reopen if`, choice contradicting settled intent without renewed Intake authority, altered behavior, terminology collision, or leaked `Non-goal`.
3. **Simplicity, deletion test, steelman `D<n>` & durable docs** *(split by
   subject across axes; both consult `intent.md`, `docs/decisions/`, and
   `docs/backend-library-selection.md` for their subject and `FAIL` on a
   strawman `D<n>`, missing decisive constraint or objective `Reopen if`, or
   reversing a rejected choice outside the applicable Intent or repository
   decision rules in [Technical Design](technical-design.md#steps))*:
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
   --locked --offline --format-version 1`, checking signatures and semantics),
   [`quality/architecture.json`](../../../quality/architecture.json)
   (`allow_dependents`), `template/profiles.json`, `Cargo.toml`
   `[workspace.lints]`, `clippy.toml`, and `vendor/` forks. `FAIL` on:
   - any contradicted or uninspected `Verified anchors` claim. Classify an
     explicitly recorded evidence gap by its effect on the decision under
     [Verdict](#verdict);
   - any `quality/architecture.json`, [Repository
     Architecture](../../repo-architecture.md#global-invariants), profile, or
     unaddressed exhaustive-type fan-out violation;
   - any mutated shared wiring/lock file per [Task Packet
     V1](../interfaces/task-packet-v1.md#field-rules) omitted from `Shared
     mutated surfaces`, or any superseded code/test or affected generated
     contract (`api/`, `.sqlx/`, `crates/grpc-contracts`) omitted from
     `Superseded paths and generated contracts`;
   - any lint violation, unsupported verified bound, or test-only `pub` leak.
     Assess unproved bounds under the author's [uncertainty disposition](technical-design.md#steps).

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

Use one fresh reviewer. Verify the accepted baseline and the exit conditions
in [Technical Design](technical-design.md#steps). Inspect primary code, `quality/architecture.json`, and non-obvious
`Cargo.lock` sources for every obligation in check **2**'s scope. Try to
construct **one realistic two-engineer divergence** (mechanism, unpinned
identifier, error/retryability, config rule, rollout path, owner, or seam) or
**one primary-source contradiction**, verify the forced-owner handoff package
required by [No-Open-Decisions
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
- `FAIL`: any surviving witnessed defect or missing decision-critical input. Return `PHASE_DEFECT` or
  `UPSTREAM_GAP` with anchor, concrete witness, and smallest owner (`Technical
  Design`, `Specification`, `Intake`, or external authority).
