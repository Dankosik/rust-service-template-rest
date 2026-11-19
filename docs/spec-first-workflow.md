# Workflow Router

Select the smallest non-direct execution path and current decision owner. If
[AGENTS.md](../AGENTS.md#direct-work) selected Direct Work, stop routing here.

## Terms

| Term | Meaning |
| --- | --- |
| `Direct Work` | Single-owner local execution under `AGENTS.md` with no workflow artifacts or ledger. |
| `Unit` | One coherent repository deliverable and its acceptance criteria inside Planning or Implementation. A ledger task is a persisted `Unit`. |
| `Seam` | Public boundary (API, CLI, trait, schema, or entry point) where behavior is observed without reaching into private internals; tests live at seams. |
| `Acceptance-Unit Lead` | Session or subagent actor owning one assigned `Unit`. |
| `Delivery Owner` | The Lead assigned final validation: it proves the assembled `Candidate` and reports the Completion Result. |
| `Candidate` | The tree state under work: assembled once every `Unit`'s `Implemented` change is integrated and its writers have joined, fixed while validation or review reads it. |
| `Ledger Orchestrator` | Single routing owner (`LEDGER_ORCHESTRATOR`) that schedules `Unit`s and writes canonical `tasks.md` state. |
| `Lane` | Bounded delegated worker (`worker-agent`, `specialist-agent`, `evidence-agent`) executing a strict subset for its parent. |
| `Ready Frontier` | Set of tasks in `tasks.md` whose coding `Depends on` prerequisites and authority are satisfied and whose `Mutable owners` and `Exclusive locks` are currently free (tasks gated only by `[acceptance]` or `[effect:<action>]` remain eligible to code; two ready tasks sharing a free owner or lock both appear on the frontier and dispatch serializes them by priority). |
| `Implemented` vs `Accepted` | A Lead reports `Implemented` when its unit meets the [Implemented bar](spec-first-workflow/interfaces/acceptance-result-v1.md#implementation-result). Landing it unlocks consumers of that code. `Accepted` closes the requested scope, including final validation, any required review, and any requested external outcomes. |
| `Smart zone` | Lean working-context envelope where one actor holds one coherent outcome, bounded files, and primary sources without attention degradation from unrelated history or diffs. |

## Path

Use `structured` when accepted decisions or proof must survive the current
reasoning pass. Use `orchestrated` when coordination is material: several
independent owners, separate mutable candidates, required fresh context,
hard-to-reverse decisions, dirty-checkout isolation, or durable multi-session
execution.

Keep a single bounded read-only lane on `structured`, and re-route only when
current evidence changes coordination, ownership, reversibility, or proof.

## Macro Phases

For a new change that needs planning, the root session conducts
[Intake](spec-first-workflow/phases/intake.md) with the user and writes
`specs/<task>/intent.md`, reusing a complete, current Intent when available.
The root retains the conversation and handles any later clarification of
requester meaning. Once Intent is ready, the root coordinates the following
phases under [Transition](spec-first-workflow/shared/transition.md#cross-phase-continuation).

| Macro phase | Possible owners |
| --- | --- |
| Definition | Specification, supporting Research, required review of the fixed macro result |
| Technical Design | Technical Design (supporting Research, System Design, Rust Architecture), required Technical Design Review |
| Planning | Planning and required Task Review / Readiness |
| Implementation | Parallel implementation, then final validation and any required delivery review |

Treat Research as a standalone macro phase only when the accepted boundary is
`research only`; otherwise run it inside the active phase. During preparation,
give each applicable macro phase from Definition through Planning its own fresh
session. The phase owner handles its research, review, and repairs. A reviewed
plan ends preparation at the [implementation handoff](spec-first-workflow/shared/transition.md#implementation-handoff).

Traverse applicable macro phases in table order, skipping a phase only when
Phase Selection evidence shows its decision is already closed or untriggered
while keeping open decisions in their owning phase and actor.

## Phase Selection

Evaluate these triggers before loading a phase owner:

| Observable trigger | Owner |
| --- | --- |
| New structured work lacks a complete [Intent](spec-first-workflow/interfaces/intent.md), or outcome, scope, authority, observable success, or first owner is ambiguous | [Intake](spec-first-workflow/phases/intake.md) |
| Current or external evidence can change a named decision | [Research](spec-first-workflow/phases/research.md) |
| Structured work lacks a reviewed behavioral contract ([Specification V1](spec-first-workflow/interfaces/spec-v1.md)) | [Specification](spec-first-workflow/phases/specification.md) |
| Structured work lacks a reviewed technical design ([Design V1](spec-first-workflow/interfaces/design-v1.md)) because Implementation would otherwise choose a runtime boundary, truth, material flow, failure/recovery or rollout mechanism, or Rust placement, invariant ownership, caller obligations, or dependency direction (even with fixed files) | [Technical Design](spec-first-workflow/phases/technical-design.md) |
| Structured work needs a ready task ledger, or an existing ledger must be resliced or reconciled after an upstream or handoff change | [Planning](spec-first-workflow/phases/planning.md) |
| The user starts a ready plan in a new session, or an accepted inline unit or active implementation task is ready and authorized | [Implementation](spec-first-workflow/phases/implementation.md) |

Intake is required for new structured work unless a complete Intent already
exists; Specification and Planning are required unless ready equivalent
authority already supplies their output (for Specification, a reviewed `spec.md`
at `status: ready` or a fixed external contract with zero open behavioral
forks—never `intent.md` alone). Reopen Intake whenever requester
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
