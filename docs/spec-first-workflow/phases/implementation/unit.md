# Unit Implementation

Use as Unit Lead (`ACCEPTANCE_UNIT_LEAD`) for one assigned unit. Turn one task
packet into code and tests meeting Step 3's `Implemented` bar. Orchestrator
owns scheduling and `tasks.md`; [Final Validation](final-validation.md) owns
assembled proof.

For a Final Validation repair, follow [Repair Assignment](repair.md). For
`no-commit` or inline-unit overrides, follow [Execution Modes](modes.md).

## Steps

### 1. Ground In Primary Sources

- Read the **entire** packet, `tasks.md` (`Global constraints` and landed
  `decisions` for touched crates), and brief-provided prerequisite,
  shared-lock, and same-crate commits.
- In an isolated worktree, reset or rebase onto the brief's integration tip if
  `HEAD` lags it.
- Read affected production code, direct callers, prerequisite diffs, one local
  sibling (error mapping, telemetry, test helpers), and cited `Sources` before
  reading tests; treat `Sources` as surgical pointers rather than open scans.
- Check `std`, locked `Cargo.lock` modules/features, and workspace helpers for
  reuse, and verify every `Reopen if` condition against live code or locked
  `Cargo.lock` source.
- For Rust changes, load `rust-coder`, [Rust
  Testing](../../../../.agents/skills/rust-testing/SKILL.md) when authoring or
  changing tests, and only other `.agents/skills/rust-*` skills matching an
  open decision ([Rust Change Surface](../../../../AGENTS.md#rust-change-surface)).

Before your first edit, ground five concrete facts:

1. Exact public **seam**(s) and task shape.
2. What base code lacks or fails today (or green baseline for `prefactor`; if
   `Outcome` already holds with zero delta, return `Blocked` with primary
   evidence).
3. Local sibling pattern and `std` / `Cargo.lock` / workspace owners reused.
4. Exact seam test file/module and fast tight-loop command
   (`cargo test -p <crate> --locked <selector>`).
5. Every `Reopen if` condition verified clear.

### 2. Build In Tight Vertical Slices

#### Default Behavioral Slice (`red -> green` Tracer Bullets)

1. **Land the seam first:** add or update public Rust seam signatures and check
   with `cargo check -p <crate> --all-targets --locked`.
2. **One vertical slice at a time (`red -> green`):** write one seam test for
   one behavior, preserved `G<n>`, or rejection path and watch it fail for the
   targeted assertion (**`red` in Rust is a compiled assertion failure against
   live code, never a compile error** — only a brand-new entry point's first
   slice may hit a minimal stub). Write only the minimal clean code to pass it
   (`green`), delegating generic mechanics to `std`, locked `Cargo.lock`
   crates, or existing workspace owners before adding any type, trait, or
   helper.
3. **Fault-sensitive tests from the start:** assert concrete caller- or
   operator-visible outcomes (status, body/Problem code, state/side-effect,
   exact error variant) against spec or worked literals — never `is_ok()`-only
   or recomputed like production code. Before calling a slice `green`, verify
   mentally that inverting or deleting the guarded branch turns the test `red`.
4. **Tight loop:** iterate with `cargo check -p <crate> --all-targets --locked`
   and focused `cargo test -p <crate> --locked <selector>`, confirming
   **non-zero** executed tests (`0 passed` is never a pass; in a worktree,
   confirm fixture-dependent tests did not silently skip).

#### Non-Default Task Shapes

| Shape | Proving loop | Done signal |
| --- | --- | --- |
| `prefactor` | `green -> green` in small steps | Unlocked internal seam compiles and passes with zero new feature behavior |
| `shared-contract` | Compile and boundary checks without placeholder panics | Minimal compiling contract/schema meets every downstream `Provides` invariant |
| `removal` / `contract` | Prove withdrawn path/form unreachable; drop obsolete tests, keeping non-fungible safety guards | Removed path gone or compile-rejected; surviving invariants green |
| `expand` | Dual-form (`red -> green` on new form, `green` on old callers) | New form works while existing callers stay unbroken |
| `migrate` | Batch call-site migration (`green -> green`) | Preserved `G<n>` green with zero old-form references inside `Mutable owners` |

Regenerate (never hand-edit) `api/`, proto, and `.sqlx/`. Load [Evidence
Contract](../../shared/evidence-contract.md#execution-evidence) only for reuse
or unavailable test infra, and [Sub-Lanes](sub-lanes.md) only when parallel
disjoint subtasks outweigh handoff cost.

Done when every `Outcome` obligation, rejection path, and preserved `G<n>`
passed its shape's proving loop (observed `red -> green` on behavioral slices),
fault sensitivity holds, and changed crates compile cleanly.

### 3. Pre-Return Self-Audit And Return

Verify every gate in order before emitting [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result):

- [ ] **Wiring & contracts:** `Outcome`, active `Observable claim` (or
  `Outcome` alone when `superseded at assembly by T<contract>`), preserved
  `G<n>`, and rejection paths are wired through the seam; generated contracts
  are regenerated.
- [ ] **Scope, hygiene & deletion test:** edits stay inside granted `Mutable
  owners`/`Exclusive locks` (no `specs/<task>/`, no stray untracked files,
  superseded in-scope code/tests deleted); diff passes [Rust Change
  Surface](../../../../AGENTS.md#rust-change-surface) (no explanatory comments,
  no `#[non_exhaustive]` on workspace public types, no debug probes or
  unjustified waivers) and [Engineering](../../../../AGENTS.md#engineering)
  (every added structure enforces a present packet constraint with no existing
  owner).
- [ ] **Sync integration tip before final checks:** join or stop all sub-lanes.
  In an isolated worktree, **commit scoped task changes onto the worktree
  branch first**; if `<integration-branch>` advanced, run `git merge
  <integration-branch>` (loading [Merge Conflict
  Resolution](../../../../.agents/skills/merge-conflict-resolution/SKILL.md) on
  conflicted hunks) and scan newly landed sibling commits and `decisions` in
  touched crates for duplicated helpers or divergent naming/error shapes. In a
  shared checkout, leave scoped edits uncommitted for Orchestrator.
- [ ] **Final crate lint, format & non-zero tests:** run `rustfmt --check` on
  touched files, `make lint-changed PKGS="<crates>"`, and `make test-package
  PKG=<crate>` (or `make test-changed PKGS="<crates>"`, or proving consumer
  crate when a touched crate holds zero tests) with **non-zero** executed
  counts (record heavy/unavailable infra tests in `unverified` with exact
  reason). In a worktree, commit any post-merge adjustments cleanly so
  `candidate` is `<branch@sha>` (fast-forwardable); in a shared checkout, set
  `candidate` to changed paths.
- [ ] **Handoff signal:** fill `decisions` (only diff-independent facts per
  `Implementation Result`, omitting narration) and `unverified` verbatim.

Return `Implementation Result` immediately.

## Escalation

| Situation | Action |
| --- | --- |
| Local test, oracle, caller, error path, cleanup, locator, fixture, or runner needs repair | Repair inside the unit. |
| Change needs a path outside granted `Mutable owners` or `Exclusive locks` | Request scope delta from Orchestrator **before** editing (`Blocked` with `next_owner: Ledger Orchestrator — scope expansion: <paths>` if a turn return is required); continue once granted (Planning stays closed). |
| Change needs an ungranted Cargo crate or feature | Leave `Cargo.toml`/`Cargo.lock` untouched (never bypass `--locked`); preserve partial work and return `Blocked` to Orchestrator naming dependency gap and owner. |
| Live code, `Reopen if`, or already-satisfied `Outcome` exposes invalid behavior, invalid architecture, or zero delta | Finish unaffected in-scope work, preserve partial edits (WIP commit on worktree branch if isolated), and return `Blocked` naming exact decision/evidence and smallest owner. |
