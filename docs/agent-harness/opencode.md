# OpenCode Harness Adapter

Use the native tool schema for the installed OpenCode generation. Check
`opencode --version` when both executables are installed.

## Preparation and handoff

Start an ordinary request in `build` under [Work Selection](../../AGENTS.md#work-selection-and-loading).
The preparation coordinator writes the intent. For each applicable macro phase,
dispatch `general` with its phase brief and accepted artifact paths. Each
phase may call `reviewer-agent` for its independent review. When nesting is
unavailable, the coordinator brokers that review and returns its verdict to
the same phase owner under [Transition](../spec-first-workflow/shared/transition.md#cross-phase-continuation).
Broker with the same read-only `reviewer-agent`. If its model is unavailable,
preserve the candidate and report the pending review. A writable `general`
session cannot supply that verdict.
After ready Planning, return the [user's launch message](../spec-first-workflow/shared/transition.md#implementation-handoff).
A new `build` or `orchestrator` session executes the named ledger.

| Installed generation | New phase, reviewer, or implementation Lead | Same assignment follow-up |
| --- | --- | --- |
| 1.x | `Task` with `subagent_type`, `description`, and `prompt`. Omit `task_id`. | Set `task_id` to the returned child ID. |
| 2.x | `subagent` with `agent`, `description`, and `prompt`. Omit `sessionID`. | Pass the returned `sessionID` to `subagent`. |

## OpenCode 2

Use the result of a foreground call when the next phase depends on it.
In [2.2.7](https://github.com/anomalyco/opencode/blob/v2.2.7/packages/core/src/tool/plugin/subagent.ts)
foreground is the default. If a call returns `running`, retain its ID and
consume the completion notification before moving the phase. A new ID is
required for each new macro phase. A follow-up keeps that child's history.

Dispatch each ready implementation unit to `acceptance-unit-lead` and
independent reviews to `reviewer-agent`. A configured role model takes
precedence over the parent's model. Use the native model override when the
user explicitly selects a different child model.
OpenCode 2 translates their shared agent files and permission rules. It reads
the nesting limit from `experimental.subagent_depth`; OpenCode 1 reads the
top-level `subagent_depth`. Use the native subagent catalog in 2.x. Project
instructions and role dispatch are independent of the 1.x Task plugin.
Apply [Implementation](../spec-first-workflow/phases/implementation.md#orchestrator-loop)
for implementation scheduling and final acceptance.

## OpenCode 1

- Dispatch ready units via Task (`subagent_type: "acceptance-unit-lead"`), even
  if `.opencode/plugins/task-subagents.js` blurbs are stale; never substitute
  `general`, `explore`, `scout`, or generic workers. Integrate candidates via
  [acceptance transition](../spec-first-workflow/phases/implementation.md#3-acceptance-transition)
  from [Acceptance Result](../spec-first-workflow/interfaces/acceptance-result-v1.md).
- Report a carrier gap only if Task rejects the call, returns unknown agent or
  no identity, or `subagent_depth` is 1. `opencode.json` sets `subagent_depth: 3`
  (root -> Lead -> worker -> worker); generated workers allow Task only to
  `worker-agent` and `evidence-agent`, and review roles deny Task.
- Task has no isolation field; use a Git worktree or `opencode --agent
  acceptance-unit-lead --dir <worktree>` only when [Agent Harness](../agent-harness.md)
  requires isolation (no experimental workspace flag or community plugin).
- Generated lanes set `hidden: true` (invoke via Task, not `@`); `reviewer-agent`
  sets `edit: deny`. No native Goal exists; background Task requires
  `OPENCODE_EXPERIMENTAL_BACKGROUND_SUBAGENTS` (foreground is default).
- OpenCode ignores `disable-model-invocation`, so `build` and `plan` deny
  `user/workflow` skills in `opencode.json`. Never commit `.opencode/` runtime files excluded by `.opencode/.gitignore`.

### Models And Dispatch

Connect xAI via `/connect`. Use `xai/grok-4.6` variant `high` on Orchestrator
and `xhigh` on Leads; OpenCode applies `variant` only when an agent pins a
model, so child lanes pin `xai/grok-4.6` plus role `grok_effort`. Preserve
user-selected primary models; Task has no model or variant field.

Pass [delegation interface](../agent-harness.md#delegation-interface) fields
via Task (`subagent_type`, `prompt`, `description`, and `task_id` only for
permitted continuation). Apply [Context And Lifetime](../agent-harness.md#context-and-lifetime)
and [Nested Execution](../agent-harness.md#nested-execution): retain returned
session IDs before waiting (missing identity is a carrier failure) and omit
`task_id` on new lanes and initial reviews. Spawned children lack `question`;
the Orchestrator primary prefers plain-text rounds under
[Grilling](../spec-first-workflow/shared/grilling.md) and uses `question` only
when it preserves full trade-off context and free-form qualification.

At final delivery, use [Review](../spec-first-workflow/shared/review.md) and
[Implementation Review](../spec-first-workflow/phases/implementation-review.md).
