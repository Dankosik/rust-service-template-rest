# Specification Review

Use when shared [Review](../shared/review.md) routes one fixed specification or
the user explicitly requests that standalone review. This adapter owns only the
Specification falsifiers and threshold.

Try to construct two counterexamples from the accepted intent and mandatory
constraints. Consider interacting rules together when their combination
affects the outcome.

- A scenario satisfies every clause yet the actor misses the accepted outcome.
- An outcome or implementation meets the accepted goal and its mandatory
  constraints, yet a clause excludes it without an authorized reason.

When a scenario exposes a mismatch, give its starting state, trigger, and
observable result.

## Lenses

- coverage of a materially affected or deliberately unchanged surface;
- factual support, authority, invariant, compatibility/failure expectation, or
  feasible proof expectation;
- an assumption, owner boundary, irreversible consequence, or rollout
  compatibility claim without an evidence-backed disposition;
- a non-goal, risk acceptance, or proof item that hides a decision needed now;
- wording that lets two reasonable implementations disagree on the required
  trigger, outcome, transition/effect, truth/finality, rejection/replay/recovery,
  compatibility, or success meaning.

`PASS` requires no surviving material divergence. `CONCERNS` may carry only
a bounded proof/risk obligation over already accepted behavior. A surviving
material counterexample or a missing or contradictory decision about behavior,
policy, authority, compatibility, truth semantics, or success is `FAIL` and
reopens Specification or its named upstream owner.
