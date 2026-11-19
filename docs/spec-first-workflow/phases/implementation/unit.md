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

- Read the **entire** task packet, `tasks.md`'s **`Global constraints`**, and
  landed `Depends on` commits and `decisions` from your brief.
- Confirm your checkout holds integrated `Depends on` outputs; in an isolated
  worktree, reset or rebase onto the brief's integration tip if `HEAD` is
  stale.
- Read affected production code, entry points, callers, landed `Depends on`
  diffs, one existing sibling in the same crate or subsystem (error mapping,
  telemetry, test helpers), and cited `Sources` (**primary source**) before
  reading tests. Verify every `Reopen if` condition against live code or locked
  `Cargo.lock` source before editing.
- For Rust changes, load `rust-coder`, load [Rust
  Testing](../../../../.agents/skills/rust-testing/SKILL.md) when authoring or
  changing tests, and load only the other `.agents/skills/rust-*` methods whose
  trigger matches an open decision in this packet ([Rust Change
  Surface](../../../../AGENTS.md#rust-change-surface)).

Complete this step only when you can name the exact public **seam**, confirm
current code lacks or fails `Outcome`, align with local crate conventions, and
confirm no `Reopen if` or upstream contradiction blocks coding.

### 2. Build In Tight Vertical Slices

- **Pin the seam first:** land new or changed public Rust **seams** (types,
  traits, signatures, or schema, with minimal failing stubs for new entry
  points) and verify with `cargo check -p <crate> --all-targets --locked` so
  tests compile against real types.
- **Red -> green vertical slices (tracer bullets):** build one behavior,
  preserved guarantee (`G<n>`), or failure/rejection path at a time through the
  public **seam**. Derive expected values from the spec, protocol, or known
  literals — never by recomputing the implementation's formula or mocking away
  the boundary under claim. **`red` in Rust means a compiled test failing an
  assertion or hitting a stub at the seam — never a compile error.** Watch it
  fail for the targeted break, write the minimal clean code to turn it `green`,
  then repeat.
- **Tight feedback loop:** iterate with `cargo check -p <crate> --all-targets
  --locked` and focused test selectors (`cargo test -p <crate> --locked
  <selector>`), confirming **non-zero** executed tests (`0 passed` is never a
  pass; in a worktree, verify tests needing untracked or gitignored fixtures
  did not silently skip). Retain valid results under [Evidence
  Contract](../../shared/evidence-contract.md#execution-evidence); leave
  full-workspace and heavy suites to Final Validation.
- **Generated contracts and sub-lanes:** regenerate (never hand-edit) generated
  contracts (`api/`, proto, `.sqlx/`). Implement directly by default; load
  [Sub-Lanes](sub-lanes.md) only when parallel disjoint subtasks clearly
  outweigh handoff cost.

Complete this step only when every `Outcome` obligation, failure/rejection
path, and preserved `G<n>` invariant passed a seam-level `red -> green` cycle
and changed crates compile cleanly.

### 3. Pre-Return Self-Audit And Return

Verify every gate before emitting [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result):

- [ ] `Outcome`, `Observable claim`, preserved `G<n>` invariants, and
  failure/rejection paths are wired end-to-end; generated contracts are
  regenerated; superseded code/tests in `Boundary -> In scope` are removed;
  zero placeholders (`todo!()`, `unimplemented!()`).
- [ ] Own `git diff` inspected: every edited path lies inside granted `Mutable
  owners` / `Exclusive locks`; zero stray files, debug logs, explanatory
  comments, or non-test `unwrap`/`expect`/`panic!`.
- [ ] **Deletion test:** every added type, trait, helper, config key, or
  fallback enforces a present packet constraint and has no matching `std` or
  `Cargo.lock` owner ([Engineering](../../../../AGENTS.md#engineering)).
- [ ] Changed crates (and in-scope callers) pass `make lint-changed
  PKGS="<crates>"` (clippy) and `rustfmt` check on touched files.
- [ ] Every added or modified test ran with a **non-zero** pass count via a
  focused selector (or is named in `unverified` with its exact reason if it
  needs heavy/unavailable infrastructure or untracked local fixtures).
- [ ] Sub-lanes joined or stopped. In an isolated worktree, merge the
  integration branch tip if it advanced, re-run focused checks after merge,
  commit cleanly, and set `candidate` to that commit/branch. (In a shared
  checkout with active siblings, leave commits and workspace-wide formatting to
  serial integration.)
- [ ] `decisions` lists every choice a sibling, downstream unit, or Delivery
  Owner may depend on (error variant/code, precedence/default, new shared test
  helper/fixture, packet deviation, narrowed assumption); `unverified` lists
  every unchecked assumption or unrun test.

Return `Implementation Result` immediately. A root-local inline Lead proceeds
directly to [Final Validation](final-validation.md).

## Escalation

| Situation | Action |
| --- | --- |
| Local test, oracle, caller, error path, cleanup, locator, fixture, or runner needs repair | Repair inside the unit. |
| Change needs a path outside granted `Mutable owners` or `Exclusive locks` | Request the scope delta from Orchestrator **before** editing (root-local Lead decides directly); continue once granted (Planning stays closed). |
| Live code or `Reopen if` exposes invalid accepted behavior or architecture | Finish unaffected in-scope work, then return `Blocked` naming the exact decision and smallest owner (root-local Lead reopens under [Transition](../../shared/transition.md)). |
