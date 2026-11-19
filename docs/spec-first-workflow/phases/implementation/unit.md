# Unit Implementation

Implement the packet's unit-local `Outcome` and `Provides`. The Delivery
Owner proves its assembled `Observable claim`.

### 1. Ground In Primary Sources

Read the entire assigned packet, ledger constraints, relevant landed decisions
and prerequisite, shared-lock and same-crate changes. Start from the brief's
integration tip. Inspect cited sources, direct callers and a local sibling
before tests. Identify the seam, base deficiency (green baseline for a
prefactor), reusable owners, focused proving command and any active
`Reopen if`. Route an already-satisfied Outcome through [Escalation](#escalation).

For Rust edits use [Rust Change Surface](../../../../AGENTS.md#rust-change-surface)
and the matching domain skills. Load [Sub-Lanes](sub-lanes.md) when independent
subtasks justify delegation.

### 2. Build In Tight Vertical Slices

Use the [packet shape](../../interfaces/task-packet-v1.md#field-rules) to
choose the smallest observable slice. For new behavior or rejection, establish
the seam and observe a compiled assertion fail for the intended reason before
implementing the fix. A minimal stub is appropriate only for the first slice
of a new entry point. Reuse adequate tests for preserved behavior. Check that
the test would catch an inverted or deleted branch. Use focused Cargo checks
and tests with `--locked` for Rust. Confirm selected tests actually executed.

For an integration-first slice, exercise production wiring through the named
seam and route a falsified assumption to its reopen owner. Keep prefactors
and migration batches green. Check a shared contract at its compiling boundary,
new and old forms on expansion, and the withdrawn path plus surviving
guarantees on removal or contraction. For a quantitative or non-code outcome,
use its accepted witness. For generated surfaces follow [Validation
Routing](../../../validation-routing.md).

### 3. Pre-Return Self-Audit And Return

- Verify the unit's Outcome, downstream Provides, rejection paths and preserved
  guarantees at their assigned boundaries. Remove superseded in-scope paths,
  check the diff against the granted owners and locks, and apply
  [Engineering](../../../../AGENTS.md#engineering). Exclude the task bundle
  and unrelated edits.
- Join delegated lanes. Leave shared-checkout edits uncommitted. In isolation,
  commit scoped changes on the unit branch, merge the current integration tip
  if it advanced, and inspect newly landed sibling changes for divergence on
  shared seams. On conflict use [Merge Conflict Resolution](../../../../.agents/skills/merge-conflict-resolution/SKILL.md).
  Recheck affected surfaces and return a clean fast-forwardable candidate.
- For touched Rust crates, check formatting, lint and package tests or the
  proving consumer if a crate has no tests. Count executed tests and record unavailable checks with their exact reason. For other surfaces select the
  applicable checks. Keep assembled validation with the Delivery Owner.
- Return the [Implementation Result](../../interfaces/acceptance-result-v1.md#implementation-result)
  with downstream decisions and exact remaining proof gaps.

## Escalation

Repair a local oracle, fixture, caller or runner within scope. Get any path or
lock expansion from the Orchestrator before editing. For an ungranted Cargo
crate or feature, preserve the manifests and lockfile and return the dependency
decision to the Orchestrator. When live evidence invalidates behavior,
architecture or the planned delta, finish independent work and return
`Blocked` with the evidence, preserved work and smallest decision owner.
