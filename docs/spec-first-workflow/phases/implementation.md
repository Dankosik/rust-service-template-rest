# Implementation

Use when accepted implementation work is ready and authorized. Fresh Leads
implement planned units and their tests in parallel; one fresh Delivery Owner
validates the assembled candidate at the end.

| Bound as | Follow |
| --- | --- |
| Ledger Orchestrator for a ready `tasks.md` | [Carrier](#carrier), then [Orchestrator Loop](#orchestrator-loop) |
| Acceptance-Unit Lead for one unit | [Unit Implementation](implementation/unit.md) |
| Acceptance-Unit Lead for a Final Validation repair | [Repair Assignment](implementation/repair.md) |
| Delivery Owner for the assembled candidate | [Final Validation](implementation/final-validation.md) |

## Carrier

Open the named `tasks.md`. Load [Resume](../shared/resume.md) only when
`tasks.md` already has runtime state (`Base:`, `[x]`, `Execution:`, `Resume:`)
or after interruption or compaction. Report a completed ledger as done and
route an unready plan to its recorded owner. For a ready ledger, bind this
session as Ledger Orchestrator (`LEDGER_ORCHESTRATOR`), including for a
one-task ledger.

A fixed inline unit has no ledger: this session is the root-local Lead,
running [Unit Implementation](implementation/unit.md) then [Final
Validation](implementation/final-validation.md).

## Orchestrator Loop

The Orchestrator routes, integrates, and writes canonical `tasks.md` state;
Leads own code, tests, repairs, and validation.

### 1. Verify Controls And Pin Baseline

- Confirm the [Agent Harness](../../agent-harness.md) adapter's full-ledger
  controls before dispatching; report any blocking capability gap.
- Pin `Base: <HEAD sha>` in `tasks.md` (or keep the recorded `Base` on resume)
  and confirm a writable integration branch with no unrelated dirty edits.
- **Commit mode:** by default, every integrated unit lands as a clean local
  commit on the integration branch so `HEAD` advances monotonically, worktrees
  receive landed `Depends on` commits, and `<Base>..HEAD` is always exact. If
  the user forbade local commits, record `Commit mode: no-commit`, serialize
  in shared checkout without worktrees, and use `git diff <Base>` everywhere
  below.
- Note the main checkout path when `specs/<task>/` is untracked or uncommitted
  so worktrees can read it.

Complete this step only when the ledger is `ready`, controls are confirmed, and
`Base`, integration branch, and commit mode are pinned.

### 2. Ready Frontier Dispatch

Inspect eligible tasks in `tasks.md` and read `Mutable owners` and `Exclusive
locks` from each ready packet before dispatching. Start every task whose
`Depends on` contracts or integrated code, disjoint `Mutable owners` and
`Exclusive locks`, and authority are met (breaking ties by priority; leaving
`[acceptance]` and `[effect:<action>]` gates for Final Validation or release).

- **Fresh Lead per task:** dispatch one fresh `acceptance-unit-lead` per ready
  task up to capacity (reserving slots for integration, review, and recovery),
  recording its locator in `Execution`.
- **Checkout isolation:** share the checkout for a single active writer; use
  isolated worktrees anchored to the integration tip for two or more concurrent
  writers under [Agent Harness](../../agent-harness.md#delegation-interface).
- **Unit Lead brief:**
  - `Method: Unit Implementation` (`docs/spec-first-workflow/phases/implementation/unit.md`)
  - `tasks.md` and packet paths (+ main checkout path if `specs/<task>/` is uncommitted)
  - integration branch name, current tip SHA (`HEAD`), and checkout mode
  - landed `Depends on` commit SHAs and their recorded `decisions`
  - stop boundary: return [Implementation Result](../interfaces/acceptance-result-v1.md#implementation-result)

Complete this step when no ready task with disjoint scope waits while capacity
is free.

### 3. Acceptance Transition

| Return or Event | Orchestrator Action |
| --- | --- |
| `Implemented` (clean) | Verify changed paths lie inside granted `Mutable owners` and `Exclusive locks`. Land serially onto the integration branch (merge worktree branch or commit scoped shared-checkout diff). Check `[x]`, record one replaceable `Implemented` receipt carrying `decisions` and `unverified` verbatim, release locks, clear `Execution`, retire Lead and worktree, and return to Step 2. |
| `Implemented` (merge or semantic conflict) | Keep Lead alive in its worktree/checkout; send the conflict against the integration tip so it merges, resolves, re-runs focused checks, and returns a fresh `Implemented` result. |
| Mid-task scope expansion request | Grant only if requested paths and locks are disjoint from active writers; update locks and reply one-to-one (Planning stays closed). |
| `Blocked` | Keep `[ ]`, retain partial work and missing input, release unused locks, and route [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) while unaffected tasks continue. When no ready or active tasks remain and any required task stays `Blocked`, go to Step 5. |
| Late/duplicate return, interrupted integration, or mid-task steering/invalidation | Load [Orchestrator Recovery](implementation/orchestrator-recovery.md). |

Never push, deploy, or touch auto-deploy branches without explicit authority.

### 4. Final Validation And Repair Routing

- **Entry gate:** enter only once every planned code task is `[x]` with zero
  active writers (run local Final Validation even if `[effect:<action>]` is
  pending).
- **Fresh Delivery Owner:** dispatch one fresh `acceptance-unit-lead` that
  implemented none of the ledger's units (including one-task ledgers) with:
  - `Method: Final Validation` (`docs/spec-first-workflow/phases/implementation/final-validation.md`)
  - `tasks.md`, integration branch name, and candidate diff (`<Base>..HEAD`)
  - accepted sources present (`intent.md`, `spec.md`, `design/`, `rollout.md`, packets)
- **Repair routing (`NeedsRepair`):** dispatch one fresh `acceptance-unit-lead`
  with:
  - `Method: Repair Assignment` (`docs/spec-first-workflow/phases/implementation/repair.md`)
  - verbatim [Repair Request](../interfaces/acceptance-result-v1.md#repair-request), integration branch, `Base`, and `HEAD`
  - affected packet paths, prior `decisions`, and combined writable scope across named owning tasks (raising capability tier per adapter when review exposed a missed invariant)
  Land the repair serially onto the integration branch, retire the repair Lead
  and worktree, and resume Delivery Owner with the new `HEAD`, repair range
  (`<old-HEAD>..<new-HEAD>`), and repair `decisions`.
- **Delivery Owner `Blocked`:** close `next_owner` under [Parent-Owned
  Recovery](../shared/transition.md#parent-owned-recovery) if within
  Orchestrator authority and resume Delivery Owner; otherwise go to Step 5.

Complete this step when Delivery Owner returns terminal [Completion
Result](../interfaces/acceptance-result-v1.md#completion-result) (`Accepted` or
unresolvable `Blocked`).

### 5. Close

Record the terminal result in `tasks.md`:

- **On `Accepted`:** set `status: done`, apply [Cleanup](../shared/cleanup.md),
  and report impact first alongside verified scope, pending CI or external
  outcomes, material optional gaps, and `FOLLOW_UP` items.
- **On `Blocked` (from Step 3 or Step 4):** set `status: blocked` with
  `Blocked stop:` naming the exact missing input, capability, or authority and
  its owner while preserving useful work.
