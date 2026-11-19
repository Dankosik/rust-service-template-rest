# Workflow Router

Select the smallest structured path when [AGENTS.md](../AGENTS.md#direct-work)
escalates beyond Direct Work.

## Terms

| Term | Meaning |
| --- | --- |
| `Direct Work` | Single-owner local execution under `AGENTS.md` with no workflow artifacts or ledger. |
| `Seam` | Public boundary (API, CLI, trait, schema, or entry point) where behavior is observed without touching private internals; tests live at seams. |
| `Ready Frontier` | Unchecked tasks in `tasks.md` whose `Depends on` prerequisites have landed and whose `Mutable owners` and `Exclusive locks` are free. |
| `Acceptance-Unit Lead` | Fresh subagent owning one implementation task or one batched repair pass. |
| `Ledger Orchestrator` | Root session (`LEDGER_ORCHESTRATOR`) executing a ready `tasks.md`: schedules Leads, lands work serially, verifies the assembled result, and runs review. |
| `Smart zone` | Lean working context where one actor holds one coherent outcome and primary sources without attention thinning from unrelated noise. |

## Macro Phases

The human defines **what** we are building once during Intake; from that point
on, the agent owns **how** to specify, design, plan, and implement autonomously
without asking the user to drive steps or make technical choices.

Keep preparation in **one continuous session** so the verbatim *why* from
grilling stays a primary source (use fresh subagents only for read-heavy scans,
`Design It Twice`, and `Plan Review`), then stop for a fresh Implementation
session:

| Phase | Owner | Output |
| --- | --- | --- |
| 1. [Intake](spec-first-workflow/phases/intake.md) | Root session + user | Confirmed shared understanding of **what** (live conversation; `specs/<task>/intent.md` only if pausing before `spec.md`) |
| 2. [Spec And Design](spec-first-workflow/phases/spec-and-design.md) | Root session (autonomous) | `specs/<task>/spec.md` (splitting large architecture into `specs/<task>/design.md`) |
| 3. [Planning](spec-first-workflow/phases/planning.md) | Root session (autonomous) | `specs/<task>/tasks.md` + `specs/<task>/tasks/<ID>-<slug>.md`, cleared by fresh [Plan Review](spec-first-workflow/phases/plan-review.md) |
| 4. [Implementation](spec-first-workflow/phases/implementation.md) | Fresh session (`LEDGER_ORCHESTRATOR`, autonomous) | Parallel `Ready Frontier` execution, final local checks, fresh [Implementation Review](spec-first-workflow/phases/implementation-review.md), and closeout |

- **Artifact hygiene:** replace superseded rules in place instead of layering
  conflicting addenda, link stable code, `api/`, migrations, and `research/*.md`
  instead of copying them, and keep surviving IDs (`R<n>`, `G<n>`, `D<n>`,
  `S<n>`, `T<ID>`) stable on reopen.
- **Reopen rule:** if new evidence invalidates a confirmed product rule or
  exposes a user-owned risk trade-off, return that single decision to the user
  with verified primary evidence and update affected artifacts in place.

## Conditional Owners

Load a conditional owner immediately before its trigger:

| Trigger | Owner |
| --- | --- |
| External contract, baseline divergence, or empirical probe can change a decision | [Research](spec-first-workflow/phases/research.md) |
| Dispatch a read-only evidence, specialist, or generative candidate lane | [Read-Only Delegation](spec-first-workflow/shared/read-only-delegation.md) |
| Decide review scope or run the review/repair loop | [Review](spec-first-workflow/shared/review.md) |
| Recover from a blocker, conflicting recommendations, or hand off a ready plan | [Recovery And Handoff](spec-first-workflow/shared/transition.md) |
| Claim scope, proof reuse, extra required checks, or unavailable infrastructure | [Evidence Contract](spec-first-workflow/shared/evidence-contract.md) |
| Resume after compaction, interruption, or mid-task steering | [Resume](spec-first-workflow/shared/resume.md) |
| Close a completed task bundle and retire worktrees | [Cleanup](spec-first-workflow/shared/cleanup.md) |
