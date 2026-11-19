# Grilling

Interview the requester **relentlessly** until you reach a **shared understanding**. Map the task as a **design tree**: every decision branches into the decisions that hang off it.

Classify each branch under [Decision Ownership](../../../AGENTS.md#decision-ownership) before asking:

- **Facts** (verifiable in repository code, contracts, `docs/decisions/`, or tools): resolve them yourself. Dispatch a fresh `evidence-agent` via [Read-Only Delegation](read-only-delegation.md) for read-heavy lookups so this session stays in the smart zone. Wait for fast local grounding before Round 1 so premise discrepancies and `docs/decisions/` conflicts surface at the root; overlap an in-flight lookup with user questions only for slow external research on strictly orthogonal branches.
- **Technical choices** (internals, crates, seams, or mechanisms with no user-visible behavior, cost, or risk difference): own them inside the agent system and keep them off the user's frontier.
- **Low-stakes reversible conventions** (outcome-preserving, reversible, idiomatic): record them as labeled assumptions with an objective `Reopen if` trigger for final confirmation rather than grilling on trivia.
- **Requester-owned decisions** (desired behavior, scope and non-goals, compatibility or breaking stance, user-visible failure policy, `Must not` prohibitions, money, auth, privacy, data-loss or irreversible effects, priority tie-breakers, or contradictions with primary code or rejected `docs/decisions/` choices): place them on the user's **frontier**.

Work the **design tree** in **rounds**. The **frontier** is every requester-owned decision whose prerequisites are settled—the questions you can ask now without guessing answers you have not heard yet. When the goal is foggy, open with one open question inviting a concrete situation and desired outcome in the requester's words. Otherwise:

- **Prioritize and pace:** order the frontier by pruning power and blast radius first (premise discrepancies, XY problem, MVP cutline, breaking compatibility, sync-vs-async durability, authority/irreversibility) before detail branches. Ask **3–5 highest-leverage questions per round**, grouped by theme; when more independent topics remain, add a one-line note naming the queued topics so the user sees progress without facing a wall of text.
- **Prefer plain-text rounds:** write rounds in plain chat text, numbered for fast replies (`1 yes, 2 B, 3 yes`), so the user can qualify answers or steer naturally. Use a native structured question widget only when the user or harness prefers it and it preserves full trade-off context and free-form input.
- **Prevent yes-bias:**
  - *Supported default* (strong codebase idiom, standard, or low-regret favorite): word the question so `"yes"` accepts the recommendation and name the trade-off given up.
  - *Balanced or high-stakes fork* (breaking change, authz/privacy, money, data loss/irreversibility, or genuinely competing product policies): do not fabricate a `"yes"` nudge—present 2–3 contrasting options (`A` / `B`) with concrete user-visible consequences and ask for an explicit choice.

```text
❓ **Q1** - **<supported-default title>**: <concise context anchor and concrete tradeoff in user terms>

➡️ <recommended answer, one-line reason, and what is given up if accepted>

---

❓ **Q2** - **<balanced/high-stakes title>**: <concise context anchor>
- **A:** <option and user-visible consequence>
- **B:** <option and user-visible consequence>

➡️ Balanced product choice — pick **A** or **B** (or specify another rule).
```

Wait for the user's answers before advancing dependent branches. Handle partial or ambiguous replies explicitly:

- **Skipped question:** silence in a partial reply is never `"yes"`; re-surface the skipped item or ask explicitly whether to convert it into an assumption (`A<n>`) or delegation.
- **Vague reply:** distinguish *explicit delegation* ("your call within X" → record in `Delegated decisions` with explicit bounds and echo the boundary in one sentence) from *unbound qualifiers* ("fast", "safe", "reliable" → offer concrete observable thresholds or policies and clarify before closing).
- **Contradiction with a settled branch:** stop and surface the conflict immediately (revise the earlier decision or narrow the new one) rather than silently overwriting or keeping both.

Each answered round reshapes the **design tree**: echo any non-obvious settled consequence in one sentence so a misreading surfaces immediately, prune invalidated sub-branches, recompute the **frontier**, and ask the next round. Keep any question that depends on an open question in this round for a later round.

Grilling has no fixed question cap; natural-language steering is the escape hatch. If the user asks to wrap up and assume the rest, convert remaining non-critical branches into labeled assumptions with `Reopen if` triggers while keeping hard [Authority](../../../AGENTS.md#authority) boundaries explicit.

The session is done when the **frontier** is empty: every requester-owned branch of the **design tree** visited, nothing user-owned left silently assumed, and the user confirms **shared understanding** before downstream action starts.
