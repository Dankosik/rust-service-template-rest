# Codex Harness Adapter

Use installed Codex schemas and tool instructions as native authority; field
availability alone does not authorize use.

## Native Map

- The root task bound as Ledger Orchestrator by [Implementation](../spec-first-workflow/phases/implementation.md#carrier)
  owns the persisted ledger. Use collaboration
  subagents for all internal execution and review (including mutable and
  cross-repo work); never create separate Codex App chats or use App task
  handoff as a carrier or fallback.
- Under [Transition](../spec-first-workflow/shared/transition.md#cross-phase-continuation),
  map preparation phases to fresh collaboration subagents with
  `fork_turns: "none"`. The root handles Intake and coordinates those phases.
- Subagents share the parent's working directory. For a neighboring repository,
  apply [Authority](../../AGENTS.md#authority) and target it in
  commands/navigation; brief paths or command `cwd` do not reload that project's
  config, roles, or tools.
- For each ready ledger unit, spawn a general-purpose subagent with
  [Unit Implementation](../spec-first-workflow/phases/implementation/unit.md) as Method
  and that packet as boundary (it is Acceptance-Unit Lead; `worker-agent` and
  read-only roles lack acceptance authority). A root-local Lead handles
  a single fixed inline unit.
- When [Agent Harness](../agent-harness.md) selects isolation, assign a
  repo-native Git worktree's absolute root; share checkouts only with disjoint
  writable owners and locks. Report missing tools, config, or write access as
  a capability gap rather than switching control planes.
- Phase actors and Leads call collaboration tools directly for descendants.
- A Goal is optional and thread-local; create one only on explicit user/system
  instruction (duration is not authorization).

## Nested Execution

`.agents/codex-project.toml` (`scripts/harness/sync.py`) enables agents
and Multi-Agent V2. V2 ignores `agents.max_depth` (do not add one); its
20-open-subagent ceiling limits session-tree concurrency (excluding root), not
depth. At capacity, finish existing work or execute bounded work locally within
role authority.

## Models

Preserve the root session settings. For each subagent, choose a model and
reasoning effort based on the judgment, risk, and proof its assignment requires.
Honor explicit user choices within their requested scope. Use the same criteria
when recommending a root model.

| Work | Starting model and effort |
| --- | --- |
| Implementation within an accepted contract, routine coordination, local diagnosis, or routine review | `gpt-6.2-sol` at `medium`. Use `high` for multiple execution paths or substantial edge-case analysis. |
| Open architecture decisions, decisions with material risks to authorization, money, or data integrity, disputed acceptance, or independent review of a high-risk change | `gpt-6.2-astra` at `high` when available, otherwise `gpt-6-astra` at `high`. Use `xhigh` for interacting invariants, conflicting evidence, or a failed attempt that exposed a material gap. |
| Bounded fact collection or mechanical checks | Work locally when short. For a useful parallel subtask, use `gpt-6-luna` at `high` when supported, or `gpt-6.2-sol` at `low` for simple retrieval. |

Use `max` when a reasoning gap remains after `xhigh` and more depth justifies
the cost. Resolve missing facts or tool failures through owner recovery. Use
`ultra` only on explicit user choice and when its automatic delegation fits
the workflow's authority, topology, and capacity.

If a helper model is unavailable, the parent may complete the work within its
role and the review's independence requirements. If Astra is required and no
supported Astra can take the assignment, report the capability gap, stop the
dependent decision or acceptance, and continue independent work.

See OpenAI's [model guide](https://developers.openai.com/codex/models) and
[subagent guidance](https://learn.chatgpt.com/docs/agent-configuration/subagents#choosing-models-and-reasoning)
for model-specific starting points.

## Dispatch And Coordination

Check the chosen pair against the callable schema. On every
`collaboration.spawn_agent` call, pass both `model` and `reasoning_effort` with
`fork_turns: "none"` when native controls allow explicit selection. If native
controls require inheritance, use it only when the parent pair matches the
chosen pair and the assignment's history requirements are met. Otherwise apply
the unavailable-model guidance above. Verify the effective pair natively before
relying on it for dependent work.

Use `collaboration.list_agents` to inspect the tree and `collaboration.wait_agent`
only on lanes with returned identities. `collaboration.send_message` steers an
active turn without starting one; `collaboration.followup_task` wakes an
idle/completed agent within [Context And Lifetime](../agent-harness.md#context-and-lifetime)
or permitted Lead reuse (re-evaluating model/effort). Neither changes
model/effort: escalate by stopping the old lane, reconciling its edits, and
spawning a fresh agent. Never wait on an idle lane or blindly repeat uncertain
dispatch; reconcile once from native status, ledger, and Git candidate identity.

For isolated work, validate worktree and base before accepting bytes. An
`Implemented` Worktree Lead returns [Acceptance
Result](../spec-first-workflow/interfaces/acceptance-result-v1.md) with a fixed
`candidate`; the Orchestrator or root-local Lead lands it serially without
re-adjudicating.

## Review And Recovery

When [Review](../spec-first-workflow/shared/review.md) selects independent
review, spawn a fresh `reviewer-agent` (`fork_turns: "none"`) with the phase
review Method. Keep agent identities across [Transition](../spec-first-workflow/shared/transition.md)
reopens and interruptions to reconcile completion and resume from the verified
checkout.
