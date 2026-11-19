# Specification

Use when structured work lacks a reviewed behavioral contract. Own observable
behavior: what affected users, callers, operators, and downstream systems can
rely on. **Observability litmus test:** if an external observer (client,
operator, downstream consumer, or black-box check) can tell two variants apart
without reading internals, Definition owns it; otherwise leave internal design,
slicing, and test authoring to downstream phases.

## Steps

1. **Ground in source truth (and existing `spec.md` on reopen).** Read
   `specs/<task>/intent.md`, any existing `specs/<task>/spec.md` (on reopen or
   resume from `blocked`, preserve unaffected `R<n>`/`G<n>` IDs and reconcile
   only invalidated clauses under [Artifacts](../shared/artifacts.md)), prior
   `research/*.md`, [Domain
   Vocabulary](../../repo-architecture.md#domain-vocabulary), and primary
   sources: committed contracts (`api/`), code, migrations, config, tests,
   `docs/decisions/`, and [Production Contract](../../production-contract.md)
   when service-wide promises apply. Verify every `intent.md` and Markdown
   baseline claim against actual code across the full **blast radius**
   (HTTP/gRPC contracts, workers/jobs/outbox/webhooks, CLI/config boot paths,
   diagnostics/readiness/metrics, optional profiles, and **pre-existing
   persisted or in-flight state**).
   - Run supporting [Research](research.md) when an external contract,
     standard, or deep baseline question can change a behavioral rule. Keep
     [Solution Discovery](research-branches.md#solution-discovery-evidence) in
     Technical Design unless feasibility changes observable behavior.
   - **Early exits:**
     - *Already satisfied (zero work anywhere):* primary sources prove every
       functional and non-functional obligation in `intent.md` already holds ->
       skip drafting and submit that primary evidence to step 5.
     - *Behavior-preserving change:* functional behavior stays unchanged while
       an operational, quantitative, or internal technical goal remains ->
       continue to step 2, placing any quantitative/operational targets in
       `Behavior delta` (or `None — behavior-preserving change` when only
       `G<n>` and a `Downstream handoff` apply).
     - *Wrong premise:* `intent.md` rests on a verified false premise about
       current behavior -> if `specs/<task>/spec.md` already exists (reopen),
       flip it to `status: blocked` and return `result: specs/<task>/spec.md`;
       otherwise return the primary discrepancy inline without creating a stub
       file (`status: blocked`, `reopen_owner: Intake`).
   - *Done when* every touched surface, caller, and persisted/in-flight state
     in the blast radius and every `intent.md` baseline claim is verified in
     primary sources, with no behavioral evidence gap left.

2. **Pin vocabulary, probe behavioral seams, and resolve every fork.** Adopt
   canonical nouns from [Domain
   Vocabulary](../../repo-architecture.md#domain-vocabulary), code, `api/`, and
   `docs/decisions/`; pin any new or overloaded term (`cancel` vs `delete`,
   `accepted` vs `completed`) before deciding rules, flagging cross-task terms
   or changed service-wide promises for `Durable contract update`.
   - When an open **observable** behavioral or operational decision matches a
     repository skill's trigger (`.agents/skills/rust-*`), load only that skill
     **strictly for external contract, failure, security, operational, or
     quantitative constraints**—ignoring internal Rust types, crate wiring, and
     test commands.
   - Probe every touched surface across [Behavioral Seams](#behavioral-seams)
     *before* drafting so edge-case forks surface here instead of mid-prose.
   - Disposition every material `intent.md` item (including `Open questions`
     and delegated limits) and every surfaced fork:

   | Resolution source | Action |
   | --- | --- |
   | Primary authority (contract, standard, `docs/decisions/`, verified guarantee) | Adopt it and cite the primary locator. |
   | Delegated by [Intent](../interfaces/intent.md) or [Decision Ownership](../../../AGENTS.md#decision-ownership) | Decide within stated limits (or state the permitted invariant), recording strongest rejected alternative, reason, and objective `Reopen if`. |
   | Downstream-only item (non-behavioral preference, internal refactor goal, or technical open question) | Record as `Downstream handoff` naming `Technical Design` or `Planning`. |
   | Bounded assumption | Use *only* when requester intent or repo convention strongly selects one obvious choice, reversal is cheap, and it alters no policy, compatibility, security/privacy posture, or external commitment; add an objective `Reopen if`. |
   | Requester- or external-policy-owned fork | Whenever two reasonable product owners could choose differently and it alters outcome, policy, unauthorized breaking compatibility, security/privacy posture, or money/legal/external effects (or requires an external policy authority): block via the protocol below. |

   - **Blocked frontier protocol:** finish all independent branches to collect
     the full **independent frontier** in one pass (holding back questions
     dependent on an unresolved root), frame options in requester behavior and
     consequences (never internal mechanisms), save resolved rules and `Blocked
     decisions` in `spec.md` at `status: blocked`, and return [Transition
     Result V1](../shared/transition.md#transition-result-v1) (`status:
     blocked`, `result: specs/<task>/spec.md`, `reopen_owner: Intake` or the
     named external policy owner) without review.
   - *Done when* every `intent.md` item and behavioral-seam fork maps to
     `R<n>`, `G<n>`, `Non-goals`, `Downstream handoff`, or `Blocked decisions`,
     with zero unresolved forks left for drafting.

3. **Render draft `spec.md`.** Write (or update on reopen)
   `specs/<task>/spec.md` at `status: draft` per [Specification
   V1](../interfaces/spec-v1.md) and [Artifacts](../shared/artifacts.md),
   recording decisions already closed in step 2 and removing any resolved
   `Blocked decisions` section before review.
   - *Done when* `spec.md` conforms to Specification V1 (zero residual `Blocked
     decisions` and zero new policy invented mid-draft).

4. **Run the Author Pre-Flight (before review).** Attack `draft` `spec.md` on
   checks an author can genuinely falsify in-context, closing gaps in place:
   - **Intent trace:** verify every material line of `intent.md` (`Success
     signal`, delegated limits, `Open questions`) maps cleanly into `spec.md`.
   - **Adversarial attack:** construct a literal implementation satisfying
     every `R<n>` and `G<n>` that still breaks an observable outcome or
     `Success signal` in `intent.md` (*malicious compliance*, excluding
     non-behavioral technical goals routed to `Downstream handoff`), and trace
     a concrete multi-rule interaction or unhappy-path scenario across
     [Behavioral Seams](#behavioral-seams).
   - **Deletion test:** for each inferred clause, ask *"If Technical Design met
     the external outcome another way without this clause, what accepted
     guarantee would break?"* Delete or relax to the observable invariant when
     none breaks.
   - **Falsifier check:** confirm every `R<n>` `Falsifier` meets [Specification
     V1](../interfaces/spec-v1.md#field-rules).
   - *Done when* every exposed loophole, mechanism leak, or vague `Falsifier`
     is closed (or any newly surfaced requester- or policy-owned fork moves
     `spec.md` to `status: blocked` via step 2's protocol).

5. **Submit for independent review.** Route the fixed `draft` `spec.md` (or
   already-satisfied no-delta evidence) via [Review
   Topology](specification-review.md#review-topology) and
   [Review](../shared/review.md). On `FAIL`:
   - *No-delta candidate:* enter step 2 (and submit the resulting `spec.md` to
     a fresh review under [Review](../shared/review.md#lifecycle)) when
     `reopen_owner: Specification`; return inline `status: blocked`
     (`reopen_owner: Intake`) with the primary discrepancy—without creating a
     stub `spec.md`—when `reopen_owner: Intake`.
   - *Draft `spec.md` — spec-local findings only:* repair in place, rerun
     affected step 4 checks, and re-submit under
     [Review](../shared/review.md#lifecycle).
   - *Draft `spec.md` — upstream gap (`reopen_owner: Intake` or external policy
     owner):* repair independent spec-local defects first so clean work
     survives, record the unresolved frontier in `Blocked decisions` (`status:
     blocked`), and return [Transition Result
     V1](../shared/transition.md#transition-result-v1) (`status: blocked`,
     `result: specs/<task>/spec.md`, `reopen_owner: <owner>`) without another
     review pass.
   - *Done when* independent review returns `PASS` or `CONCERNS` on the current
     candidate (or a `blocked` handoff is returned).

6. **Finalize and hand off.** After review returns `PASS` or `CONCERNS`, return
   [Transition Result V1](../shared/transition.md#transition-result-v1)
   (`status: ready`, `owner: Definition`) and stop:
   - **Already-satisfied no-delta result:** `result: <verified primary no-delta
     evidence>`, `next_owner: none`, without persisting `spec.md`.
   - **Specification result:** persist bounded `CONCERNS` (risk, downstream
     owner, closing action) into `Carried concerns`, flip `status: ready` in
     `spec.md`, and set `result: specs/<task>/spec.md` with `next_owner:
     Technical Design` whenever [Phase
     Selection](../../spec-first-workflow.md#phase-selection) triggers
     Technical Design or a `Downstream handoff` names `Technical Design` (or
     `Planning` when neither applies).

## Behavioral Seams

Single source of truth for the five high-divergence axes probed in step 2, step
4, and [Specification Review](specification-review.md):

1. **State & boundaries:** valid, invalid, and repeated transitions; absence,
   `null`, and defaults; identifiers, units, bounds, clocks, and expiry.
2. **Concurrency & precedence:** simultaneous conditions (auth + invalid input,
   replay + expiry, env + file config), concurrent mutations on the same
   entity, ordering, and read-after-write or eventual visibility.
3. **Failure, replay & promise boundaries:** error observables (auth-vs-not-found
   exposure, secret/PII non-exposure), idempotency, replay, and partial
   failure; on any response, status, event, or ack an actor acts on, close both
   *false-positive success* (success visible before the promised effect holds)
   and *unobservable completion* (effect lands while the actor cannot tell).
4. **Blast radius & existing state:** current vs target outcomes per consumer
   scenario, separating verified guarantees from incidental details; treatment
   of **pre-existing persisted or in-flight state** (backfill, lazy upgrade,
   grandfathering, or rejection); for any changed guarantee, state
   authorization, former-caller outcome, and mixed-version overlap expectation.
5. **Operational & quantitative observables** *(when triggered)*: startup
   rejection vs runtime degradation, config precedence, requested
   operator/audit signals, and labeled numeric targets with workload envelope.
