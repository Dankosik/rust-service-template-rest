# Intake

Use when new structured work lacks a confirmed [Intent](../interfaces/intent.md)
at `status: ready`, or outcome, scope, authority, observable success, or first
owner is unresolved.

Own framing the request and driving **shared understanding** before autonomous
preparation starts. Every downstream phase runs in a fresh session with zero
chat history, so accuracy here governs the entire pipeline.

## Steps

1. **Ground in source truth and map the design tree.**
   - Check whether `specs/<task>/intent.md` (`draft` or `ready`) or prior
     `/grilling` / `/idea-refine` outputs already exist; carry settled branches
     forward without re-asking them.
   - Separate the requester's underlying problem from any proposed mechanism
     (**XY check**): test whether a literal reading would satisfy the prompt as
     worded while leaving the real problem unresolved.
   - Wait for fast local grounding before Round 1: inspect external contracts
     (`api/openapi/service.yaml`, `api/proto/`, config, CLI), current callers,
     [Repository Architecture](../../repo-architecture.md) (including `Domain
     Vocabulary`), and relevant `docs/decisions/`. Dispatch a fresh
     `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md) for read-heavy sweeps so
     this session stays in the **smart zone**.
   - **Entry modes & early exits:**
     - *Premise or decision conflict:* if primary sources contradict the
       prompt's premise or a request reverses a rejected choice in
       `docs/decisions/`, put that verified discrepancy (with the recorded `Why`
       / `Reopen if`) at the root of Round 1; if every requested obligation
       already holds, present the verified evidence before interviewing.
     - *Raw product bet:* when no primary operator or MVP cutline is chosen, put
       operator selection, MVP boundary, and excluded directions at the root of
       the **design tree**.
     - *Pre-authored spec or ticket:* extract already-settled decisions into
       draft `I<n>`, verify them against primary code, and grill strictly on the
       delta (code contradictions, unclosed blind spots, unbound qualifiers, and
       missing `Non-goals`).
     - *Behavior-preserving change (performance / refactor / hardening):* grill
       on preserved external invariants, quantitative target + workload
       envelope, and permitted blast radius / trade-offs rather than new user
       flows.
     - *Downgrade to Direct Work:* if grounding and any brief clarification
       prove the change is clear, local, reversible, single-owner, and free of
       open architectural forks, confirm understanding and execute under [Direct
       Work](../../../AGENTS.md#direct-work) instead of launching preparation
       phases.
   - Map unresolved branches across [Blind-Spot Coverage](#blind-spot-coverage)
     and classify every branch using [Grilling](../shared/grilling.md)'s four
     buckets (**facts**, **technical choices**, **low-stakes reversible
     conventions**, **requester-owned decisions**). Route deeper
     external-contract or baseline evidence gaps to [Research](research.md).
   - *Done when* fast local codebase facts and relevant `docs/decisions/`
     constraints are verified and every branch is classified with zero technical
     mechanisms or lookupable facts on the user's **frontier**.

2. **Grill relentlessly until the frontier is empty.**
   - Run [Grilling](../shared/grilling.md) in paced **rounds** over the
     requester-owned **design tree**.
   - On multi-round interviews (3+ rounds), persist `specs/<task>/intent.md`
     incrementally at `status: draft` (keeping unasked branches and verified
     grounding locators under a temporary `Remaining frontier` section) so
     compaction or interruption preserves both settled decisions and open
     questions.
   - On non-trivial changes, once main behavior and scope settle, probe the
     sharpest surviving user-visible failure, replay/conflict, or compatibility
     edge cases before closing the frontier.
   - *Done when* the **frontier** is empty: every requester-owned branch is
     answered, repository-resolved, explicitly delegated with guiding limits, or
     converted to a labeled assumption (`A<n>`) under user steering.

3. **Synthesize `specs/<task>/intent.md` (`status: draft`) and run verification
   checks.**
   - Write `specs/<task>/intent.md` at `status: draft` using
     [Intent](../interfaces/intent.md) through [Artifacts](../shared/artifacts.md),
     recording every closed interview branch as an addressable `I<n>` entry
     (**Binding**, **Decision**, **Rejected**, **Why**), adding 1–3 user-aligned
     `Concrete scenarios` on non-trivial changes, preserving requester-supplied
     examples, edge cases, and numeric thresholds verbatim, and removing any
     draft-only `Remaining frontier` section.
   - **Mechanical gate:** run `make intent-check TASK=<task>` (`python3
     scripts/check-intent.py specs/<task>`) and fix every error.
   - **Author Cold-Start Self-Check:** re-read **only** `intent.md` with zero
     chat history and close every gap in place:
     - **Cold-start completeness:** a fresh Definition session can act without
       guessing requester intent, re-asking closed branches, or resurrecting a
       `Rejected` option.
     - **Verbatim, binding & de-mechanization fidelity:** user-pinned numbers,
       status codes, ordering rules, `Concrete scenarios`, and `Must not` rules
       appear verbatim with accurate `Binding: hard | default`; casual mechanism
       hints sit under `Suggested mechanisms (non-binding context)`.
     - **Qualifier & boundary hygiene:** every vague qualifier (`fast`, `safe`,
       `graceful`, `reliable`) binds to a concrete policy, threshold, or
       delegated guardrail; `Open questions` carry zero unanswered
       requester-owned questions; internal design, crate placement, and task
       slicing stay out of `intent.md`.
   - **Triggered Cold-Start Intent Review (before Gate 1):** dispatch one fresh
     read-only `reviewer-agent` running [Cold-Start Intent
     Review](#cold-start-intent-review) under [Review](../shared/review.md)
     whenever **any** trigger holds: `≥3` grilling rounds, pre-authored external
     spec/ticket input, or high-risk scope (breaking external contract,
     auth/security/PII, money, destructive/irreversible migration, or `≥2`
     external actors/services). Pass strictly `specs/<task>/intent.md` and the
     adapter path (omit chat summaries or author rationale). Repair confirmed
     local defects in place, ask one surgical follow-up round if a real
     requester fork surfaces, and re-run `make intent-check TASK=<task>`.
   - *Done when* `intent.md` (`status: draft`) passes `make intent-check`,
     Cold-Start Self-Check, and any triggered Cold-Start Intent Review with zero
     chat-dependent meaning.

4. **Confirm shared understanding (Gate 1) and mark `ready`.**
   - Present a compact one-screen confirmation brief in the user's language with
     a link to `specs/<task>/intent.md`:
     - problem and target outcome;
     - **1–2 concrete `Given -> When -> Then` scenarios** (happy path plus the
       sharpest failure, conflict, or existing-data case) so misreadings surface
       concretely;
     - settled interview decisions (`I1..In`: `Binding`, chosen, rejected, why);
     - hard constraints, `Must not` rules, and the **biggest intentional
       `Non-goal`**;
     - labeled assumptions (`A1..An` with `Reopen if`, highlighting the single
       riskiest assumption), delegated choices, and priority tie-breakers;
     - observable success signal.
   - Wait for explicit user confirmation before flipping `status: ready`,
     re-running `make intent-check TASK=<task>`, and starting [Cross-Phase
     Continuation](../shared/transition.md#cross-phase-continuation) (for a
     clear brief with zero open questions, this reflection is the only round).
     Skip waiting only when the user explicitly instructed to proceed without
     confirmation and zero high-risk branches or material assumptions remain.
   - On user corrections, reconcile `intent.md` in place under
     [Artifacts](../shared/artifacts.md), re-run `make intent-check`, and
     re-confirm if the delta is material.
   - *Done when* the user confirms **shared understanding**, `intent.md` is at
     `status: ready` passing `make intent-check`, and the intent locator, first
     owner, and any routing-relevant assumptions or non-blocking open questions
     are ready.

5. **Handle mid-pipeline reopens.**
   - Flip `specs/<task>/intent.md` back to `status: draft` under [Artifact
     Lifecycle V1](../shared/artifacts.md#artifact-lifecycle-v1) before grilling
     so downstream phases cannot consume a stale `ready` artifact if
     interrupted.
   - When reopened with `Blocked decisions` (`B1..Bn`)—from
     `specs/<task>/spec.md` or `specs/<task>/design/design.md` at
     `status: blocked`, or inline from a blocked handoff—translate its root
     frontier into the first [Grilling](../shared/grilling.md) round in plain
     user terms, holding back any `Follow-up if <option>` lines until the
     requester's answer takes that branch. Ask triggered follow-ups and any
     newly revealed requester uncertainty until the frontier closes. On any
     other reopen, grill strictly on what changed.
   - Reconcile settled answers into `specs/<task>/intent.md` in place while
     preserving `I<n>` and `A<n>` ID stability (never renumber surviving IDs;
     mark overturned `I<k>` explicitly `Superseded by I<m>` or updated in
     place), run `make intent-check TASK=<task>` on the reconciled `draft` plus
     Cold-Start Self-Check (and [Cold-Start Intent
     Review](#cold-start-intent-review) if the delta hits a high-risk trigger),
     confirm material deltas with the user, flip back to `status: ready`,
     re-run `make intent-check TASK=<task>` to verify `ready` invariants, and
     return control via [Cross-Phase
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
- **Consistency & durability expectation (sync vs async):** whether a relied-on
  response must already reflect durable, immediately readable state
  (read-after-write) or may complete asynchronously with bounded lag.
- **Compatibility, existing state & blast radius:** existing consumers,
  **pre-existing persisted data and in-flight work at deploy time** (`backfill`,
  `lazy upgrade`, `grandfather`, or `reject`), breaking-change permission,
  mixed-version overlap, and affected optional profiles
  (`template/profiles.json`).
- **User-visible failure & precedence policy:** partial failure vs atomic
  rejection, duplicate/replay/conflict outcomes, auth vs existence (`401`/`403`
  vs `404`), and startup fail-fast vs runtime degradation (strictly black-box).
- **Forbidden outcomes (`Must not`), prior decisions & authority:** security,
  auth, PII/secrets, money, data loss, destructive or irreversible actions,
  external side effects, conflicts with rejected `docs/decisions/` choices, and
  workload/cost/SLA ceilings.
- **Scope cutline & `Non-goals`:** plausible adjacent work explicitly excluded
  now.
- **Terminology & vague qualifiers:** overloaded terms (`cancel` vs `delete`,
  `accepted` vs `completed`) and unbound adjectives (`fast`, `safe`,
  `graceful`, `reliable`).
- **Trade-off compass (`Priority / tie-breakers`) & delegation bounds:** which
  outcome wins when downstream phases resolve unenumerated trade-offs.

## Cold-Start Intent Review

Read-only review adapter for a fixed `specs/<task>/intent.md` (`status: draft`)
before Gate 1. Return [Review Result
V1](../interfaces/review-result-v1.md) without rewriting `intent.md` or
inventing requester decisions. Complete Checks 1–4 strictly from `intent.md`
with zero chat history before opening cited primary files for Check 5.

1. **Two-reader divergence (cold):** find any clause where two fresh Definition
   sessions reading only `intent.md` would choose materially different
   observable behavior, scope, or failure outcomes.
2. **Internal contradictions:** check for conflicts across `I<n>`, `Rejected`,
   `Concrete scenarios`, `Hard constraints & Must not`, `Scope & non-goals`,
   `A<n>`, and `Success signal`.
3. **Hidden requester forks & qualifiers:** flag unbound adjectives (`fast`,
   `safe`, `reliable`), unanswered requester questions in `Open questions`
   (missing non-requester `Owner`, `Fallback`, or `Reopen if`), or undelegated
   one-way product choices disguised as `A<n>` or `Binding: default`.
4. **Mechanism leak vs mandate:** flag internal design, crate placement, task
   slicing, or casual mechanisms recorded under `Hard constraints` instead of
   `Suggested mechanisms (non-binding context)`.
5. **Premise anchor check:** inspect repository locators cited in `intent.md`
   and relevant `docs/decisions/` entries; `FAIL` (`UPSTREAM_GAP` or
   `PHASE_DEFECT`) if a baseline premise contradicts primary code or overturns a
   rejected `docs/decisions/` choice without an explicit `I<n>`.

Return `PASS` when all five checks clear (recording `none — attacked <target>`
per check and inspected locators in `evidence_boundary`), `CONCERNS` only for
non-blocking external risks already paired with `Reopen if`, or `FAIL` with
concrete witness and smallest owner (`Intake`) for any surviving gap.
