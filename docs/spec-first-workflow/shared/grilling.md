# Grilling

Interview the requester **relentlessly** until you reach a **shared
understanding**. Model the problem as a **design tree**: every decision
branches into the sub-decisions that hang off it.

## Classify Every Branch First

Place each open branch in one bucket under [Decision
Ownership](../../../AGENTS.md#decision-ownership) before asking:

- **Facts** *(verifiable in code, contracts, docs, or tools)*: resolve them
  yourself from primary sources—offload read-heavy scans to a fresh
  `evidence-agent` via [Read-Only Delegation](read-only-delegation.md) so this
  session stays in the **smart zone**. Once the target area is clear, ground
  fast local facts before Round 1; overlap background lookups with user
  questions only for slow external research on strictly orthogonal branches.
- **Technical choices outside the scope being grilled** *(internals, crates,
  seams, or mechanisms with no user-visible behavior, cost, or risk
  difference)*: own them autonomously and keep them off the user's **frontier**.
- **Low-stakes reversible conventions** *(outcome-preserving, reversible, backed
  by a clear local idiom or standard)*: skip asking trivia; track them as
  labeled assumptions (`A<n>`) with an objective `Reopen if` trigger to surface
  at confirmation.
- **Requester-owned decisions** *(desired behavior, scope and non-goals,
  compatibility or breaking stance, user-visible failure policy, `Must not`
  prohibitions, money, auth, privacy, data-loss or irreversible effects,
  external effects, priority tie-breakers, or contradictions with primary
  sources)*: place them on the user's **frontier**.

## Work The Frontier In Rounds

The **frontier** is every requester-owned decision whose prerequisites are
settled—questions you can ask now without guessing unheard answers. Hold
dependent questions for later rounds.

- **Foggy start:** if the request is too vague to identify the concrete problem
  or touched surface, open with **1 open question** inviting a real situation
  and desired outcome in the requester's words before scanning the repository.
- **Pace by pruning power:** order by blast radius first (premise
  contradictions, XY problem, MVP cutline, breaking compatibility, immediate vs
  delayed visibility, authority/irreversibility). Ask **1–5 independent
  frontier questions per round**, grouped by theme—often just **1–2** when a
  root fork prunes downstream branches; never pad with dependent questions or
  trivia. When more than 5 independent topics remain, add a one-line note
  naming the queued topics.
- **Plain-text rounds by default:** write numbered plain-text rounds for fast
  replies (`1 yes, 2 B, 3 yes`) so the user can qualify or steer naturally. Use
  a native structured question widget only when the user asks for one and it
  preserves full trade-off context and free-form input.
- **Prevent yes-bias:**
  - *Supported default* (a meaningful user-visible policy where primary code or
    domain context gives one clear favorite): word so `"yes"` accepts the
    recommendation and name the concrete trade-off given up.
  - *Balanced or high-stakes fork* (breaking change, authz/privacy, money, data
    loss/irreversibility, external effects, reversing a recorded decision, or
    genuinely competing product policies): present 2–3 contrasting options
    (`A` / `B`) with user-visible consequences and ask for an explicit choice.
- **Probe with concrete scenarios:** when a rule, boundary, or term stays
  abstract, test it with a concrete edge-case scenario (`If X happens while Y
  is in flight, should the caller see A or B?`).

```text
❓ **Q1** - **<supported-default title>**: <concise anchor and tradeoff in user terms>

➡️ <recommended answer, one-line reason, and what is given up if accepted>

---

❓ **Q2** - **<balanced/high-stakes title>**: <concise anchor>
- **A:** <option and user-visible consequence>
- **B:** <option and user-visible consequence>

➡️ Product/risk fork — pick **A** or **B** (or specify another rule).
```

Wait for the user's reply before advancing dependent branches. Between rounds:

- **Verify new factual claims:** when a reply asserts how existing code works
  or opens a new surface, check primary sources before the next round and
  surface any contradiction immediately.
- **Skipped question:** silence in a partial reply is never `"yes"`; re-surface
  it or ask explicitly whether to convert it into an assumption (`A<n>`) or
  delegation.
- **Vague reply:** separate *explicit delegation* ("your call within X" → record
  a delegated boundary with explicit limits and echo it in one sentence) from
  *unbound qualifiers* ("fast", "safe", "reliable" → offer concrete observable
  thresholds or policies before closing).
- **Contradiction with a settled branch:** surface the conflict immediately
  (revise the earlier choice or narrow the new one) rather than silently
  overwriting or keeping both.
- **Reshape the tree:** echo any non-obvious settled consequence in one
  sentence, prune invalidated sub-branches, recompute the **frontier**, and ask
  the next round.

## Closing And Escape Hatch

Grilling has no fixed question cap; natural-language steering is the escape
hatch. If the user asks to wrap up and decide the rest yourself, convert
remaining non-critical branches into labeled assumptions (`A<n>` with objective
`Reopen if`) for confirmation while keeping unresolved hard
[Authority](../../../AGENTS.md#authority) boundaries explicit (breaking
changes, auth/privacy, money, destructive/irreversible actions, or external
effects).

- **When invoked inside a workflow phase or skill** (such as
  [Intake](../phases/intake.md) or `/idea-refine`): closing the **frontier**
  hands off to that caller's own summary format and confirmation gate.
- **When invoked directly** (standalone `/grilling` or
  [Direct Work](../../../AGENTS.md#direct-work)):
  - On multi-branch work or standalone `/grilling`, present a compact summary
    (settled choices with rejected alternatives and *why*, labeled `A<n>` with
    `Reopen if`, delegated boundaries, and residual risks) and wait for
    explicit confirmation before acting.
  - On a short `Direct Work` clarification (or when the user already authorized
    proceeding once answered), if zero unshown `A<n>` assumptions, zero
    unresolved hard `Authority` forks, and zero new delegated boundaries
    remain, echo the settled rule in one line and proceed directly.

## Calibration Bar

- **Neutral fork vs fabricated yes-bias:**
  - *Strong:* *"❓ **Q2 — Cancelling a paid order**: Customer cancels after
    payment succeeds. **A:** Automatically issues a full refund. **B:** Rejects
    cancel once paid; refunds require an explicit refund action. ➡️
    Product/money fork — pick **A** or **B**."*
  - *Weak:* *"❓ Should cancelling a paid order automatically refund the
    customer? ➡️ Yes, standard UX."*
- **Labeled assumption vs trivia or silent assumption:**
  - *Strong:* Adopt existing list-endpoint default page size (`50`) off the
    frontier, record it as `A1` with `Reopen if`, and show `A1` at confirmation.
  - *Weak:* Spending a round slot on *"Should default page size be 20, 50, or
    100?"*—or picking `50` silently without listing `A1` at confirmation.
- **Concrete scenario vs abstract question:**
  - *Strong:* *"If a client retries `POST /orders` with the same key after a
    timeout, should it get the original order back or an error?"*
  - *Weak:* *"How should idempotency and error handling work for orders?"*
