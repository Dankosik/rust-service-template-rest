# Resume

Use after context compaction, interruption, or a session/actor handoff; check
current repository and coordination state before continuing rather than relying
on a compaction summary.

For mid-task steering, reconcile new guidance against work already underway:
reopen the earliest affected decision, update its owning artifact and
consequences under [Artifacts](artifacts.md), notify affected live agents
one-to-one under [Agent Harness](../../agent-harness.md#context-and-lifetime),
invalidate only dependent work and proof, and continue unaffected work. A side
question or status request does not reset the task or require replaying
completed phases.

1. Inspect the current workspace, Git status/diff, and live native agent/task
   state; for Direct Work (no `specs/<task>/` artifacts), verify in-progress
   edits and checks directly from those sources before acting.
2. For structured work, read `tasks.md` first once Planning has persisted it.
   During preparation resume Planning review or its [handoff](transition.md#implementation-handoff).
   During implementation or validation read the index and current ready-frontier
   packets. Before a ledger exists, read `workflow-plan.md` when present.
3. Read only the decision, design, research, or rollout inputs needed by the
   next action.
4. Resolve conflict by reopening the narrowest authoritative owner; never merge
   competing decisions from chat or compaction summaries.
5. Resolve current files and writer identities before consuming Implemented
   outputs. Evaluate proof reuse only when final validation begins under the
   [Evidence Contract](evidence-contract.md). Resume the actual execution stage:
   coding uses Unit Implementation's checks while coding; final-validation repair
   includes its focused rerun under existing authority. Retire a superseded
   code-only repair brief without restarting the ledger or its review cycle.

Resolve the ledger's Execution locators against native task state before
resuming or redispatching; a stored locator alone does not prove work is active.
Use Resume notes only after checking them against current workspace and evidence.
Refresh large completed coordination from the canonical ledger, native task
status, and Git identities. Do not replay transcripts or duplicate native task
lifecycle state.
