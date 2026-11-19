# Grilling

Work unresolved decisions as a **design tree** in **frontier rounds**.

- **Ask the independent frontier in one round:** every unresolved decision whose
  prerequisites are settled; hold back questions depending on an open answer.
  When the goal is foggy, start with one open question inviting a concrete
  situation and desired outcome in the requester's words.
- **Number questions for fast replies:** state the distinct outcomes and
  tradeoffs; when evidence supports a direction, offer a recommendation worded
  so `"yes"` accepts it (`1 yes, 2 B, 3 yes`).
- **Finding facts and resolving technical choices is the agent's job:** look up
  accessible codebase, contract, or config facts directly (or via a fresh
  `evidence-agent`) and resolve technical questions via their responsible owner
  under [Decision Ownership](../../../AGENTS.md#decision-ownership) instead of
  asking the requester; don't block independent frontier questions on an
  unrelated lookup.

Wait for the reply. Briefly reflect what changed when an answer shifts
understanding, recompute the frontier, and repeat until empty.
