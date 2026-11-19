# Unit Implementation

Use as Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) for one assigned unit.
Turn one task packet into code and tests meeting Step 3's `Implemented` bar.
Orchestrator owns sibling scheduling and `tasks.md`; [Final
Validation](final-validation.md) owns proof of the assembled candidate.

For a Final Validation [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request), follow
[Repair Assignment](repair.md) instead.

## Steps

### 1. Ground In Primary Sources

- Read the **entire** task packet, `tasks.md` (`Global constraints` and landed
  `decisions` for touched crates), and brief-provided prerequisite/shared-lock
  commits and `decisions`.
- Confirm your checkout holds integrated prerequisites; in an isolated
  worktree, reset or rebase onto the brief's integration tip if `HEAD` is
  stale.
- Read affected production code, callers, prerequisite diffs, one existing
  sibling in the same crate/subsystem (error mapping, telemetry, test helpers),
  and cited `Sources` (**primary source**) before reading tests.
- **Reuse before coding:** check `std`, locked `Cargo.lock` crate modules and
  features, and existing workspace helpers so you delegate generic mechanics
  instead of writing custom wrappers. Verify every `Reopen if` condition
  against live code or locked `Cargo.lock` source.
- For Rust changes, load `rust-coder`, load [Rust
  Testing](../../../../.agents/skills/rust-testing/SKILL.md) when authoring or
  changing tests, and load only other `.agents/skills/rust-*` methods whose
  trigger matches an open decision in this packet ([Rust Change
  Surface](../../../../AGENTS.md#rust-change-surface)).

Before your first edit, state four concrete facts in your working plan:
(1) exact public **seam**(s) and task shape (`vertical slice`, `prefactor`,
`shared-contract`, `integration-first`, `removal`, or
`expand/migrate/contract`); (2) what base code lacks or fails (or the green
baseline for a `prefactor` — if current code already satisfies `Outcome` with
zero delta, return `Blocked` with primary evidence instead of inventing edits);
(3) local sibling pattern and `std`/`Cargo.lock`/workspace owners being reused;
and (4) `Reopen if` clear.

### 2. Build In Tight Vertical Slices

Match the proving loop to the task shape:

- **Behavioral slice (default — `red -> green` tracer bullets):** land new or
  changed public Rust **seams** first (`cargo check -p <crate> --all-targets
  --locked`), then build one behavior, preserved `G<n>`, or rejection path at a
  time through the seam — avoid **horizontal slicing** (writing all code before
  the first `red` test, or batching all tests before the first `green` slice).
  Derive expected values from an independent source of truth (spec, protocol
  vector, or worked literal). **`red` in Rust is a compiled test failing for
  the targeted break — never a compile error:** only the first slice of a
  brand-new entry point may hit a minimal stub; every subsequent slice must
  fail its targeted assertion against live code before you write the minimal
  clean fix to turn it `green`.
- **Non-default shapes:** for a `prefactor` (**`green -> green`**), verify
  existing seam tests pass first and keep them green across small steps with
  zero new feature behavior; for a `shared-contract`, land minimal compiling
  contracts without placeholder panics and verify type/caller invariants; for a
  `removal` / `contract`, prove withdrawn behavior is unreachable and drop
  obsolete tests while keeping non-fungible safety guards intact.
- **Tight feedback loop:** iterate with `cargo check -p <crate> --all-targets
  --locked` and focused test selectors (`cargo test -p <crate> --locked
  <selector>`), confirming **non-zero** executed tests (`0 passed` is never a
  pass; in a worktree, confirm tests needing untracked fixtures did not
  silently skip). Load [Evidence
  Contract](../../shared/evidence-contract.md#execution-evidence) only when
  claiming result reuse or handling unavailable test infrastructure.
- **Generated contracts and sub-lanes:** regenerate (never hand-edit) `api/`,
  proto, and `.sqlx/`. Implement directly by default; load
  [Sub-Lanes](sub-lanes.md) only when parallel disjoint subtasks clearly
  outweigh handoff cost.

Complete this step only when every `Outcome` obligation, rejection path, and
preserved `G<n>` completed its shape's proving cycle and changed crates compile
cleanly.

### 3. Pre-Return Self-Audit And Return

Verify every gate before emitting [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result):

- [ ] **End-to-end wiring:** `Outcome`, active `Observable claim` (or `Outcome`
  alone when `Observable claim` is `superseded at assembly by T<contract>`),
  preserved `G<n>`, and rejection paths are wired through the seam, and
  generated contracts are regenerated.
- [ ] **Scope and cleanup:** this unit's edited paths stay inside granted
  `Mutable owners` and `Exclusive locks` (excluding `specs/<task>/` and any
  prior landed paths in `no-commit` mode), superseded in-scope code/tests are
  deleted, and zero stray untracked files remain.
- [ ] **Manual diff hygiene (beyond `clippy`):** zero explanatory comments,
  zero `#[non_exhaustive]` on workspace public types ([Rust Change
  Surface](../../../../AGENTS.md#rust-change-surface)), zero temporary debug
  probes/logging, and zero unjustified policy waivers.
- [ ] **Deletion test:** every added type, trait, helper, config key, or
  fallback enforces a present packet constraint with no existing owner
  ([Engineering](../../../../AGENTS.md#engineering)).
- [ ] **Crate lint and format:** changed crates pass `make lint-changed
  PKGS="<crates>"` and `rustfmt --check` on touched files.
- [ ] **Crate test suite (non-zero):** changed crates (or the proving consumer
  crate when a touched crate holds zero tests) pass `make test-package
  PKG=<crate>` (or `make test-changed PKGS="<crates>"`) with **non-zero**
  counts (record any test needing heavy/unavailable infra in `unverified` with
  its exact reason).
- [ ] **Landing state:** sub-lanes joined/stopped. In an isolated worktree,
  merge `<integration-branch>` if its tip advanced (loading [Merge Conflict
  Resolution](../../../../.agents/skills/merge-conflict-resolution/SKILL.md) on
  conflicted hunks), re-run crate lint and tests, commit cleanly, and set
  `candidate` to `<branch@sha>` (fast-forwardable). In a shared checkout, leave
  scoped edits uncommitted for Orchestrator and set `candidate` to changed
  paths.
- [ ] **Handoff signal:** `decisions` records everything a sibling, downstream
  unit, or Delivery Owner would otherwise have to read your diff to guess
  (symbol/error names, defaults, new shared test helpers, packet deviations);
  `unverified` lists every unchecked assumption or unrun test.

Return `Implementation Result` immediately. A root-local inline Lead proceeds
directly to [Final Validation](final-validation.md).

## Escalation

| Situation | Action |
| --- | --- |
| Local test, oracle, caller, error path, cleanup, locator, fixture, or runner needs repair | Repair inside the unit. |
| Change needs a path outside granted `Mutable owners` or `Exclusive locks` | Request scope delta from Orchestrator **before** editing (returning `Blocked` with `next_owner: Ledger Orchestrator — scope expansion: <paths>` if the harness requires a turn return; root-local Lead decides directly); continue once granted (Planning stays closed). |
| Live code, `Reopen if`, or already-satisfied `Outcome` exposes invalid accepted behavior, invalid architecture, or zero delta | Finish unaffected in-scope work, preserve partial edits (WIP commit on the worktree branch when isolated), and return `Blocked` naming exact decision/evidence and smallest owner (root-local Lead reopens under [Transition](../../shared/transition.md)). |
