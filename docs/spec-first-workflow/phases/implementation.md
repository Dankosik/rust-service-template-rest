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

Open the named `tasks.md` and use [Ledger Runtime
State](../interfaces/acceptance-result-v1.md#ledger-runtime-state) for
ledger fields. Load [Resume](../shared/resume.md) only when `tasks.md` already
has runtime state (`Base:`, `[x]`, `Execution:`, `Resume:`) or after
interruption or compaction. Report a completed ledger as done and route an
unready plan to its recorded owner. For a ready ledger, bind this session as
Ledger Orchestrator (`LEDGER_ORCHESTRATOR`), including for a one-task ledger.

A fixed inline unit has no ledger: this session is the root-local Lead,
running [Unit Implementation](implementation/unit.md) then [Final
Validation](implementation/final-validation.md).

## Orchestrator Loop

The Orchestrator routes, integrates, and writes canonical `tasks.md` state;
Leads own code, tests, repairs, and validation.

### 1. Verify Controls And Pin Baseline

- Confirm the [Agent Harness](../../agent-harness.md) adapter's full-ledger
  controls, `tasks.md` `status: ready`, and that every referenced `Packet:`
  file exists; report any blocking capability gap.
- Pin `Base: <HEAD sha>` in `tasks.md` (keeping recorded `Base` on resume).
- **Integration branch and artifact hygiene:** if `HEAD` sits on `main` or
  detached `HEAD` without authority to land there, create a local integration
  branch from `Base` (for example, `impl/<task>`). Confirm no unrelated dirty
  edits block integration, and keep `specs/<task>/` untracked, unstaged, and
  out of every commit until Step 5 Cleanup.
- **Commit mode:** by default, the Orchestrator lands every unit as one clean
  local commit so `<Base>..HEAD` advances monotonically. If the user forbade
  local commits, record `Commit mode: no-commit`, serialize in the shared
  checkout, run `git add -N -- <newly-created-untracked-paths>` (strictly
  newly created paths, never deleted paths or `specs/<task>/`) after each unit
  so `git diff <Base>` sees new files without committing, track already-landed
  paths across tasks, and use `git diff <Base>` everywhere below.
- Note the main checkout path when `specs/<task>/` is uncommitted so worktrees
  can read it.

Complete this step only when the ledger is `ready`, controls and packets are
confirmed, and `Base`, integration branch, and commit mode are pinned.

### 2. Ready Frontier Dispatch

Read `Mutable owners` and `Exclusive locks` from each ready packet before
dispatching. Start every task whose `Depends on` outputs, disjoint locks, and
authority are met (breaking ties by priority in `tasks.md` order; leaving
`[acceptance]` and `[effect:<action>]` gates for Final Validation or release).

- **Fresh Lead per task:** dispatch one fresh `acceptance-unit-lead` per ready
  task up to capacity (keep concurrent compiling Rust worktrees bounded—typically
  2–3—and reserve slots for integration, review, and recovery), recording its
  locator in `Execution`.
- **Checkout isolation and branch ownership:** share the checkout for a single
  active writer or any task needing untracked/gitignored local fixtures
  (serializing it against concurrent writers); use isolated worktrees anchored
  to the integration tip for two or more concurrent writers under [Agent
  Harness](../../agent-harness.md#delegation-interface). The Orchestrator is
  the sole writer of the integration branch ref (worktree Leads commit on their
  worktree branch; shared-checkout Leads leave scoped uncommitted edits).
- **Unit Lead brief:**
  - `Method: Unit Implementation` (`docs/spec-first-workflow/phases/implementation/unit.md`)
  - `tasks.md` and packet paths (+ main checkout path if `specs/<task>/` is uncommitted)
  - integration branch name, current tip SHA (`HEAD`), and checkout mode (plus prior landed uncommitted paths in `no-commit` mode)
  - landed `Depends on` **and prior shared-lock** commit SHAs with their `decisions`
  - stop boundary: return [Implementation Result](../interfaces/acceptance-result-v1.md#implementation-result)

Complete this step when no ready task with disjoint scope waits while capacity
is free.

### 3. Acceptance Transition

| Return or Event | Orchestrator Action |
| --- | --- |
| `Implemented` (clean) | 1. Confirm the unit's changed paths stay inside granted `Mutable owners` and `Exclusive locks`.<br>2. **Fast-forward invariant:** for a worktree candidate, verify `git merge-base --is-ancestor HEAD <candidate>` and land via `git merge --ff-only <candidate>` (if `HEAD` advanced, route to the row below). In a shared checkout, stage only verified paths and commit cleanly (in `no-commit` mode, run `git add -N` only on newly created untracked paths and record the unit's landed paths).<br>3. Check `[x]`, record the replaceable `Implemented` receipt (`decisions`, `unverified` verbatim), release locks, clear `Execution`, retire Lead and worktree, and return to Step 2. |
| `Implemented` (stale base or merge conflict) | Keep Lead alive in its worktree; send the current integration tip (and conflict details) so it merges `<integration-branch>` (loading [Merge Conflict Resolution](../../../.agents/skills/merge-conflict-resolution/SKILL.md) on conflicted hunks), re-runs crate checks, commits cleanly, and returns a fast-forwardable `Implemented` result. |
| Scope expansion (`next_owner: Ledger Orchestrator — scope expansion: <paths>`) | Grant and resume one-to-one if disjoint from active writers (Planning stays closed). If held by an active writer, pause the requester until that writer lands, then grant the locks and send the updated integration tip so the Lead merges `<integration-branch>` in its worktree before editing. Break mutual lock contention via [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) (stop and preserve one writer, let the other land, then redispatch). |
| `Blocked` (missing input or upstream gap) | Keep `[ ]`, preserve partial work (retain WIP commit/branch ref or keep the worktree before stopping the Lead), release unused locks, clear `Execution`, and route [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) while unaffected tasks continue. When no ready or active tasks remain and any required task stays `Blocked`, go to Step 5. |
| Late/duplicate return, interrupted integration, or mid-task steering/invalidation | Load [Orchestrator Recovery](implementation/orchestrator-recovery.md). |

Never push, deploy, or touch auto-deploy branches without explicit authority.

### 4. Final Validation And Repair Routing

- **Entry gate:** enter once every planned code task is `[x]` with zero active
  writers (run local Final Validation even if `[effect:<action>]` is pending).
- **Fresh Delivery Owner:** dispatch one fresh `acceptance-unit-lead` that
  implemented none of the ledger's units (including one-task ledgers) with
  `Method: Final Validation`
  (`docs/spec-first-workflow/phases/implementation/final-validation.md`),
  `tasks.md`, integration branch, candidate diff (`<Base>..HEAD` or `git diff
  <Base>`), and present accepted sources (`intent.md`, `spec.md`, `design/`,
  `rollout.md`, packets). On return, confirm zero candidate edits outside
  `specs/<task>/`.
- **Repair routing (`NeedsRepair`):** dispatch one fresh `acceptance-unit-lead`
  in the shared checkout (sole writer; isolate only if an adapter requires it)
  with:
  - `Method: Repair Assignment` (`docs/spec-first-workflow/phases/implementation/repair.md`)
  - verbatim [Repair Request](../interfaces/acceptance-result-v1.md#repair-request), integration branch, `Base`, and `HEAD` (plus `Commit mode: no-commit` when active)
  - affected packet paths, prior `decisions`, and combined writable scope across named owning tasks (raising capability tier per adapter on a missed invariant)
  - **on Round 2:** Round 1 commit SHA (or repaired paths/summary in `no-commit` mode), attempted hypothesis, and recheck failure
  Handle its return:
  - **`Implemented`:** verify changed paths inside granted scope (including any granted expansion), land on the integration branch (`git merge --ff-only <candidate>` if isolated worktree, one clean local commit in shared checkout, or `git add -N` on newly created untracked paths in `no-commit` mode), retire the Repair Lead (and worktree if any), and resume Delivery Owner with the new `HEAD` + `<old-HEAD>..<new-HEAD>` (or `git diff <Base> -- <repaired paths>` in `no-commit` mode), whether scope expanded, and repair `decisions`.
  - **`Blocked`:** for `Delivery Owner — refuted findings`, commit nothing, retire Repair Lead, and resume Delivery Owner with the refuting proof; for scope expansion, grant and resume one-to-one; for `UPSTREAM_GAP` or missing input, preserve useful partial work, retire Repair Lead, and route [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) (or Step 5 if unresolvable).
- **Delivery Owner `Blocked`:** close `next_owner` under [Parent-Owned
  Recovery](../shared/transition.md#parent-owned-recovery) if within authority
  and resume Delivery Owner; otherwise go to Step 5.

Complete this step when Delivery Owner returns terminal [Completion
Result](../interfaces/acceptance-result-v1.md#completion-result) (`Accepted` or
unresolvable `Blocked`).

### 5. Close

Record the terminal result in `tasks.md`:

- **On `Accepted`:** set `status: done`, apply [Cleanup](../shared/cleanup.md)
  (removing untracked `specs/<task>/` artifacts and verifying zero leftover
  worktrees; if Cleanup updates tracked `docs/`, run `make docs-check` and
  `make typos` and commit cleanly unless in `no-commit` mode), and report
  impact first alongside verified scope, pending CI or external outcomes,
  material optional gaps, and `FOLLOW_UP` items.
- **On `Blocked` (from Step 3 or Step 4):** set `status: blocked` with
  `Blocked stop:` naming the exact missing input, capability, or authority and
  its owner while preserving useful work.
