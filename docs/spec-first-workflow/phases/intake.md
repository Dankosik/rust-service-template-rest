# Intake

Align with the user on **what** we are building before autonomous preparation
starts. Keep technical mechanisms, crate layout, and implementation choices off
the user's frontier—the agent owns those autonomously once requirements settle.

## Steps

1. **Ground fast and map the design tree.**
   - Carry forward settled decisions from any prior `/grilling`, `/idea-refine`,
     or existing `specs/<task>/` artifact without re-asking them.
   - Run an **XY check**: separate the user's underlying problem from any
     casually suggested mechanism.
   - Inspect affected external contracts (`api/`, config, CLI), current callers,
     [Domain Vocabulary](../../repo-architecture.md#domain-vocabulary), and
     relevant `docs/decisions/` before Round 1. Offload wide scans to a fresh
     `evidence-agent` via [Read-Only
     Delegation](../shared/read-only-delegation.md) to protect the smart zone.
   - Adapt entry by situation:
     - *Premise conflict or already-satisfied request:* surface the verified
       code or `docs/decisions/` contradiction (with its `Why` / `Reopen if`) at
       the top of Round 1; if every requested outcome already holds, show the
       primary evidence before interviewing.
     - *Pre-authored spec or ticket:* verify claims against primary code and
       grill strictly on contradictions, blind spots, unbound qualifiers, and
       missing non-goals.
     - *Behavior-preserving change (perf / refactor / hardening):* grill on
       preserved guarantees, measurable target + workload envelope, and
       permitted blast radius.
     - *Small, local, reversible, single-owner change:* confirm understanding
       and execute directly under [Direct Work](../../../AGENTS.md#direct-work).
   - Map open branches across [Blind-Spot Coverage](#blind-spot-coverage) and
     classify each branch using [Grilling](../shared/grilling.md)'s four buckets
     (resolve facts yourself; keep technical choices internal; label safe
     conventions as assumptions; put only requester-owned decisions on the
     frontier).
   - *Done when:* local facts and `docs/decisions/` constraints are verified and
     the user's frontier holds strictly requester-owned decisions.

2. **Grill relentlessly until the frontier is empty.**
   - Work the design tree in paced plain-text rounds under
     [Grilling](../shared/grilling.md), probing the sharpest user-visible
     failure, conflict, and compatibility cases once core behavior settles.
   - Persist `specs/<task>/intent.md` (`status: draft`, listing settled
     decisions, scenarios, hard constraints, non-goals, labeled assumptions, and
     remaining questions) **only** if a multi-round interview must pause or
     compact before `spec.md` is written. Otherwise keep the live conversation
     as the primary source.
   - *Done when:* every requester-owned branch is answered, repo-resolved,
     explicitly delegated with limits, or converted to a labeled assumption with
     an objective `Reopen if`.

3. **Confirm shared understanding, then continue autonomously.**
   - Present a compact one-screen summary in the user's language:
     - problem and target outcome;
     - 1–2 concrete `Given -> When -> Then` scenarios (happy path + sharpest
       failure, conflict, or existing-data case);
     - settled choices (including rejected alternatives and *why*);
     - hard constraints, `Must not` rules, and the biggest `Non-goal`;
     - labeled assumptions (`Reopen if`), delegated boundaries, and priority
       tie-breakers;
     - observable success signal.
   - Wait for explicit user confirmation (skip waiting only if the user already
     instructed to proceed without confirmation and zero high-risk branches or
     material assumptions remain). If `intent.md` was persisted, flip it to
     `status: ready`.
   - Once confirmed, **continue autonomously in this session** into [Spec And
     Design](spec-and-design.md).
   - *Done when:* the user confirms shared understanding with zero unresolved
     product or risk forks.

## Blind-Spot Coverage

Consult only the branches this request reaches when building the design tree:

- **Problem vs mechanism (XY):** real user or operator situation behind any
  suggested mechanism.
- **Actors & surfaces:** affected users, operators, and callers (including
  legacy, retrying, concurrent, and unauthorized callers).
- **Consistency & durability:** immediate read-after-write vs bounded async lag.
- **Compatibility & existing state:** pre-existing persisted data and in-flight
  work at deploy time (`backfill`, `lazy upgrade`, `grandfather`, `reject`),
  breaking-change authority, mixed-version overlap, and optional profiles
  (`template/profiles.json`).
- **Failure & precedence policy:** partial failure vs atomic rejection,
  replay/conflict outcomes, auth vs existence (`401`/`403` vs `404`), and
  startup fail-fast vs runtime degradation.
- **Forbidden outcomes (`Must not`) & authority:** security, auth, PII/secrets,
  money, data loss, irreversible actions, external effects, rejected
  `docs/decisions/` choices, and workload/SLA ceilings.
- **Scope cutline & `Non-goals`:** plausible adjacent work explicitly excluded.
- **Terminology & qualifiers:** overloaded terms (`cancel` vs `delete`,
  `accepted` vs `completed`) and vague adjectives (`fast`, `safe`, `reliable`)
  bound to concrete policies or thresholds.
- **Trade-off compass:** priority order breaking ties when downstream phases
  make autonomous technical choices.
