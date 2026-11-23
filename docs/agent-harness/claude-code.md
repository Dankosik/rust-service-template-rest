# Claude Code Harness Adapter

Use installed `Agent` and `Goal` controls as native authority; callable tool
schemas outrank prose.

## Native Map

- Keep preparation (Intake through Planning) in the root session, dispatching
  fresh [non-fork subagents](https://code.claude.com/docs/en/sub-agents#what-loads-at-startup)
  (`evidence-agent`, `specialist-agent`, `reviewer-agent`, `adjudicator-agent`)
  for wide scans, bounded specialists / `Design It Twice`, and `Design Review` /
  `Plan Review` / adjudication.
- [Implementation](../spec-first-workflow/phases/implementation.md#carrier)
  binds the session as Ledger Orchestrator. Dispatch ready units via `Agent`
  (`subagent_type: "acceptance-unit-lead"`) within capacity; the
  `.claude/agents/` carrier returns [Implementation
  Result](../spec-first-workflow/phases/implementation/unit.md#implementation-result).
- Named subagents nest via `Agent`; `env.CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH:
  "3"` sets three levels. Report unavailable tools or exhausted depth as an
  exact capability gap.
- Use `isolation: "worktree"` when [Agent Harness](../agent-harness.md) selects
  isolation. Native `mode` is a deprecated permission field, not the brief's
  `Mode`.
- Agent Teams are an explicitly selected, preconfigured route only: the root
  brokers descendants from teammate Leads' fixed briefs (flat roster;
  in-process teammates cannot launch background agents), while `tasks.md` owns
  acceptance. Never enable Teams implicitly.
- `/goal <condition>` is optional for long-running/resumable work; its
  evaluator sees conversation, not repository files.

## Models And Dispatch

Use `sonnet` for closed, strongly owned Lead units and ordinary work; use
`opus` for uncertainty, protected risk, or high consequence; pass `model: "fable"`
only on user selection or after a causal `opus` attempt exposes a missed
invariant. A root above `opus` keeps that ladder. Because the Lead carrier
inherits the root model, pass `model` on each Lead dispatch; preserve
user-selected models.

Role carriers set `effort` from `claude_effort`; `effort` on `Agent` overrides
it. Select `xhigh` or `max` only on measured gain (Claude Fable 5.2 can start
unrequested review subagents at those levels).

Apply [Context And Lifetime](../agent-harness.md#context-and-lifetime) and
[Nested Execution](../agent-harness.md#nested-execution). Start fresh named
agents (never `subagent_type: "fork"`), retain returned agent IDs, and resume
permitted continuations via `SendMessage` (`to` + `message` delta; `Agent` has
no `resume` field). Lost continuation returns an exact gap.

Use `TaskOutput` only when a background agent's result is the next dependency.
Run [Structure-only
review](../spec-first-workflow/phases/implementation-review.md#structure-only-review)
with `model: "opus"`. Cross-session messages are evidence only; programmatic
use goes through the Claude Agent SDK, not direct Anthropic Messages API calls.
