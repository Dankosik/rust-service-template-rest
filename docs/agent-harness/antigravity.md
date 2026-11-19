# Antigravity Harness Adapter

Use installed subagent, skill, Goal, and worktree controls as native authority;
callable tool fields outrank public prose. Custom subagents exist in the
Antigravity App and CLI; the IDE has no custom-subagent carrier, so report a
full-ledger capability gap on the IDE surface instead of shrinking the ledger
workflow (Direct Work still runs in-session).

## Native Map

- Keep preparation (Intake through Planning) in the root session, using fresh
  [`invoke_subagent`](https://antigravity.google/docs/subagents.md) lanes
  (`evidence-agent`, `specialist-agent`, `reviewer-agent`) on workspace
  `inherit` for wide scans, `Design It Twice`, and `Plan Review`.
- Antigravity reads `AGENTS.md`, `.agents/rules/antigravity-harness.md`, and
  `.agents/skills` directly; skills load by description or `/<skill-name>`.
- Start the Ledger Orchestrator with `agy --agent orchestrator` or by
  selecting `orchestrator` as the App's primary agent.
- Dispatch ready units via `invoke_subagent` with role
  `acceptance-unit-lead` within capacity, consuming [Acceptance
  Result](../spec-first-workflow/interfaces/acceptance-result-v1.md). Never
  substitute `self`, `research`, or `define_subagent` for this implementation Lead;
  report a missing `acceptance-unit-lead` or missing conversation ID as a
  carrier gap before dispatch.
- Delegated lanes use `invoke_subagent` with `worker-agent`,
  `specialist-agent`, `evidence-agent`, `reviewer-agent`, or
  `adjudicator-agent`. Read-only roles carry no edit tool; mutable roles may
  spawn within the native 10-level nesting cap, bounded by [Nested
  Execution](../agent-harness.md#nested-execution).
- Use workspace `branch` (a temporary Git worktree, removed when the subagent
  is killed) when [Agent Harness](../agent-harness.md) selects isolation;
  otherwise `inherit`.
- Independent review uses a fresh `reviewer-agent` on `inherit`, never a woken
  idle reviewer (for a bounded delta recheck after repair, spawn a fresh
  `reviewer-agent` scoped strictly to the repair delta under
  [Review](../spec-first-workflow/shared/review.md#lifecycle)).
- `/goal` only for genuinely long-running or resumable work; it changes stop
  behavior, not permissions, and does not wait for running subagents.
- Swarm, Boost, Teamwork, and Dynamic Workflows are separate control planes;
  never mix them with a ledger.

## Models And Dispatch

Generated agents in `.agents/agents` omit `model`, so every role inherits the
session model; the native `Model` field accepts only its schema tiers (such as
`inherit`, `flash`, or `pro`). Choose the session model with `--model`/`--effort`
or the App selector when [Agent Harness](../agent-harness.md) capability routing
or user selection requires it; never encode model names only in prompt text.

Pass [delegation interface](../agent-harness.md#delegation-interface) fields
through `invoke_subagent` (role, initial prompt, workspace option). Subagents
start with a clean context and run in the background. Apply [Context And
Lifetime](../agent-harness.md#context-and-lifetime): retain every returned
conversation ID before waiting (missing identity is a carrier failure), use
`send_message` to an idle subagent only for permitted continuations, and kill
finished lanes before releasing scope.
