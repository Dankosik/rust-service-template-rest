# Intake

Use when new structured work lacks a complete
[Intent](../interfaces/intent.md), or outcome, meaningful scope, authority,
observable success, or the first owner is unresolved.

When forming a new Intent, the root session runs a [grilling
interview](../shared/grilling.md) with the requester, grounding questions in
their words, prior decisions, and primary codebase/contract facts (directly or
via a fresh `evidence-agent`) so code-exposed product forks settle before
Definition. Route deeper external-contract or baseline gaps to
[Research](research.md).

When reopened with `specs/<task>/spec.md` at `status: blocked`, open the
interview with its `Blocked decisions` (`B1..Bn`) frontier as the first
grilling round, follow up on any newly revealed requester uncertainty until the
frontier closes, fold answers into `specs/<task>/intent.md`, and return control
so Definition can resume without re-asking settled branches. On any other
reopen, interview strictly on what changed.

Check whether a plausible reading fulfills the wording while leaving the
requester's real problem unsolved, weighing it against their goal, prior
decisions, and corrections. Keep questions focused on situation, desired
outcome, priorities, and user-only authority under [Decision
Ownership](../../../AGENTS.md#decision-ownership). Close by reflecting the
situation, desired outcome, and material constraints in the requester's words
and inviting a correction (for a clear brief, this can be the only round).

Keep Intake open while a missing answer could change a required Intent section.
Once closed, write `specs/<task>/intent.md` through
[Artifacts](../shared/artifacts.md), carrying every material answer and its
guiding reason into the affected section and recording numeric thresholds only
when the request or an applicable constraint establishes them.

Return the intent locator, first owner, and only assumptions or open questions
that can change routing. Ready means the next owner can act without inventing
requester intent or authority; reopen when later evidence changes a material
Intent field.
