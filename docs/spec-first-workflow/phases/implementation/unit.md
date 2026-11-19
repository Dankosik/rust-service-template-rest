# Unit Implementation

Implement the assigned task packet's unit-local `Outcome` and `Provides` cleanly
on top of the integration tip, then return an [Implementation
Result](#implementation-result). The Orchestrator owns assembled validation and
review.

## Steps

1. **Ground in primary sources.**
   - Read the entire assigned packet, `tasks.md` `Global constraints`, and
     relevant landed `decisions` or diffs from prerequisites and same-crate
     tasks.
   - Inspect cited sources, direct callers, and one local sibling before writing
     tests. Identify the seam, reusable owners, focused proving command, and any
     active `Reopen if`.
   - For Rust edits, follow [Rust Change
     Surface](../../../../AGENTS.md#rust-change-surface) and load only the
     domain `rust-*` skills matching open decisions.
   - *Done when:* the target seam, base deficiency (or green baseline for a
     prefactor), and focused check command are verified in live code.

2. **Build in tight vertical slices (red -> green).**
   - Follow the packet's [shape](../planning.md#task-shapes-and-granularity):
     - *New behavior or rejection:* establish the seam, observe a compiled
       assertion go **red** for the intended reason, implement the fix, and
       watch it turn **green**. Check that inverting or deleting the guarded
       branch would turn the test red again.
     - *Prefactor / migration batch:* keep existing seam tests green throughout.
     - *Integration-first:* wire real production code through the seam and
       escalate immediately if the risk assumption falsifies.
     - *Shared contract / expand / contract:* verify the compiling boundary,
       both forms on expansion, and the withdrawn path plus preserved `G<n>` on
       removal.
   - Run focused Cargo checks and tests with `--locked`, confirming selected
     tests actually executed. Use [Validation
     Routing](../../../validation-routing.md) for generated contracts (`api/`,
     `.sqlx/`, proto).
   - *Done when:* `Outcome`, downstream `Provides`, rejection paths, and
     preserved `G<n>` are implemented and green locally.

3. **Self-audit and return.**
   - Remove superseded in-scope paths, confirm the diff stays inside granted
     `Mutable owners` and `Exclusive locks` (excluding `specs/<task>/` and
     unrelated edits), and apply [Engineering](../../../../AGENTS.md#engineering).
   - For touched Rust crates, verify formatting, lint, and package tests (or
     the proving consumer if a crate has no tests); record any unavailable check
     with its exact reason in `unverified`.
   - In an isolated worktree, commit scoped changes on the unit branch, merge
     the current integration tip if it advanced (resolving conflicts via
     [Merge Conflict
     Resolution](../../../../.agents/skills/merge-conflict-resolution/SKILL.md)
     and rechecking affected seams), and return a clean fast-forwardable
     candidate. In a shared checkout, leave edits uncommitted.
   - *Done when:* [Implementation Result](#implementation-result) is returned
     with diff-independent `decisions` and exact `unverified` gaps.

## Escalation

- Request any path or lock expansion from the Orchestrator before editing,
  leaving the live worktree untouched (no WIP commit) so work can continue once
  granted.
- For an ungranted Cargo crate or feature, keep manifests and `Cargo.lock`
  untouched and return the dependency request to the Orchestrator.
- If live evidence shows `Outcome` already holds or invalidates behavior or
  architecture, preserve partial work (as a WIP commit in an isolated worktree)
  and return `Blocked` naming the exact evidence and smallest owner.

## Implementation Result

Return format for `ACCEPTANCE_UNIT_LEAD` (unit implementation and repair
passes):

```text
unit: <T<ID> or repair round>
verdict: Implemented | Blocked
candidate: <commit/branch, bounded diff, or none if blocked>
provides: <implemented contract; no acceptance claim>
decisions: <only diff-independent facts downstream needs: new/changed public symbols, error codes, defaults, test helpers, packet deviations, repair root causes/refutations; or none>
unverified: <unchecked assumptions or unrun tests with exact reason, or none>
next_owner: <Orchestrator, Orchestrator — scope expansion: <paths>, Orchestrator — refuted findings, or exact missing input and owner>
```

Return `Implemented` only after passing Step 3. On `Blocked`, preserve partial
work (as a WIP commit in an isolated worktree; keep the live worktree untouched
without a WIP commit for scope expansion).
