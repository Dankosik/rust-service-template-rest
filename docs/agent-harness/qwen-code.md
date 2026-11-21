# Qwen Code Harness Adapter

Use installed Qwen agent controls as native authority; callable tool schemas
outrank prose.

## Native Map

- Keep preparation (Intake through Planning) in the root session, using fresh
  [regular subagents](https://qwenlm.github.io/qwen-code-docs/en/users/features/sub-agents/#fork-subagent)
  (`agent`, never `fork`) for evidence scans, bounded specialists / `Design It
  Twice`, and `Design Review` / `Plan Review` / adjudication.
- [Implementation](../spec-first-workflow/phases/implementation.md#carrier)
  binds Ledger Orchestrator. Dispatch ready units via `agent`
  (`subagent_type: "acceptance-unit-lead"`, display name `Agent`, not `task`)
  within capacity; the `.qwen/agents/` carrier returns [Implementation
  Result](../spec-first-workflow/phases/implementation/unit.md#implementation-result).
  `todo_write` mirrors execution only; `tasks.md` owns acceptance.
- Ordinary named agents nest via `agent` (`model.maxSubagentDepth: 5`);
  teammates, forks, and workflow-spawned agents cannot nest. Missing tools or
  exhausted depth return an exact parent gap.
- Use `isolation: "worktree"` when [Agent Harness](../agent-harness.md) selects
  isolation.
- Top-level ordinary agents run in background by default; nested agents and
  caller-owned `working_dir` launches are foreground-only and reject
  `run_in_background: true`. Top-level Orchestrator sessions dispatch
  background reviewers directly.
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
