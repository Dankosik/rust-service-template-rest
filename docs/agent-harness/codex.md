# Codex Harness Adapter

Use installed Codex schemas and tool instructions as native authority; field
availability alone does not authorize use.

## Native Map

- Root task using `$orchestrator` owns the persisted ledger. Use collaboration
  subagents for all internal execution and review (including mutable and
  cross-repo work); never create separate Codex App chats or use App task
  handoff as a carrier or fallback.
- Under [Transition](../spec-first-workflow/shared/transition.md#cross-phase-continuation),
  map phase actors to fresh collaboration subagents and the continuation owner
  to the root.
- Subagents inherit the parent's working context. For a neighboring repository,
  apply [Authority](../../AGENTS.md#authority) and target it in
  commands/navigation; brief paths or command `cwd` do not reload that project's
  config, roles, or tools.
- For each ready ledger unit, spawn a general-purpose subagent with
  [Implementation](../spec-first-workflow/phases/implementation.md) as Method
  and that packet as boundary (it is Acceptance-Unit Lead; `worker-agent` and
  read-only roles lack acceptance authority). A root-local Lead handles
  single-unit Implementation.
- When [Agent Harness](../agent-harness.md) selects isolation, assign a
  repo-native Git worktree's absolute root; share checkouts only with disjoint
  writable owners and locks. Report missing tools, config, or write access as
  a capability gap rather than switching control planes.
- Phase actors and Leads call collaboration tools directly for descendants.
- A Goal is optional and thread-local; create one only on explicit user/system
  instruction (duration is not authorization).

## Nested Execution

`.agents/codex-project.toml` (`scripts/codex-agents-sync.sh`) enables agents
and Multi-Agent V2. V2 ignores `agents.max_depth` (do not add one); its
20-open-subagent ceiling limits session-tree concurrency (excluding root), not
depth. At capacity, finish existing work or execute bounded work locally within
role authority.

## Models

When assigning models natively, use `gpt-6-astra` for decision-owning roles:
Ledger Orchestrator, phase owner, Acceptance-Unit Lead, `specialist-agent`,
`reviewer-agent`, and `adjudicator-agent`. Preserve explicit user-selected
models and effort (including a non-Astra root); never rewrite root settings.

Astra supports `low`, `medium`, `high`, `xhigh`, `max`, and `ultra` (prefer
`high` when assigning effort):

| Effort | Use when |
| --- | --- |
| `low` | Mechanical retrieval or readback without new judgment; prefer a bounded helper if worthwhile. |
| `medium` | Coordination only applies closed ledger decisions or routes known results. |
| `high` | Default for phase decisions, implementation ownership, acceptance, domain judgment, and review. |
| `xhigh` | Interacting invariants, ambiguous recovery, weak proof, material reviewer conflict, or failed causal attempt. |
| `max` | Unresolved reasoning gap remains after `xhigh` and depth justifies cost. |
| `ultra` | Maximum reasoning with auto-delegation, only if harness preserves topology, authority, and capacity (never auto-select). |

Raise effort before the affected decision or repair, and choose afresh after a
unit closes; missing facts or broken tools require owner recovery.

Override natively only for bounded execution/evidence work: `gpt-5.6-luna`
(`low`) for closed mechanical work, `gpt-5.6-terra` (`medium`, or `high`/`xhigh`
for harder implementation inside an accepted contract). Astra keeps decision
and acceptance authority; unresolved judgment returns to Astra and raises
Astra's effort. Use `evidence-agent` for advisory research, not a lower-model
`specialist-agent` or `reviewer-agent`. Never select `gpt-5.6-sol` for any
role, execution, evidence, escalation, or fallback.

If Astra is unavailable or rejected, retain the failure and stop the dependent
decision/acceptance—never fall back to an execution model. Verify effective
models natively; prose names are not native selections.

## Dispatch And Coordination

Pass `model` and `reasoning_effort` on the initial `collaboration.spawn_agent`
call (`fork_turns: "none"` for execution lanes, evidence lanes, and independent
review; `"all"` inherits parent settings and rejects overrides). Never use a
bootstrap turn or follow-up to bypass initial selection.

Use `collaboration.list_agents` to inspect the tree and `collaboration.wait_agent`
only on lanes with returned identities. `collaboration.send_message` steers an
active turn without starting one; `collaboration.followup_task` wakes an
idle/completed agent within [Context And Lifetime](../agent-harness.md#context-and-lifetime)
or permitted Lead reuse (re-evaluating model/effort). Neither changes
model/effort: escalate by stopping the old lane, reconciling its edits, and
spawning a fresh agent. Never wait on an idle lane or blindly repeat uncertain
dispatch; reconcile once from native status, ledger, and Git candidate identity.

For isolated work, validate worktree and base before accepting bytes. An
`Implemented` Worktree Lead returns an Acceptance Result with fixed candidate
and exact `HANDOFF_READY`; the Orchestrator or root-local Lead lands it
serially without re-adjudicating.

## Review And Recovery

When [Review](../spec-first-workflow/shared/review.md) selects independent
review, spawn a fresh `reviewer-agent` (`fork_turns: "none"`) with the phase
review Method. Keep agent identities across [Transition](../spec-first-workflow/shared/transition.md)
reopens and interruptions to reconcile completion and resume from the verified
checkout.
