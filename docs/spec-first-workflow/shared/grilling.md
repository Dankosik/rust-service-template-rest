# Grilling

Interview the requester **relentlessly** until you reach a **shared understanding**. Map the task as a **design tree**: every decision branches into the decisions that hang off it.

Classify each branch under [Decision Ownership](../../../AGENTS.md#decision-ownership) before asking:

- **Facts** (verifiable in repository code, contracts, `docs/decisions/`, or tools): resolve them yourself. Dispatch a fresh `evidence-agent` via [Read-Only Delegation](read-only-delegation.md) for read-heavy lookups so this session stays in the smart zone. Do not stall the round on a lookup: treat an in-flight lookup as an unsettled prerequisite, hold only the questions downstream of it, and ask the rest of the frontier now.
- **Technical choices** (internals, crates, seams, or mechanisms with no user-visible behavior, cost, or risk difference): own them inside the agent system and keep them off the user's frontier.
- **Low-stakes reversible conventions** (outcome-preserving, reversible, idiomatic): record them as labeled assumptions with an objective `Reopen if` trigger for final confirmation rather than grilling on trivia.
- **Requester-owned decisions** (desired behavior, scope and non-goals, compatibility or breaking stance, user-visible failure policy, `Must not` prohibitions, money, auth, privacy, data-loss or irreversible effects, priority tie-breakers, or contradictions with primary code): place them on the user's **frontier**.

Work the **design tree** in **rounds**. The **frontier** is every requester-owned decision whose prerequisites are settled—the questions you can ask now without guessing answers you have not heard yet. When the goal is foggy, open with one open question inviting a concrete situation and desired outcome in the requester's words. Otherwise ask the whole frontier in one round, in plain chat text, numbered for fast replies (`1 yes, 2 B, 3 yes`), with your recommended answer on its own line:

```text
❓ **Q1** - **<short decision title>**: <concise context anchor, concrete tradeoff or options in user terms>

➡️ <recommended answer and one-line reason>

---

❓ **Q2** - **<short decision title>**: <concise context anchor, concrete tradeoff or options in user terms>

➡️ <recommended answer and one-line reason>
```

Word each question so `"yes"` accepts your recommended answer. Wait for the user's answers before advancing dependent branches.

Each answered round reshapes the **design tree**: echo any non-obvious settled consequence in one sentence so a misreading surfaces immediately, prune invalidated sub-branches, recompute the **frontier**, and ask the next round. Keep any question that depends on an open question in this round for a later round.

Grilling has no fixed question cap; natural-language steering is the escape hatch. If the user asks to wrap up and assume the rest, convert remaining non-critical branches into labeled assumptions with `Reopen if` triggers while keeping hard [Authority](../../../AGENTS.md#authority) boundaries explicit.

The session is done when the **frontier** is empty: every requester-owned branch of the **design tree** visited, nothing user-owned left silently assumed, and the user confirms **shared understanding** before downstream action starts.
