# Intake

Align with the user on **what** we are building (and *why*) before autonomous
preparation starts. Keep technical mechanisms, crate layout, and implementation
choices off the user's frontier—the agent owns those autonomously once
requirements settle.

## Steps

1. **Ground fast, run the XY check, and adapt entry.**
   - Carry forward settled decisions, rejected alternatives, constraints, `A<n>`
     assumptions, and delegated boundaries from any prior `/grilling`,
     `/idea-refine`, or `specs/<task>/intent.md` without re-asking them.
   - **XY check (problem vs mechanism vs constraint):** when the requester
     proposes a mechanism (cache, table, queue, worker, crate, or wire shape),
     test whether literal compliance could leave the real user or operator
     problem unsolved. Lift suggested mechanisms into observable goals
     (freshness, latency, failure tolerance, scope) and check whether the named
     mechanism is a **hard external/business constraint** (for example, an
     external consumer already bound to that contract) or a **mechanism guess**
     owned by [Technical Design](technical-design.md).
   - **Ground repository anchors** under [Grilling](../shared/grilling.md)
     (opening with 1 problem-framing question first when the request is foggy,
     and offloading wide scans to `evidence-agent`): inspect affected external
     contracts (`api/`, config, CLI), current callers, [Domain
     Vocabulary](../../repo-architecture.md#domain-vocabulary), and relevant
     `docs/decisions/`. Load [Research](research.md) only when an unknown
     external contract or empirical fact gates a requester decision.
   - **Adapt entry by situation:**
     - *Premise conflict or already-satisfied request:* put the verified code
       or `docs/decisions/` contradiction (with its `Why` / `Reopen if`) at the
       top of Round 1. If every requested outcome already holds, show primary
       evidence first to test whether a different underlying problem remains;
       if the user confirms zero coding delta remains, verify via [Zero-Delta
       Check](plan-review.md#zero-delta-check) before closing without launching
       [Specification](specification.md).
     - *Raw product bet* (no primary operator or MVP cutline yet): put primary
       operator, core value bet / MVP cutline, and biggest exclusions at the
       root of Round 1.
     - *Pre-authored spec or ticket:* verify claims against live code and grill
       strictly on contradictions, blind spots, unbound qualifiers, and missing
       `Must not` / `Non-goals`.
     - *Behavior-preserving change* (perf / refactor / hardening): grill on
       preserved external guarantees, measurable target + workload envelope,
       and permitted blast radius rather than new user flows.
     - *Small, local, reversible, single-owner change:* confirm understanding
       and execute directly under [Direct Work](../../../AGENTS.md#direct-work).
   - Map open branches across [Product And Risk Blind
     Spots](#product-and-risk-blind-spots) and filter through
     [Grilling](../shared/grilling.md) so zero lookupable facts or technical
     choices enter the frontier.
   - *Done when:* XY framing separates problem from mechanism guesses, primary
     code and `docs/decisions/` anchors are verified (for any non-foggy entry),
     and the frontier holds strictly requester-owned product or risk forks.

2. **Grill the product tree and sharpen domain language.**
   - Work the requester-owned design tree in paced rounds under
     [Grilling](../shared/grilling.md).
   - **Active domain modeling:** challenge terms immediately when they conflict
     with live code or [Domain
     Vocabulary](../../repo-architecture.md#domain-vocabulary) or overload
     distinct concepts (`cancel` vs `void` vs `delete`, `account` vs `user` vs
     `tenant`). Tag each newly settled term as **task-local** (`spec.md`) or
     **cross-task** (to add to `Domain Vocabulary`).
   - **Probe sharp edge cases inside grilling:** on behavior-changing work,
     once core behavior settles, test at least the sharpest surviving
     user-visible failure, conflict/replay, authorization, or existing-data
     case with a concrete scenario before closing the frontier.
   - Persist `specs/<task>/intent.md` (`status: draft`, using [Optional
     `intent.md` Format](#optional-intentmd-format)) **only** if a multi-round
     interview must pause, hand off, or compact before `spec.md` is written.
   - *Done when:* the frontier is empty, vague qualifiers are bound to
     observable thresholds or explicit delegations, at least one sharpest
     unhappy or existing-data scenario has been tested on behavior-changing
     work, and remaining non-critical choices are tracked as labeled `A<n>`
     assumptions (with objective `Reopen if`) or delegated boundaries.

3. **Confirm shared understanding and cross into [Specification](specification.md).**
   - Present a compact one-screen summary in the user's language:
     - problem, primary actor/operator, and target outcome;
     - 1–2 concrete `Given -> When -> Then` scenarios (happy path + sharpest
       failure, conflict, or existing-data case);
     - settled product choices (including **rejected alternatives and *why***)
       and settled domain terms;
     - hard constraints, `Must not` prohibitions, and biggest `Non-goals`;
     - labeled `A<n>` assumptions (`Reopen if`), delegated boundaries (with
       limits), priority tie-breakers, and any residual risks;
     - observable success signal.
   - **Confirmation gate:** wait for explicit user confirmation. Skip waiting
     **only** when the user already explicitly instructed to proceed without
     confirmation **and** zero high-risk/authority forks, unshown `A<n>`
     assumptions, unconfirmed `Must not` rules, or newly introduced delegated
     boundaries remain—still emitting the compact summary above so the settled
     baseline is explicit in the transcript.
   - **Phase boundary (`Continue` vs `intent.md`):**
     - If `specs/<task>/intent.md` already exists on disk (`status: draft`),
       always update it in place with confirmed decisions, remove `Remaining
       frontier`, and flip to `status: ready` so resumed or reviewing contexts
       never read a stale draft.
     - *Inside the **smart zone** (default):* create no new `intent.md`, keep
       the live conversation as the **primary source**, and continue
       autonomously into [Specification](specification.md), where `spec.md`
       distills every durable rule and *why* for downstream fresh contexts.
     - *Pausing, handing off, or near the **smart zone** edge:* persist
       `specs/<task>/intent.md` (`status: ready`) using [Optional `intent.md`
       Format](#optional-intentmd-format), then compact or resume around
       `intent.md` before starting [Specification](specification.md).
   - *Done when:* either Intake exited cleanly via [Direct
     Work](../../../AGENTS.md#direct-work) or [Zero-Delta
     Check](plan-review.md#zero-delta-check), or shared understanding is
     confirmed with zero unresolved product/risk forks, no stale `draft`
     `intent.md` remains, and work enters [Specification](specification.md)
     inside the smart zone (or with `intent.md` at `status: ready`).

## Product And Risk Blind Spots

Consult only the branches this request reaches, and ask a branch **only** when
it is a genuine product or risk fork not already fixed by repo conventions
(`docs/decisions/`, [Production Contract](../../production-contract.md), or
sibling endpoints). Protocol edge cases, rollout mechanics, and optional
profile invariants belong autonomously to [Specification](specification.md) and
[Technical Design](technical-design.md).

- **Problem vs mechanism (XY):** real user/operator situation behind any
  suggested mechanism, and whether a named mechanism is a hard external
  constraint vs an implementation guess.
- **Actors & visibility:** affected users, operators, and callers (legacy,
  retrying, concurrent, unauthorized)—and whether unauthorized callers may
  learn that a resource exists.
- **Freshness & completion promise:** whether callers must see the effect
  immediately after the response or can tolerate bounded async lag.
- **Existing data & compatibility:** what happens to pre-existing records and
  in-flight work once this ships (keep old behavior, migrate existing data,
  reject, or upgrade on touch), and whether breaking callers or a maintenance
  window is permitted.
- **User-visible failure & conflict policy:** partial success vs all-or-nothing
  rejection, duplicate/retry outcomes, and whether a missing dependency stops
  the operation or degrades with a user-visible fallback.
- **Forbidden outcomes (`Must not`) & authority:** security, auth/tenant
  isolation, PII/secrets, money, data loss, irreversible actions, external
  effects, overturning rejected `docs/decisions/` choices, and SLA/cost
  ceilings.
- **Scope cutline & `Non-goals`:** plausible adjacent work explicitly excluded.
- **Terminology, qualifiers & tie-breakers:** overloaded domain terms, vague
  adjectives (`fast`, `safe`, `reliable`) bound to concrete thresholds, and
  priority order breaking ties for downstream autonomous choices.

## Optional `intent.md` Format

Persist `specs/<task>/intent.md` **only** on pause, handoff, or compaction
before `spec.md` is written:

- **Header:** `# Intent: <short outcome>` and `status: draft | ready`.
- **Problem & success signal:** problem, primary actor/operator, and observable
  success signal.
- **Settled product choices:** confirmed decisions with **rejected alternatives
  and *why***.
- **Concrete scenarios:** `Given -> When -> Then` (happy path + sharpest
  failure, conflict, or existing-data case).
- **Hard constraints, `Must not` & `Non-goals`:** external constraints,
  forbidden outcomes, and excluded scope.
- **Delegated boundaries & tie-breakers:** explicit user-authorized choice
  limits, priority order, and residual risks.
- **Labeled assumptions:** `A<n>` + objective `Reopen if` (`spec.md` preserves
  surviving IDs).
- **Domain terms:** settled definitions, tagging cross-task terms for [Domain
  Vocabulary](../../repo-architecture.md#domain-vocabulary).
- **Verified baseline locators:** inspected `api/`, code, callers, and
  `docs/decisions/` paths.
- **Remaining frontier:** *(include only while `status: draft`; remove at
  `status: ready`)*.

## Calibration Bar

- **XY lift vs mechanism leakage:**
  - *Strong:* *"You mentioned a Redis cache with 60s TTL — is up to 60s of
    stale reads acceptable to callers, and is Redis itself a hard external
    constraint? If only freshness and latency matter, I'll own the caching
    mechanism in Technical Design."*
  - *Weak:* *"What Redis key prefix, TTL jitter, and eviction policy should we
    use?"*
- **User product fork vs autonomous spec/tech detail:**
  - *Strong:* *"When this ships, should existing unpaid orders follow the new
    expiry rule immediately or keep the rule they were created under?"*
  - *Weak:* *"How should we handle mixed-version rollout overlap and inert
    `template/profiles.json` profiles?"*
- **Concrete edge probe vs abstract reassurance:**
  - *Strong:* *"Scenario check: Tenant B requests `GET /orders/<tenant-A-id>` —
    should they see 'not found' (hiding existence) or 'forbidden'?"*
  - *Weak:* *"We'll enforce tenant authorization and safe error handling."*
