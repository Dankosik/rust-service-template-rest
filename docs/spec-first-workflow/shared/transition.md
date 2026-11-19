# Transition

Use when a result may move or reopen a macro phase, or cross an actor/session
boundary.

Move once the current owner is `ready`, its triggered decisions have a
disposition, required review permits movement, and the next owner has the
accepted inputs and authority needed to make its own decisions. Mark
untriggered phases `skipped` directly, use inline movement within a session,
and emit [Transition Result V1](#transition-result-v1) at durable macro-phase
boundaries.

Carry only the current result, authoritative locators, movement evidence,
candidate identity when relevant, proof boundary, exact blocker or next action,
and reopen condition. Package next-session prompts through [Prompt
Composition](../../prompt-composition.md), and report same-phase repair or
context rollover as resume state.

For a persisted Implementation ledger, [Acceptance
Transition](../phases/implementation.md#3-acceptance-transition)
owns the immediate `Implemented`, `Accepted`, or `Blocked` transition, while
fixed inline units move directly.

Reopen only the smallest owner invalidated by current evidence while preserving
unaffected decisions and proof. Pause the current phase actor at an explicit
phase boundary, unavailable required external input, new authority boundary, or
required durable handoff; stop the whole request on an explicit user stop.

On a mechanical locator or receipt refresh after an accepted repair, inspect the
exact delta, update current identities, and retain the earlier verdict for
unchanged semantic scope; route any change in meaning, mechanism, oracle, risk,
or dependency admission through its owner and review.

## Cross-Phase Continuation

After Intake, the root coordinator dispatches one applicable preparation phase
at a time in a [fresh phase session](../../agent-harness.md#context-and-lifetime)
through the selected adapter. Include the intent path in the phase brief.
Wait for its reviewed Transition Result,
re-read the authoritative artifacts, and continue through Planning or the
user's earlier stopping point. Return a change in requester meaning to Intake
and reopen only the results it invalidates. Persist coordination only under
[Artifacts](artifacts.md). Phase results remain the decision authority.

The phase actor owns its decision, artifact, repair, and movement evidence. When
native nesting cannot support a needed specialist or independent reviewer, the
coordinator dispatches that bounded request on the actor's behalf and relays the
result. This request is not phase movement; only the phase actor returns its
reviewed Transition Result. The coordinator does not substitute its own review
or decision for the actor's.

If the harness cannot provide the required fresh session, apply
[Parent-Owned Recovery](#parent-owned-recovery) and preserve the resumable
result. When replacing a coordinator during recovery, transfer its continuation
state and result-delivery route to a single successor.

## Implementation Handoff

When Planning returns a reviewed, ready `tasks.md`, leave the ledger and its
inputs available in the same checkout. Present a compact one-screen plan
overview in the user's language—each task's `T<ID>`, one-line deliverable,
non-default shape when applicable (`prefactor`, `shared seam`,
`integration-first`, `expand`/`migrate`/`contract`), `Depends on`, initial Ready
Frontier (noting any shared `Exclusive locks` that serialize otherwise parallel
tasks), and any carried `CONCERNS` risks—so the user can verify slicing,
granularity, and real parallelism at a glance. Follow with one message to
send in a new session there, in their language, with the actual ledger path,
such as “Implement the plan in `specs/order-export/tasks.md`.” Include the
selected adapter's native invocation in the handoff when needed. End the
planning session's work here. Only the user starts the implementation session.

If Planning proves there is no implementation work, report that outcome.
If the plan is blocked, return the missing input and its owner under
[Parent-Owned Recovery](#parent-owned-recovery). Offer the launch message only
for a ready ledger.

## Parent-Owned Recovery

A delegated blocker, missing proof, or technical-policy gap remains work for
the requesting parent, including a root-local Lead. Without a separate parent,
the current task owner retains that responsibility. Identify the dependent
action and smallest decision or evidence owner. Close the input directly when
the parent's role and active phase permit it; otherwise dispatch that owner
through an authorized carrier and consume its result. Keep required phase,
review, and acceptance boundaries intact.

When technical recommendations conflict, the responsible decision owner
compares evidence and constraints, obtains a discriminating probe or specialist
judgment when needed, and resolves each material objection against the accepted
drivers. Converge on an evidence-supported choice and reopen condition; do not
use a vote or unanimity as proof. When equivalent options survive, the decision
owner chooses the simplest one satisfying the constraints.

For uncertain recovery, name the next discriminating result and a bounded work
window in existing task state before another attempt. On that checkpoint or a
repeated result with no new evidence, compare actual progress with the intended
result. Continue while findings close or a concrete available probe can
distinguish remaining causes. Changing reviewer, owner, or reasoning effort
does not reset a no-progress recovery; reworded concerns, elapsed time, and
repeated status reports are not new evidence.

For the same unresolved finding without new evidence, use one focused
independent diagnosis when needed, not another whole-unit review. The
responsible decision owner resolves the finding against the accepted contract
and resulting evidence. Reuse that diagnosis until new evidence invalidates it;
another actor or higher effort alone is not a new recovery path. Confirmed
defects and missing mandatory proof still prevent acceptance.

When no available authorized probe or recovery can obtain the required evidence,
retain `Blocked` with the exact unverified claim, limitation, and input or
capability that would permit resumption. Do not redispatch the unchanged gap.
A work-window limit changes strategy; it neither waives proof nor creates a
new task boundary, user approval, or success claim. Retain useful partial work.

Continue independent authorized work, then resume the blocked unit from the
resolved input. A terminal child result does not complete the parent's outcome.
Escalate user-owned decisions or unavailable external inputs and authority only
through [Decision Ownership](../../../AGENTS.md#decision-ownership). If required
evidence or native capability remains unavailable after authorized recovery,
report the exact limitation and incomplete claim; do not invent success or ask
the user to choose a technical mechanism.

## Transition Result V1

Return one durable macro-phase receipt:

```text
status: ready | blocked | skipped
owner: <phase>
result: <artifact path or inline result>
review: <Review Result V1 locator or inline result; none for blocked/skipped>
movement_evidence: <why the next owner may act, or none>
reopen_owner: <owner or none>
next_owner: <owner or none>
```

Use no receipt for ordinary inline movement.
