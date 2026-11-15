# OpenCode Harness Adapter

Use installed OpenCode 1 agent, Task, skill, and command controls as native
authority; callable Task fields and frontmatter outrank prose. Never mix with
`opencode2`.

## Native Map

- Default `build` stays Direct Work until the user asks to orchestrate a ready
  Implementation ledger, which launches routing from `build` or `orchestrator`
  (never ask the user to Tab, `/agent`, `/orchestrator`, or start a second
  process). `build` may load the `orchestrator` skill via `skill`.
- Dispatch ready units via Task (`subagent_type: "acceptance-unit-lead"`), even
  if `.opencode/plugins/task-subagents.js` blurbs are stale; never substitute
  `general`, `explore`, `scout`, or generic workers. Integrate candidates via
  [acceptance transition](../spec-first-workflow/phases/planning/ledger-contract.md#acceptance-transition)
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
  `user/workflow` skills in `opencode.json` while keeping `orchestrator` loadable
  on `build`. Never commit `.opencode/` runtime files excluded by `.opencode/.gitignore`.

## Models And Dispatch

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
the Orchestrator primary may use `question` for user-owned decisions.

At final delivery, use [Review](../spec-first-workflow/shared/review.md) and
[Implementation Review](../spec-first-workflow/phases/implementation-review.md).
