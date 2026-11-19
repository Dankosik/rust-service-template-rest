# Antigravity Harness Adapter

Use installed subagent, skill, Goal, and worktree controls as native authority;
callable tool fields outrank public prose. Custom subagents exist in the
Antigravity App and CLI; the IDE has no custom-subagent carrier, so an IDE
session runs the ledger in-session and reports delegated units as a carrier
gap.

## Native Map

- In the App or CLI, start each preparation phase with a new
  [`invoke_subagent`](https://antigravity.google/docs/subagents.md) using
  `self`, the phase brief, and workspace `inherit`. This gives the child the
  caller's tools and checkout with a fresh conversation. Track its returned
  conversation ID. On an IDE surface without this control, preserve the
  completed input and report the phase that needs the App or CLI.
- Antigravity reads `AGENTS.md`, `.agents/rules/antigravity-harness.md`, and
  `.agents/skills` directly; skills load by description or `/<skill-name>`.
- Start the Ledger Orchestrator with `agy --agent orchestrator` or by
  selecting `orchestrator` as the App's primary agent; the orchestrator skill
  is hidden from model selection.
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
  idle reviewer.
- `/goal` only for genuinely long-running or resumable work; it changes stop
  behavior, not permissions, and does not wait for running subagents.
- Swarm, Boost, Teamwork, and Dynamic Workflows are separate control planes;
  never mix them with a ledger.

## Models And Dispatch

Generated agents in `.agents/agents` omit `model`, so every role inherits the
session model; the native field accepts only `inherit`, `flash`, or `pro`.
Choose the session model with `--model`/`--effort` or the App selector when
[Agent Harness](../agent-harness.md) capability routing or user selection
requires it; never encode model names only in prompt text.

Pass [delegation interface](../agent-harness.md#delegation-interface) fields
through `invoke_subagent` (role, initial prompt, workspace option). Subagents
start with a clean context and run in the background. Apply [Context And
Lifetime](../agent-harness.md#context-and-lifetime): retain every returned
conversation ID before waiting (missing identity is a carrier failure), use
`send_message` to an idle subagent only for permitted continuations, and kill
finished lanes before releasing scope.

At final delivery, use [Review](../spec-first-workflow/shared/review.md) and
[Implementation Review](../spec-first-workflow/phases/implementation-review.md).
