# Workflow Router

Select the smallest non-direct execution path and current decision owner. If
[AGENTS.md](../AGENTS.md#direct-work) selected Direct Work, stop routing here.

## Terms

| Term | Meaning |
| --- | --- |
| `Direct Work` | Single-owner local execution under `AGENTS.md` with no workflow artifacts or ledger. |
| `Unit` | One coherent repository deliverable and its acceptance criteria inside Planning or Implementation. |
| `Acceptance-Unit Lead` | Session or subagent actor owning one assigned `Unit` (or final delivery validation). |
| `Ledger Orchestrator` | Single routing owner (`LEDGER_ORCHESTRATOR`) that schedules `Unit`s and writes canonical `tasks.md` state. |
| `Lane` | Bounded delegated worker (`worker-agent`, `specialist-agent`, `evidence-agent`) executing a strict subset for its parent. |
| `Ready Frontier` | Set of tasks in `tasks.md` whose consumed contracts, writable scopes, and authority are ready to start immediately. |
| `Implemented` vs `Accepted` | `Implemented` means code and tests landed with writers joined (unlocking downstream coding); `Accepted` means final assembled validation and required review passed. |

## Path

Use `structured` when accepted decisions or proof must survive the current
reasoning pass. Use `orchestrated` when coordination is material: several
independent owners, separate mutable candidates, required fresh context,
hard-to-reverse decisions, dirty-checkout isolation, or durable multi-session
execution.

Keep a single bounded read-only lane on `structured`, and re-route only when
current evidence changes coordination, ownership, reversibility, or proof.

## Macro Phases

| Macro phase | Possible owners |
| --- | --- |
| Definition | Intake, supporting or standalone Research, Specification, required review of the fixed macro result |
| Technical Design | supporting Research, System / Integration Design, Rust Code / Ownership Design, required Technical Design Review |
| Planning | Planning and required Task Review / Readiness |
| Implementation | Parallel implementation, then final validation and any required delivery review |

Treat Research as a standalone macro phase only when the accepted boundary is
`research only`; otherwise run it inside the active phase alongside review,
repair, and the smallest upstream reopen. Assign at most one macro phase per
phase actor. When a request spans phases, the root session acts solely as
continuation coordinator—routing results under
[Transition](spec-first-workflow/shared/transition.md) and delegating each
selected macro phase to a fresh actor through the authorized harness—stopping at
any explicit phase-only or research-only boundary.

Traverse applicable macro phases in table order, skipping a phase only when
Phase Selection evidence shows its decision is already closed or untriggered
while keeping open decisions in their owning phase and actor.

## Phase Selection

Evaluate these triggers before loading a phase owner:

| Observable trigger | Owner |
| --- | --- |
| New structured work lacks a complete [Intent](spec-first-workflow/interfaces/intent.md), or outcome, scope, authority, observable success, or first owner is ambiguous | [Intake](spec-first-workflow/phases/intake.md) |
| Current or external evidence can change a named decision | [Research](spec-first-workflow/phases/research.md) |
| Structured work lacks a ready behavior delta | [Specification](spec-first-workflow/phases/specification.md) |
| Implementation would otherwise choose a runtime boundary, truth, material flow, failure/recovery behavior, or rollout mechanism | [System / Integration Design](spec-first-workflow/phases/system-integration-design.md) |
| Package/file ownership is not mechanically forced by current code and accepted design | [Rust Code / Ownership Design](spec-first-workflow/phases/rust-code-ownership-design.md) |
| Structured work lacks one ready fixed unit or the smallest dependency-ordered ledger | [Planning](spec-first-workflow/phases/planning.md) |
| One accepted implementation unit is ready and authorized | [Implementation](spec-first-workflow/phases/implementation.md) |

Intake is required for new structured work unless a complete Intent already
exists; Specification and Planning are required unless ready equivalent
authority already supplies their output. Reopen Intake whenever requester
meaning is missing or changed. Load only the triggered owner, recording
`skipped` solely when a durable [Transition
result](spec-first-workflow/shared/transition.md) requires that disposition.

## Conditional Owners

Load a conditional owner immediately before its trigger:

| Trigger | Owner |
| --- | --- |
| Decide whether or what to persist | [Artifacts](spec-first-workflow/shared/artifacts.md) |
| Interpret or update artifact status | [Artifact Lifecycle V1](spec-first-workflow/shared/artifacts.md#artifact-lifecycle-v1) |
| Dispatch a non-implementation read-only lane | [Read-Only Delegation](spec-first-workflow/shared/read-only-delegation.md) |
| Decide or run independent review | [Review](spec-first-workflow/shared/review.md) |
| Resume after compaction, interruption, or session/actor handoff | [Resume](spec-first-workflow/shared/resume.md) |
| Move, reopen, or cross an actor/session/macro-phase boundary | [Transition](spec-first-workflow/shared/transition.md) |
| Close a completed task bundle | [Cleanup](spec-first-workflow/shared/cleanup.md) |

### Implementation-Input Closure

Implementation reopens the smallest owner when a required input is unavailable
from its named authority.
