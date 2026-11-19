# Implementation

Execute a ready `specs/<task>/tasks.md` autonomously from the first `Ready
Frontier` through final verification, independent review, and closeout.

## Carrier

- **Single fixed unit without a ledger:** execute directly under
  [Direct Work](../../../AGENTS.md#direct-work) (which owns local validation
  and review triggers).
- **Named ledger (`tasks.md`):** inspect its status. If runtime state exists or
  work was interrupted, reconcile under [Resume](../shared/resume.md). Report a
  `done` ledger, resolve a `blocked` ledger from `Blocked stop:`, route a
  `draft` ledger through [Planning](planning.md) and [Plan
  Review](plan-review.md) before executing (or stop and report if restricted to
  implementation-only), or run a `ready` ledger as root Ledger Orchestrator
  (`LEDGER_ORCHESTRATOR`)—including a one-task ledger.

## Orchestrator Loop

The Orchestrator schedules tasks, lands work serially, verifies the assembled
result, runs independent review, and alone writes [Ledger Runtime
State](../interfaces/acceptance-result-v1.md#ledger-runtime-state). Leave unit
coding and repair edits strictly to fresh `ACCEPTANCE_UNIT_LEAD` subagents.

### 1. Pin Baseline And Controls

- Confirm full-ledger controls under [Agent Harness](../../agent-harness.md)
  and select the integration worktree under [Branches and
  Worktrees](../../../AGENTS.md#branches-and-worktrees) before mutating ledger
  state.
- Keep existing `Base:` on resume; otherwise pin the starting integration `HEAD`
  as `Base:`. Keep the writable ledger in the integration worktree (if the
  accepted `specs/<task>/` bundle is untracked in the source checkout, copy its
  current contents into the integration worktree before updating it; exclude
  `specs/<task>/` from candidate commits) and pass its actual path to isolated
  Leads.
- Use local commits by default. If the user explicitly forbade local commits,
  record `Commit mode: no-commit`, serialize writers in one checkout, compare
  working-tree deltas before and after each writer against its grant, mark
  verified new candidate files intent-to-add (`git add -N -- <paths>`,
  excluding `specs/<task>/`), and use `git diff <Base>` (scoped to verified
  candidate paths if unrelated dirty edits exist) as the assembled candidate.
- *Done when:* `Base:` is pinned on the integration branch and the initial
  `Ready Frontier` is verified.

### 2. Dispatch Ready Frontier

- Dispatch one fresh `ACCEPTANCE_UNIT_LEAD` per ready task in ledger priority
  order up to capacity (bound concurrent compiling Rust worktrees to 2–3 and
  reserve slots for integration, review, and recovery).
- Assign only tasks with disjoint `Mutable owners` and `Exclusive locks` to run
  in parallel; serialize tasks needing non-isolated fixtures. Use isolated
  worktrees anchored to the integration tip whenever two or more writers run
  concurrently.
- Record each active Lead's native locator under `Execution:` in `tasks.md`.
- Keep briefs sparse: send [Unit Implementation](implementation/unit.md),
  ledger and packet paths, integration branch/tip, checkout mode, and relevant
  landed `decisions` or diffs.
- *Done when:* all eligible non-conflicting frontier slots are running.

### 3. Acceptance Transition

Integrate `Implemented` returns serially (matching each return to its active
task and ignoring completed duplicates; on interruption, compare Git and
`tasks.md` and resume from the first unfinished step):

1. Check the Lead's diff against its granted `Mutable owners` and `Exclusive
   locks`, rejecting edits to `specs/<task>/`. In a shared checkout commit only
   verified paths (leaving verified edits uncommitted and marking new files
   intent-to-add in `no-commit` mode). For an isolated candidate fast-forward
   the integration branch; if the tip advanced, send the new tip back to the
   same Lead to merge and recheck before landing.
2. Check `[x]` in `tasks.md` and record the task's receipt (carrying its
   `decisions` and `unverified` verbatim).
3. Release locks, clear `Execution:`, retire the Lead and its worktree under
   [Cleanup](../shared/cleanup.md), and refill the `Ready Frontier`
   immediately.

- **Scope expansion or blockers:** grant disjoint new paths before editing (if
  held by another active writer, wait for that landing and refresh the
  requester's tip first). For contention, missing inputs, or upstream gaps,
  preserve partial work, release unused locks, clear `Execution:`, and resolve
  under [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery).
- *Done when:* every code task is `[x]` on the integration branch and all
  writers have joined.

### 4. Final Verification, Review, And Repair

Because the Orchestrator wrote zero unit code, it owns assembled verification
directly on the frozen integration candidate (`git diff <Base>..HEAD`, or the
scoped `no-commit` candidate diff from Step 1):

1. **Verify assembled proof:** run required [Validation
   budget](../../../AGENTS.md#validation-budget) checks plus any
   source-mandated `Additional proof` (reusing valid unchanged scoped receipts
   under [Evidence Contract](../shared/evidence-contract.md#execution-evidence)
   and using [Validation Routing](../../validation-routing.md) for generated or
   mixed surfaces). Map every packet `Observable claim`, preserved `G<n>`, and
   `Completion` criterion to actual executed test/command output, reconciling
   every `unverified` entry. If any required local check fails, batch and route
   it through Step 4.3 before starting initial review.
2. **Run independent review:** begin initial review only after Step 4.1 passes.
   Select the disposition under [Review Trigger](../shared/review.md#trigger).
   When review applies, dispatch fresh [Implementation
   Review](implementation-review.md) (`2-lens panel`, single 4-axis, or
   `structure-only`) with the pinned diff command, accepted bundle, and check
   receipts—omitting builder narration.
   - `REVIEW_HANDOFF_INVALID`: repair the diff or handoff boundary and
     redispatch without consuming a repair round.
   - `NEEDS_PARENT`: resolve missing input or unavailable required proof under
     [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) (or
     hold at `status: blocked`).
   - `FAIL`: verify every finding against live code before editing or
     escalating:
     - Confirmed `UPSTREAM_GAP`: resolve under [Parent-Owned
       Recovery](../shared/transition.md#parent-owned-recovery) (update
       `spec.md`/`design.md`/packets in place if technical; reopen the user only
       for a product/risk contradiction or missing external authority).
     - Confirmed `TASK_DEFECT` or `INTEGRATION_DEFECT`: batch into Step 4.3.
     - Complete refutation (zero code changes): never self-approve past `FAIL`;
       dispatch `adjudicator-agent` with both sides' primary anchors under
       [Review Lifecycle](../shared/review.md#lifecycle).
3. **Repair confirmed defects:** batch confirmed `TASK_DEFECT` and
   `INTEGRATION_DEFECT` findings (or pre-review Step 4.1 failures) into one
   fresh [Repair Lead](implementation/repair.md) with combined writable scope
   and the adapter's capability escalation for a missed invariant (max **2
   repair rounds** per defect class under [Review
   Lifecycle](../shared/review.md#lifecycle)).
   - On `Implemented`: land via Step 3 without flipping task checkboxes and
     re-run invalidated checks. If initial review has not run yet, return to
     Step 4.2; otherwise dispatch one fresh `Delta Recheck` reviewer (or a fresh
     full review if repair expanded scope, interfaces, or risk).
   - On complete refutation (`Blocked`, `next_owner: Orchestrator — refuted
     findings`): retire the Repair Lead without a landing, never self-approve
     past `FAIL`, and dispatch `adjudicator-agent` with both sides' primary
     anchors under [Review Lifecycle](../shared/review.md#lifecycle) (repairing
     if adjudication confirms the defect, or recording the resolution and
     clearing if it rejects the finding with primary proof).
- *Done when:* every required local check and `Observable claim` passes on the
  current candidate and required review clears (`PASS`).

### 5. Close

- Apply [Authority](../../../AGENTS.md#authority) before any external write or
  auto-deploy push.
- `Accepted` holds when Step 4 clears and every explicitly requested external
  outcome in `Completion` has direct evidence and authority.
  - On `Accepted`: record the final receipt, flip `tasks.md` to `status: done`,
    and run [Cleanup](../shared/cleanup.md).
  - If local proof and review pass while an explicitly requested external gate
    remains pending—or if recovery cannot close a required blocker—preserve
    partial work and hold `tasks.md` at `status: blocked` with `Blocked stop:`
    recording the verified local scope and the exact pending gate/input and
    owner.
- Report impact, executed checks, material limitations, and any pending
  external gates.
