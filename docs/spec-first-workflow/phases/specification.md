# Specification

Use when structured work lacks a reviewed behavioral contract. Own observable
behavior: what external observers (users, callers, operators, downstream
systems) can rely on.

**Observability boundary:** Fix every externally observable rule, invariant,
signal requirement, failure/compatibility contract, and any exact identifier
already fixed by `intent.md`, `api/`, existing config/code, or an external
standard (or any new client wire identifier that external callers require fixed
here). Leave unfixed new identifier spellings (such as new config keys,
metric/span names, table/column names, or internal/unfixed error slugs—marked
`Open to Technical Design:` on the owning rule), internal mechanisms, slicing,
and test authoring to downstream phases.

**Non-standard paths:** For reopen/resume from `blocked`, already-satisfied
`no-delta`, a false premise in `intent.md`, a behavior-preserving change, or a
blocked fork, load [Non-Standard Specification
Paths](specification/non-standard-paths.md).

## Steps

1. **Ground in primary sources (static — no builds, tests, or services).** Read
   `specs/<task>/intent.md`, prior `research/*.md`, [Domain
   Vocabulary](../../repo-architecture.md#domain-vocabulary), and primary
   sources: `api/`, code, migrations, config, `scripts/template/profiles.json`,
   tests, `docs/decisions/`, and [Production
   Contract](../../production-contract.md) when service-wide promises apply.
   Trace live callers, consumers, and pre-existing persisted/in-flight state
   across the **blast radius** (HTTP/gRPC, workers/jobs/outbox/webhooks,
   CLI/config boot, readiness/diagnostics, optional profiles).
   - Offload wide multi-crate sweeps to a fresh `evidence-agent` under
     [Read-Only Delegation](../shared/read-only-delegation.md) and verify
     decision-critical anchors directly.
   - Load supporting [Research](research.md) when an external contract or deep
     baseline question can change behavior; leave [Solution
     Discovery](research-branches.md#solution-discovery-evidence) to Technical
     Design unless feasibility changes observable behavior.
   - *Done when:* every touched symbol, endpoint, schema, config key, profile,
     and `intent.md` baseline claim is verified at primary locators (or exited
     via [Non-Standard Paths](specification/non-standard-paths.md)).

2. **Pin vocabulary, run Behavioral Probes, and settle every fork.** Adopt
   canonical nouns from `Domain Vocabulary`, code, `api/`, and `docs/decisions/`.
   Pin new or overloaded terms (`cancel` vs `delete`, `accepted` vs `completed`)
   before deciding rules; flag cross-task terms or changed service-wide promises
   for `Durable contract update`.
   - When an open external-contract decision matches `rust-api-contract`,
     `rust-security`, `rust-config`, `rust-reliability`, `rust-observability`, or
     `rust-performance`, load only that skill and extract strictly
     external-observer constraints.
   - Load and walk every applicable probe in [Behavioral
     Probes](specification/behavioral-probes.md) on each touched entity and
     surface, then settle every `intent.md` item and surfaced fork in order:

   | Source | Action |
   | --- | --- |
   | Primary authority (`api/`, code, `docs/decisions/`, RFC MUST, `Production Contract`) | Adopt and cite locator. |
   | Delegated by [Intent](../interfaces/intent.md) or [Decision Ownership](../../../AGENTS.md#decision-ownership) | Decide within limits (or state an explicit **permitted invariant**) with steelman alternative, reason, and objective `Reopen if`. |
   | Downstream-only (non-behavioral preference or internal refactor goal) | Record as `Downstream handoff` (`Technical Design` or `Planning`). |
   | Bounded assumption *(two-way door)* | Use only when `intent.md` + repo convention/RFC SHOULD strongly select one cheaply reversible choice altering no policy, compatibility, security/privacy boundary, or external commitment; add an objective `Reopen if` *(e.g. matching sibling RFC 9457 error shape)*. |
   | Requester- or policy-owned fork *(one-way door)* | Unsettled choice altering user semantics, breaking compatibility, persisted-data fate, security/privacy exposure, or money/SLA/external effects *(e.g. cancelling a completed job deletes artifact vs `409`)* → [Blocked Frontier](specification/non-standard-paths.md#blocked-frontier). |

   - *Done when:* every applicable probe and `intent.md` item maps to `R<n>`,
     `G<n>`, `Non-goals`, `Downstream handoff`, or `Blocked decisions`.

3. **Render draft `spec.md`.** Write `specs/<task>/spec.md` at `status: draft`
   per [Specification V1](../interfaces/spec-v1.md).
   - *Done when:* `spec.md` conforms to Specification V1 and encodes only step
     2's closed decisions.

4. **Run Author Pre-Flight.** Check `draft` `spec.md` and repair in place:
   - **Intent trace:** every requirement, delegated limit, `Success signal`, and
     `Open question` in `intent.md` maps to a `spec.md` anchor.
   - **Scenario walkthrough:** execute the 1–2 hardest multi-rule or unhappy
     paths using only the written `R<n>`/`G<n>` word for word; close any gap.
   - **Deletion test:** for each inferred clause, ask *"If Technical Design met
     the external outcome another way without this clause, what accepted
     guarantee breaks?"* Delete or relax to a permitted invariant when none
     breaks.
   - **Falsifier check:** every `R<n>` `Falsifier` meets [Specification
     V1](../interfaces/spec-v1.md#field-rules).
   - *Done when:* zero untraced intent lines, walkthrough gaps, mechanism leaks,
     or weak `Falsifier`s remain (new one-way forks exit via [Blocked
     Frontier](specification/non-standard-paths.md#blocked-frontier)).

5. **Dispatch and synthesize independent review.** Submit `draft` `spec.md`
   under [Review](../shared/review.md) to fresh `reviewer-agent` lane(s) running
   [Specification Review](specification-review.md) with a fresh context window
   (never forking parent history):
   - **Default (1 reviewer):** local or single-surface spec (all six checks).
   - **Two-axis parallel (2 reviewers):** multi-surface spec, breaking/external
     compatibility change, or authorization, money, data integrity, or
     concurrent state transitions—dispatch **Axis 1 (Truth & Scope)** and **Axis
     2 (Precision & Purity)** in parallel; keep reports distinct and require
     every dispatched axis to clear on the same candidate.
   - **On `FAIL`:** verify each finding's citation and witness against primary
     sources first.
     - *Refuted finding:* never self-approve past `FAIL`; present the refuting
       primary anchors in one bounded delta recheck (or replacement reviewer
       under [Review](../shared/review.md#lifecycle)), routing any surviving
       evidence-backed or inter-axis conflict to `adjudicator-agent` under
       [Review](../shared/review.md#route) (confirmed defects still require
       repair and recheck).
     - *Confirmed `PHASE_DEFECT`:* repair in place, rerun affected step 4
       checks, and re-run only failed/invalidated axes under
       [Review](../shared/review.md#lifecycle).
     - *Confirmed `UPSTREAM_GAP`:* repair independent local defects first, then
       exit via [Wrong Premise In
       Intent](specification/non-standard-paths.md#wrong-premise-in-intent)
       (false premise) or [Blocked
       Frontier](specification/non-standard-paths.md#blocked-frontier)
       (undelegated fork) without another review.
   - *Done when:* every dispatched axis returns `PASS` or `CONCERNS` on the
     current candidate (with any surviving disputed finding resolved by
     `adjudicator-agent`), or a `blocked` handoff returns.

6. **Finalize and hand off.** Copy any bounded `CONCERNS` into `Carried
   concerns`, flip `status: ready` in `spec.md`, and return [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`,
   `owner: Definition`, `result: specs/<task>/spec.md`) with `next_owner:
   Technical Design` whenever [Phase
   Selection](../../spec-first-workflow.md#phase-selection) triggers Technical
   Design or a `Downstream handoff` or `Carried concerns` item names `Technical
   Design` (otherwise `Planning`). Stop here.
