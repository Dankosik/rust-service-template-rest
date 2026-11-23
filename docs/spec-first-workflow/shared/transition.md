# Recovery And Handoff

Governs handing off a ready plan to a fresh Implementation session and
recovering autonomously from blockers, conflicting recommendations, or stalled
attempts.

## Implementation Handoff

When [Planning](../phases/planning.md) clears [Plan
Review](../phases/plan-review.md) with a `status: ready` `tasks.md`, leave the
plan bundle in the same checkout and present a compact one-screen overview in
the user's language:

- each task's `T<ID>`, one-line deliverable, non-default shape tag, and `Depends
  on`, plus the ledger's `Completion` target;
- execution waves (initial `Ready Frontier`, parallel waves vs shared-lock
  serializations);
- any carried `CONCERNS` or `Reopen if` risks.

Follow with one message the user can paste into a **fresh session** with the
actual ledger path (for example: *"Implement the plan in
`specs/order-export/tasks.md`."*), including any native entry invocation the
active harness needs. End preparation here so Implementation starts in a clean
context window.

If Planning proves zero implementation work remains, report the verified
evidence directly instead of emitting a launch message.

## Parent-Owned Recovery

A delegated blocker, missing proof, or technical conflict belongs to the
requesting parent—never push technical choices or manual debugging onto the
user.

- **Resolve conflicts by evidence:** compare constraints and primary evidence,
  run a discriminating probe or specialist lane when needed, and converge on
  the simplest option meeting all constraints with an objective `Reopen if`.
- **Bound no-progress loops:** before retrying an uncertain failure, state the
  discriminating hypothesis and a bounded checkpoint. Changing actor, model, or
  effort without new evidence does not reset a stalled loop. On the same
  unresolved finding without new evidence, run one focused independent
  diagnosis (or `adjudicator-agent` for a reviewer dispute), not another
  whole-unit review.
- **Stop cleanly only when truly blocked:** if no authorized probe or recovery
  can obtain required evidence or authority, keep `Blocked` with the exact
  unverified claim, preserved partial work, and the missing input or authority
  required to resume. Continue independent authorized work first.
- **Escalate strictly under [Decision Ownership](../../../AGENTS.md#decision-ownership):**
  surface only genuine user-owned product/risk choices or unavailable external
  authority, framed in user-visible consequences.
