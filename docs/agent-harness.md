# Agent Harness

Harness-neutral native delegation and durable controls. `AGENTS.md` owns
authority; workflow phases own why a control applies; the selected adapter owns
how the harness provides it.

## Delegation Interface

Across checkout or actor changes, carry workflow revision and instruction
locators, verify effective model and effort natively, and resolve material drift
with the continuation owner before dependent work while keeping checkout
state and user or native authority intact. Skip bootstrap turns when owners are
unchanged.

Brief each delegated lane with its checkable outcome, relevant context and
paths, writable scope when mutating, and stop or proof boundary without copying
repository-wide rules.
Share the checkout across read-only lanes, and across concurrent mutable units
or lanes when writable owners and locks are disjoint; serialize overlapping
mutations, reserving isolated worktrees for concurrent conflicting edits where
isolation costs less than waiting.

Count the active tree (Leads, mutable descendants, review and validation lanes)
against capacity, reserving slots for review, unlock, and landing.

Trigger [Parent-Owned
Recovery](spec-first-workflow/shared/transition.md#parent-owned-recovery) on
native failure, missed checkpoint, or repeated uninformative attempts rather
than quiet execution time. Before replacing a parent, reassigning scope, or
resetting a stalled subtree, preserve edits and findings, reconcile state, and
confirm descendant termination natively.

Pass model, effort, isolation, identity, and lifecycle in native tool fields.
Choose capability from open judgment, uncertainty, protected risks, and
interacting invariants: use balanced settings for closed work, raise capability
when signals conflict or an attempt/review exposes a missed invariant (retaining
it through repair), and run children on the lightest sufficient settings while
honoring user-selected models.

Dispatch unit Leads via the selected adapter's Acceptance-Unit Lead carrier, and
leaf lanes via `evidence-agent`, `specialist-agent`, `worker-agent`,
`reviewer-agent`, or `adjudicator-agent`; put expertise or review lenses in
`Method` and quality tiers in model/effort fields.

## Context And Lifetime

| Owner | Lifetime |
| --- | --- |
| Orchestrator | The ledger; recover from canonical state if unreliable. |
| Architect / phase owner | Macro phase; independent design or new phase uses a fresh actor. |
| Lead | Its unit; related-unit reuse only under Planning Ledger Contract. |
| Execution lane | One bounded brief and its corrections, including smaller models. |
| Evidence lane | One bounded evidence brief and related follow-ups below. |

A fresh phase session starts with the adapter's normal project instructions
and a brief naming the phase, accepted input files, writable scope, and expected
result. Exclude the coordinator's and earlier phases' chat turns, tool outputs,
and conversation summaries. Give the actor access to the checkout containing
those inputs, including uncommitted artifacts. A new worktree or session ID
alone does not establish fresh history.

Keep steps and repairs of the same macro phase with its owner. Independent
review gets its own actor under [Review](spec-first-workflow/shared/review.md).
Start execution and evidence lanes, including descendants, with the same
history boundary and the tools and permissions appropriate to their brief.

Reuse a worker only for corrections—or a parent-granted disjoint writable-scope
expansion—inside its unchanged outcome, capability tier, and proof boundary; end
it once consumed. Evidence lanes may answer related follow-ups for the same
parent decision when independence is unneeded, rechecking affected assumptions.

When reconciled guidance or a decision changes an active child's inputs or
constraints inside its unchanged outcome, capability tier, and current or
parent-expanded disjoint writable scope, notify only each affected child
directly, one-to-one, with the decision delta, its consequence for that child's
assignment, and any updated artifact locator rather than relying on silent file
edits or broadcasting to unaffected lanes.

Use a fresh agent (stopping the old child and reconciling its partial edits
first) when a change alters outcome, reassigns or narrows writable scope, or
changes required capability, or for an unrelated investigation or independent
judgment (Lead reuse excludes execution lanes). On stalled diagnosis or
unreliable context, change strategy via the parent rather than copying failed
transcripts or rotating by time/turn count. Follow-ups continue history without
resetting context.

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

If a required project role is missing from a source checkout, check the
[template's harness setup](../README.md#how-agents-work-here) before treating
the native control as unavailable. A source-only instruction check can pass
before the selected adapter files have been generated.

| Current harness | Adapter |
| --- | --- |
| Codex App or Codex CLI | [Codex](agent-harness/codex.md) |
| Claude Code CLI, desktop, web, or IDE | [Claude Code](agent-harness/claude-code.md) |
| Qwen Code CLI or IDE | [Qwen Code](agent-harness/qwen-code.md) |
| Grok Build CLI, TUI, or ACP | [Grok Build](agent-harness/grok-build.md) |
| Cursor IDE, CLI, or Cloud Agents | [Cursor](agent-harness/cursor.md) |
| OpenCode CLI, TUI, desktop, or IDE | [OpenCode](agent-harness/opencode.md) |
| Antigravity App, CLI, or IDE | [Antigravity](agent-harness/antigravity.md) |

Never mix control planes in one outcome. A task, subagent, worktree, model, or
Goal is a carrier—never authorization or unit ownership. Use a durable Goal
only for genuinely long-running/resumable work meeting native auth and
capability rules.

All adapters preserve the same ledger, unit ownership, review, and transition
semantics. Select topology from callable controls, not product name, and
report an unavailable full-ledger carrier as a capability gap instead of
shrinking the workflow.
