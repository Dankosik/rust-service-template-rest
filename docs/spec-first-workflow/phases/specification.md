# Specification

Continue autonomously in the same session after [Intake](intake.md) confirms
shared understanding. Own the externally observable behavioral contract
(**WHAT**); leave technical mechanisms, storage or migration tactics, crates,
and code structure to [Technical Design](technical-design.md).

**External Observer Test:** If two implementations differ here, could an
external caller, consumer, operator, or auditor tell them apart without reading
source code or internal storage? If yes, pin the rule in `spec.md`. If no, leave
it to Technical Design—except explicit **hard external constraints** carried
from Intake that legitimately bind HOW (for example, an externally mandated
provider or datastore, compliance/data-residency rule, or infrastructure cost
ceiling).

## Steps

1. **Ground external-seam delta and inventory affected surfaces (read-only).**
   - Build on Intake's confirmed baseline (re-verifying baseline claims against
     live code when resuming from `specs/<task>/intent.md`) without re-asking
     settled or delegated choices. Inspect the deeper live external-contract
     facts Intake did not need to pin: exact `api/` schemas, existing statuses
     and wire error codes, field nullability and defaults, CLI flags/exit codes,
     sibling endpoint conventions, operator-facing `crates/config` keys, [Domain
     Vocabulary](../../repo-architecture.md#domain-vocabulary),
     `docs/decisions/`, and existing persisted entity shapes that govern
     pre-existing data at deploy. Offload wide scans to a fresh `evidence-agent`
     via [Read-Only Delegation](../shared/read-only-delegation.md) to stay in
     the **smart zone**; load [Research](research.md) whenever an unknown
     external standard, provider contract, or consumer behavior gates an `R<n>`.
   - Before drafting any `R<n>`, map the affected behavioral surface required by
     `## Actors, baseline & terminology` in [Artifact Format](#artifact-format).
   - Surface any verified premise contradiction directly to the user instead of
     authoring around it. If every requested outcome already holds with zero
     coding delta, verify via [Zero-Delta Check](plan-review.md#zero-delta-check)
     before reporting completion.
   - *Done when:* the full affected behavioral surface for `## Actors, baseline
     & terminology` is verified in primary sources.

2. **Close observable behavior (WHAT) across Behavioral Probes.**
   - Walk all seven probes in [Behavioral
     Probes](specification/behavioral-probes.md) across every inventoried entity
     and external seam.
   - **Autonomous closure vs. user reopen:**
     - *Close autonomously:* protocol and boundary semantics (`omitted`/`null`/
       empty, exact expiry, sorting/tie-breakers, error codes, failure
       precedence) consistent with `docs/decisions/`, [Production
       Contract](../../production-contract.md), and sibling seams; choices
       inside user-delegated boundaries (within stated limits); and low-stakes
       reversible edge conventions (pinning the concrete rule in `R<n>` and
       tracking its `A<n>` assumption with objective `Reopen if`).
     - *Reopen the user strictly for:* a verified premise contradiction or an
       unresolved product/risk fork (money, auth/existence hiding, externally
       observed fate of pre-existing data at deploy, breaking callers, or a
       conflict with `Must not`).
   - Shape every closed rule, interaction table, pinned external identifier,
     public-seam `Falsifier`, risk-anchored `G<n>`, and quantitative target
     under [Artifact Format](#artifact-format).
   - *Done when:* every inventoried entity and external seam clears [Behavioral
     Probes](specification/behavioral-probes.md)'s exhaustiveness bar, with zero
     unresolved product/risk forks.

3. **Persist `specs/<task>/spec.md` (`status: draft`).**
   - Write `spec.md` at `status: draft` using [Artifact Format](#artifact-format),
     omitting empty optional sections and linking unchanged code, `api/`,
     migrations, and `research/*.md` instead of copying them.
   - *Done when:* `specs/<task>/spec.md` is on disk at `status: draft` with
     complete surface inventory, closed behavioral rules, and zero unresolved
     product/risk forks.

4. **Falsify on disk and enter [Technical Design](technical-design.md).**
   - Attack the persisted `spec.md` as a hostile cold-start reader and tighten
     every surviving defect in place before crossing into Technical Design:
     - **Two-Implementation Divergence Test:** walk the 2–3 hardest edge,
       concurrent, replay, or pre-existing-data paths reading only `spec.md`
       (zero chat history); tighten any clause where two independent engineers
       could ship externally different statuses, wire codes, ordering, side
       effects, or state transitions.
     - **Malicious Compliance Attack:** try to satisfy every `R<n>` and
       `Falsifier` literally while defeating the observable success signal or a
       `Must not` prohibition; strengthen any weak clause or happy-path-only
       falsifier.
     - **HOW-Leak Scrub:** lift any embedded external requirement into an
       observable WHAT rule (or verify it is a recorded Intake hard external
       constraint), then remove any prescribed internal mechanism, storage or
       cache layout, migration tactic (`backfill` vs `lazy upgrade`), crate or
       module split, or internal-seam falsifier.
     - **Cold-Start Self-Sufficiency Check:** verify `spec.md` alone carries
       Intake's non-obvious *why*, rejected product alternatives, hard external
       constraints, `Must not` separated from `Non-goals`, delegated boundaries
       (with limits), settled domain terms (`Means here` / `Does not mean` +
       cross-task tags), and sharpest concrete edge scenarios (consumed directly
       by [Design It Twice](technical-design/design-it-twice.md) and
       [Implementation Review](implementation-review.md)).
   - Continue directly into [Technical Design](technical-design.md); compact at
     this phase boundary around `spec.md` only if heavy baseline/research reads
     pushed context near the **smart zone** edge.
   - *Done when:* on-disk `specs/<task>/spec.md` clears all four attacks with
     zero surviving external divergences, malicious-compliance loopholes,
     ungrounded HOW leaks, or dropped Intake rationales, and work enters
     Technical Design inside the smart zone.

## Artifact Format

Keep durable IDs (`R<n>`, `G<n>`, `A<n>`) stable on reopen: preserve surviving
`A<n>` IDs from Intake / `intent.md`, and allocate newly added spec assumptions
past the highest existing `A<n>` across `spec.md` and `design.md`.

- **Header:** `# Spec: <short observable outcome>` and `status: draft | ready`
  *(stays `draft` until cleared by [Design Review](design-review.md) on
  substantive architecture, or [Plan Review](plan-review.md) on lightweight
  inline architecture)*.
- **`## Context & success signal`:** problem, primary actor/operator, observable
  success signal, priority tie-breakers, **hard external constraints** (external
  contracts or ceilings that legitimately bind HOW), settled product choices
  (**rejected alternatives and *why***), and **concrete scenarios**
  (`Given -> When -> Then` covering the happy path plus the sharpest failure,
  concurrency/replay, and pre-existing-data cases).
- **`## Actors, baseline & terminology`:**
  - Touched domain entities and lifecycles;
  - Actors and trust boundaries → external seams with primary locators;
  - Existing callers, consumers, and pre-existing or in-flight entity classes
    (or verified absence);
  - Verified baseline delta (linking existing `api/`, code, migrations, and
    `research/*.md`);
  - **Terminology** *(omit when no domain terms need disambiguation)*:
    domain-specific terms only (no generic programming words, no implementation
    details), using `| Term | Means here | Does not mean | Scope |` with
    `Scope` = `task-local` or `cross-task` (cross-task rows feed `Durable doc
    updates` → [Domain Vocabulary](../../repo-architecture.md#domain-vocabulary)).
- **`## Behavior delta`:** `### R<n> — <coherent observable capability>` with:
  - atomic trigger → observable-outcome clauses, using a compact **State ×
    Event**, **Input Boundary**, or **Precedence** table whenever `≥2` states,
    input variants, or competing failure conditions interact;
  - pinned **externally observable identifiers** (routes/RPCs, statuses, wire
    error codes, payload fields, CLI flags/exit codes, operator-contract
    config/metric/audit names);
  - for measurable performance, capacity, or cost goals: labeled numeric target,
    workload envelope, comparable baseline, and measurement boundary;
  - a public-seam **Falsifier** attacking the sharpest boundary, race, or
    precedence loophole.
  *(Use `Behavior delta: None — behavior-preserving change` strictly when
  neither external behavior nor a quantitative target changes.)*
- **`## Preserved guarantees`:** `G<n>` — concrete unchanged observable
  invariants **this change can realistically disturb**, with primary locators.
- **`## Must not & Non-goals`:**
  - **`Must not`** — hard safety, isolation, data-integrity, or compatibility
    prohibitions (violations are defects/`FAIL`, traced across Design, Plan, and
    Implementation Reviews).
  - **`Non-goals`** — plausible adjacent scope explicitly excluded (prevents
    scope creep).
- **`## Assumptions & reopen triggers`:** `A<n>` bounded assumptions + objective
  `Reopen if`, explicitly **delegated boundaries** (with user-stated limits),
  and any carried residual risks.

*(On [lightweight architecture](technical-design.md#design-review-gate),
Technical Design appends a compact inline `## Architecture` section directly to
`spec.md`.)*

## Calibration Bar

- **Crisp WHAT vs HOW-leak vs vague WHAT:**
  - *Strong:* *"Replaying `POST /orders` with the same `Idempotency-Key` and
    identical body bytes within 24h returns the original `201` order without
    creating a second order; replaying with a modified body returns `422
    idempotency_key_mismatch`; a concurrent in-flight duplicate returns `409
    idempotency_request_in_progress`; replaying at or after 24h
    (`now >= expires_at`) treats the request as new."*
  - *Weak (HOW-leak):* *"Add `UNIQUE (tenant_id, idempotency_key)` on `orders`
    and cache the JSON receipt in Redis with a 24h TTL."*
  - *Weak (under-specified):* *"Handle duplicate `POST /orders` requests
    idempotently and return an appropriate error on conflict."*
- **Interaction table vs prose essay:**
  - *Strong:* Compact Precedence table (`1. Unauthenticated -> 401
    authentication_required`, `2. Tenant mismatch -> 404 not_found`, `3. Invalid
    body -> 422 unprocessable_content`, `4. Modified replay -> 422
    idempotency_key_mismatch`) + explicit `now >= expires_at` rule.
  - *Weak:* A dense paragraph stating *"Unauthorized, invalid, expired, or
    conflicting requests are rejected with the matching error"* without
    resolving overlaps or `==`.
- **Sharp public-seam Falsifier vs tautology / happy-path-only / internal-seam Falsifier:**
  - *Strong:* *"Concurrent `POST /orders/{id}/pay` and `POST /orders/{id}/cancel`
    both return `200`, or replaying an `Idempotency-Key` with a modified body
    returns `200` instead of `422 idempotency_key_mismatch`."*
  - *Weak:* Repeating `R<n>` verbatim, testing only happy-path failure (`POST
    /orders` returns `500`), checking internals (*"`OrderRepo::insert` is called
    twice"*), or naming a unit test / `just` command (tests belong to
    Implementation per [Evidence
    Contract](../shared/evidence-contract.md#design-proof)).
- **Risk-anchored `G<n>` and `Must not` vs `Non-goals` separation:**
  - *Strong:* `G1` pins a specific at-risk sibling contract (`GET /orders`
    default descending `created_at, id` order and cursor stability, anchored to
    `api/openapi/service.yaml` and `crates/orders/src/http.rs`); `Must not`
    forbids revealing cross-tenant order existence (`404`, never `403`);
    `Non-goals` excludes CSV export and admin replay UI.
  - *Weak:* *"G1: Existing API endpoints keep working"* (unanchored boilerplate)
    or listing *"Must not leak tenant data"* under `Non-goals`.
