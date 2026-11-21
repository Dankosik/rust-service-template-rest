# Cursor Harness Adapter

Use installed Task, skill, Goal, and worktree controls as native authority;
callable Task fields outrank public prose.

## Native Map

- Keep preparation (Intake through Planning) in the root session, using fresh
  [Cursor subagents](https://cursor.com/docs/subagents#how-subagents-work)
  (omit `resume`) for wide scans, bounded specialists / `Design It Twice`, and
  `Design Review` / `Plan Review` / adjudication.
- [Implementation](../spec-first-workflow/phases/implementation.md#carrier)
  binds the session as Ledger Orchestrator; Cursor reads `.agents/skills`
  directly.
- Dispatch ready units via Task (`subagent_type: "acceptance-unit-lead"`) within
  capacity, consuming [Implementation
  Result](../spec-first-workflow/phases/implementation/unit.md#implementation-result).
  Never substitute `generalPurpose`, `explore`, `shell`, or generic workers;
  report missing `acceptance-unit-lead` or missing agent identity as a carrier
  gap before dispatch.
- Delegated lanes use Task with `worker-agent`, `specialist-agent`,
  `evidence-agent`, `reviewer-agent`, or `adjudicator-agent`. Main sessions and
  direct Task children may spawn one descendant hop; grandchildren cannot spawn
  (Leads fan out writable subsets directly).
- Use a Git worktree, or Task `environment: "cloud"` for VM/branch isolation,
  when [Agent Harness](../agent-harness.md) selects isolation.
- Independent review uses a fresh `reviewer-agent` (`readonly: true`, no
  `resume`, no worktree).
- `/goal` is optional and rolling out; use only for genuinely long-running or
  resumable work.

## Models And Dispatch

Project agents pin `grok-4.6` with role-specific effort
(`grok-4.6[effort=low|medium|high|xhigh]`: `xhigh` on `acceptance-unit-lead`
and `adjudicator-agent`, `high` on `specialist-agent` and `reviewer-agent`,
`medium` on `worker-agent`, `low` on `evidence-agent`). Override via Task
`model` when [Agent Harness](../agent-harness.md) capability routing or user
selection requires it; preserve user-selected models and never encode model
names only in prompt text.

Pass [delegation interface](../agent-harness.md#delegation-interface) fields
through Task (`subagent_type`, `prompt`, `description`, `model`, `resume`,
`run_in_background` when overlapping, `environment` for cloud isolation). Apply
[Context And Lifetime](../agent-harness.md#context-and-lifetime) and [Nested
Execution](../agent-harness.md#nested-execution): retain every returned agent
ID before waiting (missing identity is a carrier failure), omit `resume` on new
lanes and every review (including Delta Recheck), and use `resume` only for
permitted continuations.

Raise `reviewer-agent` Task `model` only for highest-consequence review
boundaries.
