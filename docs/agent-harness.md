# Agent Harness

Harness-neutral native delegation and durable controls. `AGENTS.md` owns
authority; workflow phases own why a control applies; the selected adapter owns
how the harness provides it.

## Delegation Interface

Across checkout or actor changes, keep workflow revision and instruction
locators, verify effective model/effort natively, and resolve material drift
with the continuation owner before dependent work (never mix policies,
bulk-sync active checkouts, or override user/native authority). Unchanged
owners need no bootstrap turn.

Send only [Subagent Brief Template](subagent-brief-template.md) fields.
Read-only lanes share the checkout. Concurrent mutable units/lanes share a
checkout only when mutable owners and locks are disjoint; otherwise serialize,
or isolate only when cheaper than waiting—never for sequential work, cheap
disjoint units, or read-only review.

Count the active tree (Leads, mutable descendants, review/validation lanes)
against capacity, reserving slots for review, unlock, and landing.

Silence is not a stall; trigger [Parent-Owned
Recovery](spec-first-workflow/shared/transition.md#parent-owned-recovery) on
native failure, missed checkpoint, or repeated uninformative attempts. Before
replacing a parent, reassigning scope, or resetting a stalled subtree, preserve
edits/findings, reconcile state, and confirm descendant termination natively.

Carry model, effort, isolation, identity, and lifecycle in tool fields. Select
capability from remaining judgment, uncertainty, protected risks, and
interacting invariants—not locks or unwritten tests. Use balanced settings for
closed work; raise capability when signals disagree or a causal attempt/review
exposes a missed invariant (not for expected test failures or syntax/setup
errors), and keep it through repair. Children use the cheapest permitted
settings; preserve user-selected models.

Use `evidence-agent`, `specialist-agent`, `worker-agent`, `reviewer-agent`, or
`adjudicator-agent`; put expertise or review lenses in `Method` and quality tiers
in model/effort fields.

## Context And Lifetime

| Owner | Lifetime |
| --- | --- |
| Orchestrator | The ledger; recover from canonical state if unreliable. |
| Architect / phase owner | Macro phase; independent design or new phase uses a fresh actor. |
| Lead | Its unit; related-unit reuse only under Planning Ledger Contract. |
| Execution lane | One bounded brief and its corrections, including smaller models. |
| Evidence lane | One bounded evidence brief and related follow-ups below. |

Start execution/evidence lanes (including descendants) with fresh history
excluding parent conversation while keeping instructions, tools, permissions,
and repo files; supply accepted behavior, decisions, locators, writable scope,
and expected outcome. Reviewer freshness follows
[Review](spec-first-workflow/shared/review.md).

Reuse a worker only for corrections inside its unchanged outcome, writable
scope, and proof boundary; end it once consumed. Evidence lanes may answer
related follow-ups for the same parent decision when independence is unneeded,
rechecking affected assumptions.

Use a fresh agent for a new outcome, writable scope, unrelated investigation, or
independent judgment (Lead reuse excludes execution lanes). On stalled
diagnosis or unreliable context, change strategy via the parent rather than
copying failed transcripts or rotating by time/turn count. Follow-ups continue
history without resetting context.

## Nested Execution

Orchestrator owns the ledger; Leads return `Implemented`; one assigned delivery
owner owns final validation and acceptance. Workers delegate strict subsets
only if the adapter permits descendants and Implementation's benefit,
ownership, and lock rules hold. Missing product/architecture decisions return to
their owner; unwritten tests do not block delegation. At a depth/tool limit, the
nearest capable parent dispatches or completes the subset without inventing an
acceptance unit or unsupported carrier.

Each delegating parent owns its subtree: retain child identities, assign
disjoint writable scopes (never edited concurrently by the parent), integrate
results serially, and join/stop descendants before returning, freezing, or
releasing scope.

## Select One Adapter

| Current harness | Adapter |
| --- | --- |
| Codex App or Codex CLI | [Codex](agent-harness/codex.md) |
| Claude Code CLI, desktop, web, or IDE | [Claude Code](agent-harness/claude-code.md) |
| Qwen Code CLI or IDE | [Qwen Code](agent-harness/qwen-code.md) |
| Grok Build CLI, TUI, or ACP | [Grok Build](agent-harness/grok-build.md) |
| Cursor IDE, CLI, or Cloud Agents | [Cursor](agent-harness/cursor.md) |
| OpenCode CLI, TUI, desktop, or IDE | [OpenCode](agent-harness/opencode.md) |

Never mix control planes in one outcome. A task, subagent, worktree, model, or
Goal is a carrier—never authorization or unit ownership. Use a durable Goal
only for genuinely long-running/resumable work meeting native auth and
capability rules.

All adapters preserve the same ledger, unit ownership, review, and transition
semantics. Select topology from callable controls, not product name, and
report an unavailable full-ledger carrier as a capability gap instead of
shrinking the workflow.
