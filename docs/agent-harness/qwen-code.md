# Qwen Code Harness Adapter

Use installed Qwen agent controls as native authority; callable tool schemas
outrank prose.

## Native Map

- Start a new [regular subagent](https://qwenlm.github.io/qwen-code-docs/en/users/features/sub-agents/#fork-subagent)
  for each preparation phase. Call `agent` with the phase brief and omit
  `subagent_type` to select general-purpose. An explicit `fork` inherits history.
- [Implementation](../spec-first-workflow/phases/implementation.md#carrier)
  binds Ledger Orchestrator. Dispatch ready units via `agent`
  (`subagent_type: "acceptance-unit-lead"`, display name `Agent`, not `task`)
  within capacity; the `.qwen/agents/` carrier returns
  [Acceptance Result](../spec-first-workflow/interfaces/acceptance-result-v1.md).
  `todo_write` mirrors execution only; `tasks.md` owns acceptance.
- Ordinary named agents nest via `agent` (`model.maxSubagentDepth: 5`);
  teammates, forks, and workflow-spawned agents cannot nest. Missing tools or
  exhausted depth return an exact parent gap.
- Use `isolation: "worktree"` when [Agent Harness](../agent-harness.md) selects
  isolation.
- Top-level ordinary agents run in background by default; nested agents and
  caller-owned `working_dir` launches are foreground-only and reject
  `run_in_background: true`. When overlap helps, let the root broker a
  background reviewer from the Lead's fixed brief and return its result.
- Agent Teams are an explicitly selected, preconfigured route only: the root
  brokers descendants from teammate Lead briefs (`task_create`/`task_update`
  mirror execution only). Never require Teams for full-ledger work.

## Models And Dispatch

Frontmatter accepts `inherit`, `fast`, a model ID, or `authType:modelId` (`fast`
for closed mechanical work, `inherit` for ordinary/closed Lead work, strongest
configured for uncertainty or protected risk). Effort inherits from the session
(no per-agent effort field); preserve user-selected models.

Apply [Context And Lifetime](../agent-harness.md#context-and-lifetime) and
[Nested Execution](../agent-harness.md#nested-execution). Start fresh named
agents (never `subagent_type: "fork"`), pass [delegation
interface](../agent-harness.md#delegation-interface) fields natively, and
retain `task_id`. `list_agents` and `send_message` (`task_id`) continue
background agents; foreground nested results return inline (report an exact gap
if no continuation exists rather than inventing a resume field).

At final delivery, use [Review](../spec-first-workflow/shared/review.md) and
[Implementation Review](../spec-first-workflow/phases/implementation-review.md);
start background review alongside final validation only where async execution
is permitted.
