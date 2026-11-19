# Unit Implementation

Use as Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) for one assigned unit. The
Lead turns one task packet into code and tests meeting the [Implemented
bar](../../interfaces/acceptance-result-v1.md#implementation-result). The
Orchestrator owns sibling scheduling and ledger writes; [Final
Validation](final-validation.md) owns proof of the assembled candidate.

If dispatched on a Final Validation [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request), follow
[Repair Assignment](#repair-assignment) instead of Steps 1–3.

## Steps

### 1. Ground In Primary Sources

- Read the **entire** task packet (`Outcome`, `Depends on`, `Sources`,
  `Boundary`, `Mutable owners`, `Exclusive locks`, `Final validation`,
  `Reopen if`) and `tasks.md`'s **`Global constraints`**, plus any landed
  dependency commits and `decisions` in your brief.
- Confirm your checkout contains integrated `Depends on` outputs. In an
  isolated worktree, confirm `HEAD` matches the integration tip in your brief
  and reset or rebase onto it if the harness branched from a stale ref.
- Read the affected production code, entry points, downstream callers, and
  cited `Sources` (**primary source**) before reading tests. Verify every
  `Reopen if` condition against live code or locked `Cargo.lock` crate source
  before editing.
- For Rust changes, load `rust-coder` and only the `.agents/skills/rust-*`
  methods whose trigger matches an open decision in this packet
  ([Rust Change Surface](../../../../AGENTS.md#rust-change-surface)).

Complete this step only when you can name the exact public **seam** (entry
point or caller observing the change), confirm current code lacks or fails
`Outcome`, and verify no `Reopen if` or upstream contradiction blocks coding.

### 2. Build In Tight Vertical Slices

- **Pin the seam first:** when adding or changing a shared Rust **seam**
  (public types, traits, signatures, or schema), land the boundary declarations
  first and verify with `cargo check` so code and tests compile against checked
  types.
- **Work red → green in vertical slices:** build one behavior or invariant at a
  time through the real **seam** instead of writing all production code first
  and backfilling tests afterward. Consult [Rust
  Testing](../../../../.agents/skills/rust-testing/SKILL.md) to select the
  smallest proving layer and minimal fixtures (reusing an existing minimal
  fixture or constructing only the state the assertion needs in test code, and
  removing unused setup you touch).
- **Independent oracle, never tautologies:** derive expected values from the
  accepted spec, protocol, or known literals — never by recomputing the
  implementation's formula or mocking away the boundary under claim. For bug
  fixes and new observable behavior, write a failing **red** test at the seam
  first, watch it fail for the targeted break, then write the minimal clean
  Rust code to turn it **green**. Where wiring must exist before a test
  compiles, verify the new or consolidated guard actually fails when the
  guarded behavior is broken. Preserve independent safety and integration
  guards when consolidating overlapping cases.
- **Tight feedback loop:** type-check and lint as you edit (`cargo check -p
  <crate> --all-targets --locked` or `make lint-changed PKGS="<crates>"` per
  [Validation budget](../../../../AGENTS.md#validation-budget)) and run focused
  single-test selectors (`cargo test -p <crate> --locked <selector>`),
  confirming **non-zero** executed tests (`0 passed` is never a pass). Add one
  cheap local boundary probe when an unverified assumption could cause
  substantial rework, and retain passing results for reuse under the [Evidence
  Contract](../../shared/evidence-contract.md#execution-evidence). Leave full
  workspace and heavy suites to Final Validation.
- **Generated contracts and sub-lanes:** regenerate (never hand-edit) generated
  contracts (`api/`, proto, `.sqlx/`). Implement directly by default; load
  [Sub-Lanes](sub-lanes.md) only when one unit clearly splits into parallel
  disjoint subtasks whose concurrency outweighs handoff cost.

### 3. When The Packet Does Not Fit

| Situation | Action |
| --- | --- |
| A local test, oracle, caller, error path, cleanup, locator, fixture, or runner needs repair | Repair it inside the unit. |
| The change needs a path outside granted `Mutable owners` or `Exclusive locks` | Send the scope delta to the Orchestrator **before** editing there (a root-local Lead decides directly). Continue once disjoint scope and updated locks are granted; Planning stays closed. |
| Live code or a `Reopen if` check exposes invalid accepted behavior or architecture | Finish unaffected in-scope work, then return `Blocked` naming the exact decision and smallest owner instead of redesigning silently. A root-local Lead reopens under [Transition](../../shared/transition.md). |

### 4. Pre-Return Self-Audit And Return

Verify every item before emitting [Implementation
Result](../../interfaces/acceptance-result-v1.md#implementation-result):

- [ ] `Outcome` and `Final validation -> Observable claim` are wired end-to-end
  through entry points and callers, with generated sources updated, superseded
  code/tests in `Boundary -> In scope` removed, and zero placeholders
  (`todo!()`).
- [ ] Own diff inspected (`git diff` scoped to own paths): every edited file
  lies inside granted `Mutable owners` / `Exclusive locks`; zero stray files,
  debug logs, explanatory comments, or non-test `unwrap`/`expect`/`panic!`. In
  a shared checkout with active siblings, leave commits and workspace-wide
  formatting to serial integration.
- [ ] Changed crates pass targeted lint and formatting (`make lint-changed
  PKGS="<crates>"` or `cargo check -p <crate> --all-targets --locked` plus
  `rustfmt` check on touched files).
- [ ] Every added or modified test executed with a **non-zero** pass count via
  a focused selector (or is named in `unverified` with its exact reason if it
  needs heavy or unavailable infrastructure).
- [ ] Any spawned sub-lanes have joined or stopped. In an isolated worktree,
  changes are cleanly committed on the worktree branch (with the integration
  tip merged if advanced) and `candidate` names that commit/branch.
- [ ] `decisions` records every non-obvious local choice, trade-off, or packet
  deviation the Delivery Owner cannot read from the diff; `unverified` records
  every unchecked assumption or unrun test.

Return `Implementation Result` immediately so the Orchestrator can integrate
and unlock downstream work. A root-local inline Lead proceeds directly to
[Final Validation](final-validation.md).

## Repair Assignment

Use when dispatched fresh from Final Validation on a [Repair
Request](../../interfaces/acceptance-result-v1.md#repair-request) against the
assembled candidate:

1. **Ground and reproduce (`red`).** Read the Repair Request (falsified claim,
   classification, anchored evidence), the affected packet(s), and prior
   `decisions`. For product, test, or seam defects, reproduce the exact failing
   check or failure path on the assembled candidate before editing.
2. **Diagnose root cause.** Treat any remedy suggested by a review finding as
   evidence, not a mandate. Weigh what a fix adds or trades away: repair at the
   shared root without weakening guards, adding unjustified waivers, or
   introducing new crates, layers, config keys, or fallbacks
   ([Engineering](../../../../AGENTS.md#engineering)). For
   `INTEGRATION_DEFECT`, reconcile both sides inside the Orchestrator-granted
   combined writable scope.
3. **Verify (`green`) and audit.** Confirm the reproduced check passes with
   non-zero executed tests, complete the Step 4 Pre-Return Self-Audit, and
   record in `decisions` both the root cause and the exact invalidated surface
   so Final Validation reruns only affected checks.
