# Agent Harness

Harness-neutral native delegation and durable controls. `AGENTS.md` owns
authority; workflow phases own why a control applies; the selected adapter owns
how the harness provides it.

## Delegation Interface

- **Brief sparsely and verify native controls:** pass each lane's checkable
  outcome, authoritative paths, writable scope (when mutating), and stop or
  proof boundary without copying repository-wide rules. Pass model, effort,
  isolation, identity, and lifecycle in native tool fields, and verify the
  effective model/effort natively before relying on dependent output.
- **Role routing:** dispatch unit Leads via the selected adapter's
  Acceptance-Unit Lead carrier, and leaf lanes via `evidence-agent`,
  `specialist-agent`, `worker-agent`, `reviewer-agent`, or `adjudicator-agent`.
  Put expertise or review lenses in `Method` and quality tiers in model/effort
  fields.
- **Capability routing:** choose model and effort from open judgment,
  uncertainty, protected risks, and interacting invariants. Use balanced
  settings for closed work, raise capability when signals conflict or a failure
  exposes a missed invariant (retaining it through repair), and run children on
  the lightest sufficient settings while honoring user-selected models.
- **Checkout isolation:** share the checkout across read-only lanes and when
  only one mutable writer is active. When two or more mutable units run in
  parallel, use isolated worktrees anchored to the integration tip whenever the
  adapter supports them (avoiding Cargo `target/` lock contention, transitive
  build breakage, and mixed diffs); when it lacks isolation, serialize
  overlapping writers and scope edits/checks strictly to each unit's paths.
- **Capacity and stall recovery:** count the entire active tree (Leads, mutable
  descendants, review and validation lanes) against capacity, reserving slots
  for review, recovery, and landing. Trigger [Parent-Owned
  Recovery](spec-first-workflow/shared/transition.md#parent-owned-recovery) on
  native failure, missed checkpoint, or repeated uninformative attempts—never on
  quiet execution time alone. Before replacing an active Lead or resetting a
  stalled subtree, preserve partial edits and findings, reconcile state, and
  confirm descendant termination natively.

## Context And Lifetime

| Owner | Lifetime |
| --- | --- |
| Orchestrator | The ledger; recover from canonical state if unreliable. |
| Preparation session | Continuous root session from Intake through Planning (fresh subagents only for evidence scans, `Design It Twice`, and `Plan Review`). |
| Lead | Its unit until integrated; fresh Lead per task and per batched repair pass under [Implementation](spec-first-workflow/phases/implementation.md#orchestrator-loop). |
| Execution lane | One bounded brief and its corrections, including smaller models. |
| Evidence lane | One bounded evidence brief and related follow-ups. |

Keep preparation (Intake through Planning, plus Plan Review repairs) in the
continuous root session. Start every subagent—Lead, `evidence-agent`,
`specialist-agent`, `reviewer-agent`, `adjudicator-agent`, or `worker-agent`—with
a fresh conversation (no parent chat turns, tool outputs, or summaries) and
access to the checkout containing accepted inputs; a new worktree or session ID
alone does not establish fresh history.

- **Continue an existing child:** only for corrections, a parent-granted
  disjoint scope expansion, or an evidence follow-up inside its unchanged
  outcome, capability tier, and proof boundary; end finished lanes once
  consumed.
- **Steer an active child:** when a decision changes its inputs inside its
  unchanged outcome and scope, notify only each affected child directly,
  one-to-one, with the delta, its consequence, and updated artifact locators
  rather than relying on silent file edits or broadcasting to unaffected lanes.
- **Spawn a fresh child** (stopping the old child and reconciling its partial
  edits first): whenever outcome, capability tier, or narrowed/reassigned scope
  changes, or for independent judgment or a new strategy after a stall—change
  strategy via the parent rather than copying failed transcripts or rotating by
  turn count.

## Nested Execution

Orchestrator owns the ledger, assembled verification, and review routing; Leads
own their unit and return `Implemented`. Workers delegate strict subsets only
when the adapter permits descendants and ownership and lock rules hold. At a
depth or tool limit, the nearest capable parent dispatches or completes the
subset directly without inventing an acceptance unit or unsupported carrier.

Each delegating parent owns its subtree: retain child identities, assign
disjoint writable scopes (never edited concurrently by the parent), integrate
results serially, and stop or join descendants before returning, freezing, or
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
