# Implementation

Use when accepted implementation work is ready and authorized. Fresh Leads
implement the planned units and their tests in parallel; one fresh Delivery
Owner validates the assembled candidate at the end.

| Bound as | Follow |
| --- | --- |
| Ledger Orchestrator for a ready `tasks.md` | [Carrier](#carrier), then [Orchestrator Loop](#orchestrator-loop) |
| Acceptance-Unit Lead for one unit or repair | [Unit Implementation](implementation/unit.md) |
| Delivery Owner for the assembled candidate | [Final Validation](implementation/final-validation.md) |

## Carrier

In the session started to execute a plan, open the named `tasks.md` and check
its current state under [Resume](../shared/resume.md). Report a completed
ledger as done and route an unready plan to its recorded owner. For a ready
ledger, bind this session as Ledger Orchestrator (`LEDGER_ORCHESTRATOR`),
including for a one-task ledger.

A fixed inline unit has no ledger. Its session is the root-local Lead: it
follows [Unit Implementation](implementation/unit.md), then acts as Delivery
Owner under [Final Validation](implementation/final-validation.md).

## Orchestrator Loop

The Orchestrator routes, integrates, and writes canonical `tasks.md` state.
Each unit's code, tests, and validation belong to its Lead.

### 1. Verify Controls And Pin Baseline

- Confirm the selected [Agent Harness](../../agent-harness.md) adapter's
  full-ledger controls before any dispatch; report any capability gap that
  prevents full-ledger execution.
- Record `Base: <HEAD sha>` in `tasks.md` runtime state (or retain the recorded
  `Base` on resume) so worktrees, Final Validation, and Review share one exact
  diff baseline.
- Check checkout state before dispatching isolated worktrees: confirm
  uncommitted `specs/<task>/` inputs remain accessible from worktrees (passing
  the main checkout path when untracked/uncommitted) and no unrelated dirty
  edits block serial integration.

Complete this step only when the ledger is `ready`, controls are confirmed, and
`Base` is pinned.

### 2. Ready Frontier Dispatch

Start every task whose `Depends on` contracts or integrated `Implemented` code,
disjoint `Mutable owners` and `Exclusive locks`, and implementation authority
are present. When eligible tasks compete for the same lock or capacity, choose
by priority while keeping dispatched scopes and locks disjoint. Keep
`[acceptance]` and `[effect:<action>]` gates at final validation or release;
complete local contract-backed coding and leave later gates in the ledger.

- **Fresh Lead per task:** dispatch one fresh `acceptance-unit-lead` for each
  ready task up to capacity, reserving capacity for integration, review, and
  recovery. Record each Lead locator in the task's `Execution` field.
- **Checkout isolation:** share the checkout when only one mutable writer is
  active. When two or more mutable Leads run concurrently, use isolated
  worktrees anchored to the current integration tip whenever the adapter
  supports them (avoiding Cargo `target/` lock contention, transitive build
  breakage, and mixed `git diff`). When an adapter lacks worktrees, serialize
  overlapping crates and enforce path-scoped edits and checks without commits
  or workspace-wide formatting.
- **Sparse context-pointer brief:** pass pointers, not pasted specs or repo
  rules — the `tasks.md` path, the task packet path, the current integration
  tip (`HEAD` / `Base`), checkout mode (plus main checkout path for uncommitted
  `specs/<task>/`), landed `Depends on` commit/diff identities with their
  recorded `decisions`, and the stop boundary (return [Implementation
  Result](../interfaces/acceptance-result-v1.md#implementation-result); never
  edit `tasks.md` or start final validation).

Complete this step when no ready task with a disjoint scope waits while
capacity is free.

### 3. Acceptance Transition

Consume each Lead return as the sole writer of canonical ledger state:

| Return or Event | Orchestrator Action |
| --- | --- |
| `Implemented` (clean) | Verify returned paths/candidate lie inside granted scope and locks. Integrate serially into the integration tip (merge the worktree branch or confirm the shared-checkout diff). Check `[x]`, record one replaceable `Implemented` receipt with its `decisions` and `unverified`, release its locks, **retire the Lead and remove its worktree**, and return to Step 2. |
| `Implemented` (semantic merge conflict) | Keep the Lead alive; send it the conflict against the current integration tip to resolve and return a fresh `Implemented` result. Invalidate affected proof. |
| Mid-task scope expansion request | Grant only if requested paths and locks are disjoint from active writers. Update locks and reply one-to-one; Planning stays closed. |
| `Blocked` | Keep `[ ]`, retain partial code and the exact missing input, release unused locks, and route [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) while unaffected tasks continue. |
| Late/duplicate return, interrupted integration, or mid-task steering/invalidation | Load [Orchestrator Recovery](implementation/orchestrator-recovery.md). |

Preserve external-effect boundaries (never push, deploy, or touch auto-deploy
branches without explicit authority). Keep `status: ready` while coding, final
validation, owner-held repair, or an authorized external step (including a
running check) can advance; reserve `status: blocked` for genuinely unavailable
required inputs, capabilities, or authority.

### 4. Final Validation And Repair Routing

- **Entry gate:** enter only after every planned code task in `tasks.md` is
  `[x]` (`Implemented` and integrated) with zero active writers. Run local
  final validation immediately even when an external `[effect:<action>]` gate
  is pending.
- **Fresh Delivery Owner:** dispatch one fresh `acceptance-unit-lead` bound to
  [Final Validation](implementation/final-validation.md) that implemented none
  of the ledger's units (including for a one-task ledger). Brief it with sparse
  pointers: `tasks.md`, the pinned `Base..HEAD` candidate identity, and
  `spec.md` / `design/` / `rollout.md` paths when present.
- **Repair routing:** when the Delivery Owner returns a [Repair
  Request](../interfaces/acceptance-result-v1.md#repair-request)
  (`NeedsRepair`), dispatch one **fresh** `acceptance-unit-lead` in [Repair
  Assignment](implementation/unit.md#repair-assignment) mode against the
  current assembled candidate (raising capability tier per the adapter if
  review exposed a missed invariant; granting one Lead a combined writable
  scope across both sides of an `INTEGRATION_DEFECT` seam while no conflicting
  writer is active). Integrate the repair serially, retire the repair Lead, and
  resume the Delivery Owner with the new candidate identity and exact repair
  delta.

Complete this step when the Delivery Owner returns a terminal [Completion
Result](../interfaces/acceptance-result-v1.md#completion-result) (`Accepted` or
`Blocked`).

### 5. Close

Record the Delivery Owner's `Completion Result` in `tasks.md`:

- **On `Accepted`:** set `status: done`, apply [Cleanup](../shared/cleanup.md)
  (migrating any durable decisions to canonical owners before removing
  execution-only artifacts), and report impact first alongside verified scope,
  any pending CI or external outcomes, material optional gaps, and non-blocking
  `FOLLOW_UP` items.
- **On `Blocked`:** set `status: blocked` with `Blocked stop:` naming the exact
  missing input, capability, or authority and its owner while preserving useful
  work.
