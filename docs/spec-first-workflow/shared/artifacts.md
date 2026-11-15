# Artifacts

Use when deciding whether a result must survive the current reasoning pass.

Persist only when another actor or phase must consume the result, work will
resume later, evidence must be refreshed or audited, implementation needs a
stable decision or ledger, or rollout has a real operational sequence.
Otherwise keep the result inline.

Task-local artifacts live under `specs/<task>/`:

| Trigger | Artifact | Owns |
| --- | --- | --- |
| New structured work completes requester interview | [`intent.md`](../interfaces/intent.md) | Requester meaning synthesized after interview |
| Behavior delta must survive | `spec.md` | Outcome, behavior delta, constraints, proof expectations |
| Implementation would choose mechanism or placement | `design/` | Selected system and ownership decisions |
| Several units/dependencies or durable execution state exist | `tasks.md`, `tasks/<ID>-<slug>.md` | Index order/status/results and each task's outcome/boundary/acceptance |
| Evidence must be reused or refreshed | `research/*.md` | Findings, limits, conflicts, decision effect |
| Deployment/migration/backfill has a real sequence | `rollout.md` | Operational gates, recovery, observables |
| Pre-ledger cross-phase or cross-session coordination cannot be recovered from those artifacts | `workflow-plan.md` | Goal, active execution instructions, current phase/owner locator, cross-phase decisions and consequences, unfinished work/next action |

Task artifacts preserve accepted behavior, design, and scope; current workflow
owners govern execution timing. Keep at most one active coordination record:
when persisted, `workflow-plan.md` holds pre-ledger coordination and is deleted
once Planning persists `tasks.md` (carrying any active execution instructions
into `Global constraints`).

When new guidance or a mid-task decision changes scope, contracts, completion
criteria, or execution policy, reconcile it against work already underway:
replace the affected active text in its owning artifact and consumed task
packets, and record its consequences for dependent units, writable scopes/locks,
and invalidated proof. Do not leave conflicting requirements active beneath a
newer addendum. Preserve unaffected decisions, effect authority, and valid
evidence; mark superseded records historical and remove them from the active
read path.

Keep current outcome, exclusions, remaining work, and next action in their
existing owners. Link to historical evidence when needed instead of copying
its execution instructions into current state. Git retains superseded prose;
required operational and financial evidence retains its existing custody.

Reference stable OpenAPI, code, tests, generated sources, and external contracts
instead of copying them. Split an artifact only when the split creates a real
owner or materially improves review. Stop when the next actor can act, prove
the result, and identify the reopen owner without chat archaeology.

Do not persist a task ledger when Planning finds no executable implementation
unit. Record the no-implementation disposition in the owning phase result.

## Artifact Lifecycle V1

Use one status when durable state changes an action:

| Status | Meaning |
| --- | --- |
| `draft` | The owning decision or execution record is still being authored or repaired. |
| `ready` | The artifact has closed its owned decisions and its next consumer can act without semantic invention. |
| `blocked` | A required missing decision or input and its reopen owner prevent useful progress. |
| `done` | Execution and global completion are proved; this status is not evidence by itself. |

A material invalidation moves `ready` back to `draft` or `blocked`. Only the
artifact owner changes status. Required or conditionally triggered independent
review follows the shared [Review](review.md) lifecycle. Task receipts and
executable-ledger transitions use [Task Ledger V1](../interfaces/task-ledger-v1.md).
