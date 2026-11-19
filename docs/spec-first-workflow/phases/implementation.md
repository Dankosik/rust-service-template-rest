# Implementation

Use when accepted implementation work is ready and authorized. Fresh Leads
implement planned units and their tests in parallel; one fresh Delivery Owner
validates the assembled candidate at the end.

| Role | Carrier | Follow |
| --- | --- | --- |
| Ledger Orchestrator (`LEDGER_ORCHESTRATOR`) | Root session | [Carrier](#carrier), then [Orchestrator Loop](#orchestrator-loop) |
| Unit Lead (one planned unit) | Adapter's Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) | [Unit Implementation](implementation/unit.md) |
| Repair Lead (Final Validation repair) | Adapter's Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) | [Repair Assignment](implementation/repair.md) |
| Delivery Owner (assembled candidate) | Adapter's Acceptance-Unit Lead (`ACCEPTANCE_UNIT_LEAD`) | [Final Validation](implementation/final-validation.md) |

## Carrier

Open the named `tasks.md` and use [Ledger Runtime
State](../interfaces/acceptance-result-v1.md#ledger-runtime-state) for ledger
fields. Load [Resume](../shared/resume.md) only when `tasks.md` already has
runtime state (`Base:`, `[x]`, `Execution:`, `Resume:`) or after interruption
or compaction. Report a completed ledger as done and route an unready plan to
its recorded owner. For a ready ledger, bind this session as Ledger
Orchestrator (`LEDGER_ORCHESTRATOR`), including for a one-task ledger.

For a fixed inline unit with no ledger, load [Inline Unit
Mode](implementation/modes.md#inline-unit-mode).

## Orchestrator Loop

Route, integrate, and write canonical `tasks.md` state; never edit production
or test code or run test suites as Orchestrator.

### 1. Verify Controls And Pin Baseline

- Confirm the [Agent Harness](../../agent-harness.md) adapter's full-ledger
  controls and run `make ledger-check TASK=<task>` to verify `status: ready`,
  upstream readiness, packet existence, and lock-aware Ready Frontier; report
  any blocking capability gap.
- Pin `Base: <HEAD sha>` in `tasks.md` (keeping recorded `Base` on resume).
- **Branch and artifact hygiene:** if `HEAD` sits on `main` or detached `HEAD`
  without authority to land there, create a local integration branch from
  `Base` (e.g., `impl/<task>`). Confirm no unrelated dirty edits block
  integration, keep `specs/<task>/` untracked and out of every commit until
  Step 5 Cleanup, and note the main checkout path when `specs/<task>/` is
  uncommitted so worktrees can read it.
- **Commit mode and session constraints:** land every unit as one clean local
  commit (`<Base>..HEAD` advances monotonically). If the user forbade local
  commits, load [No-Commit Mode](implementation/modes.md#no-commit-mode).
  Carry any session-level user execution constraints into Lead and Delivery
  Owner briefs.

Done when `ledger-check` passes on `ready`, controls are confirmed, and `Base`,
integration branch, and commit mode are pinned.

### 2. Ready Frontier Dispatch

Read **only** `Mutable owners` and `Exclusive locks` from each ready packet
(leave `Sources`, `Outcome`, and `Provides` to the Lead so Orchestrator stays
in its smart zone). Start every task whose coding `Depends on`, disjoint locks,
and authority are met (tie-break by `tasks.md` priority; leave `[acceptance]`
and `[effect:<action>]` gates for Final Validation or release).

- **Fresh Unit Lead per task:** dispatch one fresh Acceptance-Unit Lead
  (`ACCEPTANCE_UNIT_LEAD`, per adapter) per ready task up to capacity (keep
  concurrent compiling Rust worktrees bounded—typically 2–3—and reserve slots
  for integration, review, and recovery); record its locator in `Execution`.
- **Checkout isolation and branch ownership:** share the checkout for one
  active writer or any task needing untracked/gitignored local fixtures
  (serialized against concurrent writers); use isolated worktrees anchored to
  the integration tip for two or more concurrent writers under [Agent
  Harness](../../agent-harness.md#delegation-interface). Orchestrator owns the
  integration branch ref (worktree Leads commit on their worktree branch;
  shared-checkout Leads leave scoped uncommitted edits).
- **Unit Lead brief** (sparse pointers):
  - `Method: Unit Implementation` (`docs/spec-first-workflow/phases/implementation/unit.md`)
  - `tasks.md` and packet paths (+ main checkout path if `specs/<task>/` is uncommitted)
  - integration branch, current tip SHA (`HEAD`), checkout mode, and any active user constraints or `modes.md` rules
  - landed `Depends on`, prior shared-lock, and same-crate commit SHAs (decisions in `tasks.md`)
  - stop boundary: return [Implementation Result](../interfaces/acceptance-result-v1.md#implementation-result)

Done when no ready task with disjoint scope waits while capacity is free.

### 3. Acceptance Transition

Integrate each Lead return as it arrives and refill Step 2 immediately for
newly unblocked tasks while unrelated Leads remain in flight.

| Return or Event | Orchestrator Action |
| --- | --- |
| `Implemented` (clean) | 1. Confirm changed paths stay inside granted `Mutable owners`/`Exclusive locks` with zero `specs/<task>/` paths.<br>2. Worktree: verify `git merge-base --is-ancestor HEAD <candidate>` and run `git merge --ff-only <candidate>` (route below if `HEAD` advanced). Shared checkout: stage verified paths and commit cleanly.<br>3. Mark `[x]`, record replaceable `Implemented` receipt (`decisions`/`unverified` verbatim), release locks, clear `Execution`, retire Lead/worktree, and refill Step 2. |
| `Implemented` (stale base or merge conflict) | Keep Lead alive in its worktree; send current integration tip (and conflict details) so it merges `<integration-branch>` (loading [Merge Conflict Resolution](../../../.agents/skills/merge-conflict-resolution/SKILL.md) on conflicted hunks), re-runs crate lint/tests, commits cleanly, and returns a fast-forwardable `Implemented` result. |
| Scope expansion (`next_owner: Ledger Orchestrator — scope expansion: <paths>`) | Grant and resume one-to-one if disjoint from active writers (Planning stays closed). If held by an active writer, pause requester until that writer lands, then grant locks and send the updated tip so Lead merges `<integration-branch>` before editing. Break mutual contention via [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery). |
| `Blocked` (missing input or upstream gap) | Keep `[ ]`, preserve partial work (WIP commit/branch ref or worktree before stopping Lead), release unused locks, clear `Execution`, and route [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) while unaffected tasks continue. |
| Late/duplicate return, interrupted integration, or mid-task steering/invalidation | Load [Orchestrator Recovery](implementation/orchestrator-recovery.md). |

Never push, deploy, or touch auto-deploy branches without explicit authority.

Exit Steps 2–3 to Step 4 once every planned code task is `[x]` with zero active
writers (or go to Step 5 if no ready or active tasks remain and a required task
stays `Blocked`).

### 4. Final Validation And Repair Routing

- **Entry gate:** every planned code task is `[x]` with zero active writers
  (run local Final Validation even when `[effect:<action>]` is pending).
- **Fresh Delivery Owner:** dispatch one fresh Acceptance-Unit Lead
  (`ACCEPTANCE_UNIT_LEAD`, per adapter) that implemented none of the ledger's
  units (including 1-task ledgers) with `Method: Final Validation`
  (`docs/spec-first-workflow/phases/implementation/final-validation.md`),
  `tasks.md`, integration branch, pinned `Base` and `HEAD` (`<Base>..HEAD`),
  present accepted bundle (`specs/<task>/`), and any active user constraints or
  `modes.md` rules.
- **Read-only enforcement:** on return, verify `git status` shows zero
  candidate edits outside `specs/<task>/` — discard any unauthorized Delivery
  Owner mutation and reject/re-run validation (in [No-Commit
  Mode](implementation/modes.md#no-commit-mode), enforce read-only via its
  pre-dispatch snapshot rule instead).
- **Repair routing (`NeedsRepair`):** dispatch one fresh Acceptance-Unit Lead
  (`ACCEPTANCE_UNIT_LEAD`, per adapter) in the shared checkout (sole writer;
  isolate only if an adapter requires it) with `Method: Repair Assignment`
  (`docs/spec-first-workflow/phases/implementation/repair.md`), verbatim [Repair
  Request](../interfaces/acceptance-result-v1.md#repair-request) (including
  `prior_attempt` on Round 2), integration branch, `Base`, `HEAD`, affected
  packets, prior `decisions`, combined writable scope across named owning tasks
  (raising capability tier per adapter on a missed invariant), and active user
  constraints or `modes.md` rules.

| Repair Lead Return | Orchestrator Action |
| --- | --- |
| `Implemented` | Confirm changed paths stay inside granted scope (zero `specs/<task>/`); land onto integration branch (`git merge --ff-only <candidate>` if isolated, one clean local commit in shared checkout); retire Repair Lead/worktree; resume Delivery Owner with new `HEAD`, `<old-HEAD>..<new-HEAD>`, whether scope expanded, and repair `decisions`. |
| `Blocked` (`Delivery Owner — refuted findings`) | Commit nothing, retire Repair Lead, and resume Delivery Owner with the refuting proof. |
| `Blocked` (scope expansion) | Grant and resume one-to-one. |
| `Blocked` (`UPSTREAM_GAP` or missing input) | Preserve partial work, retire Repair Lead, and route [Parent-Owned Recovery](../shared/transition.md#parent-owned-recovery) (or Step 5 if unresolvable). |

- **Delivery Owner `Blocked`:** close `next_owner` under [Parent-Owned
  Recovery](../shared/transition.md#parent-owned-recovery) if within authority
  and resume Delivery Owner; otherwise go to Step 5.

Done when Delivery Owner returns terminal [Completion
Result](../interfaces/acceptance-result-v1.md#completion-result) (`Accepted` or
unresolvable `Blocked`).

### 5. Close

- **On `Accepted`:** set `status: done` in `tasks.md`, apply
  [Cleanup](../shared/cleanup.md) (delete untracked `specs/<task>/`, confirm
  zero leftover worktrees; if Cleanup updates tracked `docs/`, run `make
  docs-check` and `make typos` and commit cleanly), and report impact first
  alongside verified scope, any unrequested pending CI/`main` checks, material
  optional gaps, and `FOLLOW_UP` items.
- **On `Blocked`** (Step 3, Step 4, or any explicitly requested external
  outcome in `Completion` not yet observed or authorized): set `status:
  blocked` with `Blocked stop:` naming exact missing input/authority and owner
  while preserving useful work.

Done when `tasks.md` records terminal state, temporary artifacts/worktrees are
retired on `Accepted`, and the report leads with impact plus open action items.
