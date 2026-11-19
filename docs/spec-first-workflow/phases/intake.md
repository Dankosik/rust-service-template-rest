# Intake

Use when new structured work lacks a confirmed [Intent](../interfaces/intent.md)
at `status: ready`, or outcome, scope, authority, observable success, or first
owner is unresolved.

Own framing the request and driving **shared understanding** before autonomous
preparation starts. Every downstream phase runs in a fresh session with zero
chat history, so accuracy here governs the entire pipeline.

## Steps

1. **Ground in source truth and map the design tree.**
   - Separate the requester's underlying problem from any proposed mechanism
     (**XY check**): test whether a literal reading would satisfy the prompt as
     worded while leaving the real problem unresolved.
   - Verify affected repository facts before asking the user: external contracts
     (`api/openapi/service.yaml`, `api/proto/`, config, CLI), current callers,
     [Repository Architecture](../../repo-architecture.md) (including `Domain
     Vocabulary`), and relevant `docs/decisions/`. Dispatch a fresh
     `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md) for read-heavy sweeps so
     this session stays in the **smart zone**.
   - **Early exits:** if primary sources contradict the prompt's premise, put
     that verified discrepancy at the root of the first grilling round; if every
     requested obligation already holds, present the verified evidence before
     interviewing; if the request is still a raw product bet without a chosen
     primary operator or MVP cutline, put operator selection, MVP boundary, and
     excluded directions at the root of the **design tree**.
   - Map unresolved branches across [Blind-Spot Coverage](#blind-spot-coverage)
     and classify every branch using [Grilling](../shared/grilling.md)'s four
     buckets (**facts**, **technical choices**, **low-stakes reversible
     conventions**, **requester-owned decisions**). Route deeper
     external-contract or baseline evidence gaps to [Research](research.md).
   - *Done when* obvious codebase facts are verified (or running in a fresh
     `evidence-agent`) and every branch is classified with zero technical
     mechanisms or lookupable facts on the user's **frontier**.

2. **Grill relentlessly until the frontier is empty.**
   - Run [Grilling](../shared/grilling.md) in **rounds** over the
     requester-owned **design tree**.
   - On multi-round interviews (3+ rounds), persist `specs/<task>/intent.md`
     incrementally at `status: draft` so compaction or interruption preserves
     every settled branch.
   - On non-trivial changes, once main behavior and scope settle, probe the
     sharpest surviving user-visible failure, replay/conflict, or compatibility
     edge cases before closing the frontier.
   - *Done when* the **frontier** is empty: every requester-owned branch is
     answered, repository-resolved, explicitly delegated with guiding limits, or
     converted to a labeled assumption (`A<n>`) under user steering.

3. **Synthesize `specs/<task>/intent.md` (`status: draft`) and run the
   Cold-Start Self-Check.**
   - Write `specs/<task>/intent.md` at `status: draft` using
     [Intent](../interfaces/intent.md) through [Artifacts](../shared/artifacts.md),
     recording every closed interview branch as an addressable `I<n>` entry
     (**Decision**, **Rejected**, **Why**) and preserving requester-supplied
     examples, edge-case scenarios, and established numeric thresholds verbatim.
   - Re-read **only** `intent.md` with zero chat history and close every gap in
     place:
     - **Cold-start completeness:** a fresh Definition session can act without
       guessing requester intent, re-asking closed branches, or resurrecting a
       `Rejected` option.
     - **Verbatim & de-mechanization fidelity:** user-pinned numbers, status
       codes, ordering rules, and `Must not` rules appear verbatim; casual
       mechanism hints sit under `Suggested mechanisms (non-binding context)`.
     - **Qualifier & boundary hygiene:** every vague qualifier (`fast`, `safe`,
       `graceful`, `reliable`) binds to a concrete policy, threshold, or
       delegated guardrail; internal design, crate placement, and task slicing
       stay out of `intent.md`.
   - *(Optional high-stakes check)* After a long interview or on high-risk scope
     (breaking contracts, security/auth, money, destructive migrations), you may
     dispatch one fresh read-only `reviewer-agent` on `draft` `intent.md` alone
     to flag cold-start ambiguities, contradictions, or unbound qualifiers
     before Gate 1.
   - *Done when* `intent.md` (`status: draft`) passes every check with zero
     chat-dependent meaning.

4. **Confirm shared understanding (Gate 1) and mark `ready`.**
   - Present a compact one-screen confirmation brief in the user's language with
     a link to `specs/<task>/intent.md`:
     - problem and target outcome;
     - settled interview decisions (`I1..In`: chosen, rejected, why);
     - hard constraints, `Must not` rules, and explicit `Non-goals`;
     - labeled assumptions (`A1..An` with `Reopen if`), delegated choices, and
       priority tie-breakers;
     - observable success signal.
   - Wait for explicit user confirmation before flipping `status: ready` and
     starting [Cross-Phase
     Continuation](../shared/transition.md#cross-phase-continuation) (for a
     clear brief with zero open questions, this reflection is the only round).
     Skip waiting only when the user explicitly instructed to proceed without
     confirmation and zero high-risk branches or material assumptions remain.
   - On user corrections, reconcile `intent.md` in place under
     [Artifacts](../shared/artifacts.md) and re-confirm if the delta is
     material.
   - *Done when* the user confirms **shared understanding**, `intent.md` is at
     `status: ready`, and the intent locator, first owner, and any
     routing-relevant assumptions or non-blocking open questions are ready.

5. **Handle mid-pipeline reopens.**
   - When reopened with `Blocked decisions` (`B1..Bn`)—from
     `specs/<task>/spec.md` or `specs/<task>/design/design.md` at
     `status: blocked`, or inline from a blocked handoff—translate its root
     frontier into the first [Grilling](../shared/grilling.md) round in plain
     user terms, holding back any `Follow-up if <option>` lines until the
     requester's answer takes that branch. Ask triggered follow-ups and any
     newly revealed requester uncertainty until the frontier closes. On any
     other reopen, grill strictly on what changed.
   - Reconcile settled answers into `specs/<task>/intent.md` in place (adding
     new `I<n>` entries, updating affected sections, removing superseded
     assumptions or open questions) and return control via [Cross-Phase
     Continuation](../shared/transition.md#cross-phase-continuation) to the
     earliest phase invalidated by the updated `intent.md` (reopening Definition
     when `spec.md` exists and observable behavior or baseline changed; resuming
     the blocked phase directly only when `spec.md` is unaffected or Definition
     was skipped).

## Blind-Spot Coverage

Consult only the branches this change reaches when building the **design tree**
in step 1:

- **Problem vs mechanism (XY):** concrete user or operator situation behind any
  proposed mechanism.
- **Actors & external surfaces:** affected users, operators, and callers
  (including legacy, retrying, concurrent, and unauthorized callers) and their
  external touchpoints.
- **Compatibility & blast radius:** existing consumers or persisted data found
  in source truth, breaking-change permission, and mixed-version overlap.
- **User-visible failure & precedence policy:** partial failure vs atomic
  rejection, duplicate/replay/conflict outcomes, auth vs existence (`401`/`403`
  vs `404`), and degradation behavior (strictly black-box).
- **Forbidden outcomes (`Must not`) & authority:** security, auth, PII/secrets,
  money, data loss, destructive or irreversible actions, external side effects,
  and cost or SLA ceilings.
- **Scope cutline & `Non-goals`:** plausible adjacent work explicitly excluded
  now.
- **Terminology & vague qualifiers:** overloaded terms (`cancel` vs `delete`,
  `accepted` vs `completed`) and unbound adjectives (`fast`, `safe`,
  `graceful`, `reliable`).
- **Trade-off compass (`Priority / tie-breakers`) & delegation bounds:** which
  outcome wins when downstream phases resolve unenumerated trade-offs.
