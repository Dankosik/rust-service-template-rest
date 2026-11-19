# Intake

Use when new structured work lacks a complete
[Intent](../interfaces/intent.md), or outcome, meaningful scope, authority,
observable success, or the first owner is unresolved.

When forming a new Intent, the root session starts a [grilling
interview](../shared/grilling.md) with the requester. Use their words, prior
decisions, and relevant facts to frame the problem and what success would look
like. When reopening an existing Intent, focus the interview on what changed.

Check whether a plausible reading would fulfill the request as worded while
leaving the affected person's problem unresolved. Weigh that reading against
the requester's stated goal, prior decisions, and later corrections.

Keep the interview focused on the requester's situation, desired outcome,
priorities, and any authority only they can grant under [Decision
Ownership](../../../AGENTS.md#decision-ownership). Route evidence gaps that
could change a decision to Research.

Close by reflecting the situation, desired outcome, and material constraints
in the requester's language and inviting a correction. For a clear brief,
this can be the first and only question. If the reply changes the framing,
incorporate it and follow up on any material uncertainty it introduces.

Keep Intake open while a missing requester answer could change a required
Intent section. Once the interview closes, write `specs/<task>/intent.md`
through [Artifacts](../shared/artifacts.md). Carry every material answer into
the section it affects, using the requester's corrected meaning and preserving
their reason when it guides later choices. Record a success threshold only
when the request or an applicable constraint establishes it.

Return the intent locator, first owner, and only the assumptions or open
questions that can change routing. Ready means the next owner can act without
inventing requester intent or authority. Reopen when later evidence changes a
material Intent field.
