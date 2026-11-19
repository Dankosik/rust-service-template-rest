# Specification

Use when structured work lacks a reviewed behavioral contract. Own observable
behavior: what external observers (users, callers, operators, downstream
systems) can rely on.

**Observability boundary:**
- **Pin here:** externally observable rules, invariants, trust/redaction and
  failure/compatibility contracts, operator signals, and exact identifiers
  already fixed by `intent.md`, `api/`, existing code/config, or an external
  standard (plus new client wire identifiers external callers require fixed
  here).
- **Mark `Open to Technical Design:`** permitted behavioral variations and
  unfixed new identifier spellings (config keys, metric/span names,
  table/column names, internal/unfixed error slugs) on the owning `R<n>`.
- **Leave to downstream phases:** internal mechanisms and seams (`Technical
  Design`), task slicing (`Planning`), and test authoring (`Implementation`).

## Steps

1. **Ground in primary sources (static by default — no builds, tests, or
   services).**
   - If `specs/<task>/spec.md` already exists, load [Reopen Or Resume From
     Blocked](specification/non-standard-paths.md#reopen-or-resume-from-blocked).
   - Read `specs/<task>/intent.md` (require `status: ready`; return `status:
     blocked`, `reopen_owner: Intake` without creating a stub `spec.md` if it is
     still an unconfirmed `draft`), `specs/<task>/workflow-plan.md` (when
     present), prior `research/*.md`, [Domain
     Vocabulary](../../repo-architecture.md#domain-vocabulary), and primary
     sources: `api/`, code, migrations, config, `template/profiles.json`,
     tests, `docs/decisions/`, and [Production
     Contract](../../production-contract.md) when service-wide promises apply.
   - Trace live callers, consumers, trust boundaries, and persisted/in-flight
     state across the **blast radius** (HTTP/gRPC, workers/jobs/outbox/webhooks,
     CLI/config boot, readiness/diagnostics, optional profiles).
   - **Smart-zone delegation:** offload wide multi-crate sweeps to a fresh
     `evidence-agent` ([Read-Only
     Delegation](../shared/read-only-delegation.md)), verifying decision-critical
     anchors directly.
   - **Supporting research:** load [Research](research.md) (here or in step 2)
     when an external contract or deep baseline question can change behavior;
     leave [Solution Discovery](research-branches.md#solution-discovery-evidence)
     to Technical Design unless feasibility changes observable behavior.
   - **Early exits & mode switch:** on a contradicted `intent.md` premise, take
     [Wrong Premise In
     Intent](specification/non-standard-paths.md#wrong-premise-in-intent); when
     zero functional, operational, quantitative, or technical work remains, take
     [Already-Satisfied
     No-Delta](specification/non-standard-paths.md#already-satisfied-no-delta);
     when functional behavior stays unchanged while an operational,
     quantitative, or internal technical goal remains, load [Behavior-Preserving
     Change](specification/non-standard-paths.md#behavior-preserving-change).
   - *Done when:* touched symbols, endpoints, schemas, config keys, profiles,
     `docs/decisions/` constraints, and `intent.md` baseline claims are verified
     at primary locators (or an early exit fired).

2. **Pin vocabulary, walk all Behavioral Probes, and settle every fork.**
   - Adopt canonical nouns from `Domain Vocabulary`, code, `api/`, and
     `docs/decisions/`. Pin new or overloaded terms with `not <confused sense>`
     before deciding rules. Honor rejected choices in `docs/decisions/` unless
     their reopen condition is met or `intent.md` reopens them; flag cross-task
     terms, changed service-wide promises, or reopened `docs/decisions/` records
     for `Durable contract update`.
   - **External-contract skills (conditional):** when an open external-contract
     fork is unresolved by `api/`, code, or probes and matches
     `rust-api-contract`, `rust-security`, `rust-config`, `rust-reliability`,
     `rust-observability`, or `rust-performance`, load only that skill and
     extract external-observer rules (statuses, trust/redaction, boot/degrade,
     budgets, signals)—leaving crate wiring, types, generators, and tests to
     downstream phases.
   - Load [Behavioral Probes](specification/behavioral-probes.md) and account
     for **all six probes (#1–#6)** on every touched entity and surface—either
     closing a rule/guarantee/fork or verifying the primary-source fact making
     the probe vacuously closed (never silently skipping a probe). Route any
     unresolved behavioral authority conflict in `research/*.md` to [Blocked
     Frontier](specification/non-standard-paths.md#blocked-frontier) or its
     evidence owner ([Conflict Or
     Freshness](research-branches.md#conflict-or-freshness)), and carry bounded
     unknowns and `valid as of` refresh triggers into `Reopen if`.
   - Settle every `intent.md` item (settled interview decisions `I<n>` including
     their `Binding` strength, `Rejected` exclusions, and `Why` rationale,
     `Concrete scenarios`, hard constraints and `Must not` prohibitions,
     `Suggested mechanisms` translated to observable invariants unless
     mandated, `Priority / tie-breakers`, delegated limits, assumptions `A<n>`,
     `Terminology`, and `Open questions`) and surfaced fork in order:

     | Source | Action |
     | --- | --- |
     | Primary authority (`api/`, code, `docs/decisions/`, RFC MUST, `Production Contract`) | Adopt and cite primary locator. |
     | Permitted invariant suffices *(observers need only the bound, not the exact mechanism or new spelling)* | State the observable invariant and mark `Open to Technical Design:` on the owning `R<n>`. |
     | Delegated by [Intent](../interfaces/intent.md) or [Decision Ownership](../../../AGENTS.md#decision-ownership) | Decide within limits with steelman alternative, reason, and objective `Reopen if`. |
     | Downstream-only (non-behavioral preference or internal technical goal) | Record as `Downstream handoff` (`Technical Design` or `Planning`). |
     | Bounded assumption *(two-way door)* | Cheaply reversible behavioral choice strongly backed by repo convention or RFC SHOULD leaving callers, persisted-data fate, trust/privacy, and external commitments unchanged; add objective `Reopen if` *(e.g. default page limit = 50 matching sibling endpoints)*. |
     | Requester- or policy-owned fork *(one-way door)* | Unsettled choice altering user semantics, compatibility, persisted-data fate, authz/privacy exposure, or money/SLA/external effects *(e.g. cancelling a completed job deletes artifact vs `409`)* → [Blocked Frontier](specification/non-standard-paths.md#blocked-frontier). |

   - *Done when:* all six probes on every touched entity/surface, every
     `intent.md` item, and every `research/*.md` bounded unknown, conflict, or
     refresh trigger map to `R<n>`, `G<n>`, `Non-goals`, `Downstream handoff`,
     `Reopen if`, or `Blocked decisions`.

3. **Render draft `spec.md`.** Write `specs/<task>/spec.md` at `status: draft`
   per [Specification V1](../interfaces/spec-v1.md).
   - *Done when:* `spec.md` conforms to Specification V1 and encodes only step
     2's closed decisions.

4. **Run Author Pre-Flight (static, on-disk `draft` only).** Repair gaps in
   place:
   - **Intent & research trace:** every `intent.md` settled decision (`I<n>`
     respecting `Binding: hard | default` and without reviving any `Rejected`
     option), `Concrete scenarios`, hard constraint, `Must not` prohibition,
     assumption (`A<n>`), delegated limit, `Success signal`, `Open question`,
     and live `research/*.md` unknown/refresh trigger maps to a `spec.md`
     anchor.
   - **Adversarial dry-run:** act as a literal, hostile implementer on the 1–2
     hardest multi-rule/unhappy paths and check every table cell is total—try to
     break `intent.md`'s `Success signal` (**malicious compliance**) or produce
     two externally different outcomes (**two-implementations divergence**)
     while obeying every `R<n>`/`G<n>` verbatim; close every loophole.
   - **Deletion & identifier test:** for each inferred clause, ask *"If
     Technical Design met the external outcome another way without this clause,
     what accepted guarantee breaks?"* Delete or relax to `Open to Technical
     Design:` when none breaks, and verify no unfixed identifier spelling (see
     Observability boundary) was pinned as fixed.
   - **Falsifier check:** every `R<n>` `Falsifier` meets [Specification
     V1](../interfaces/spec-v1.md#field-rules).
   - *Done when:* zero untraced items, adversarial loopholes, table gaps,
     mechanism/identifier leaks, or weak `Falsifier`s remain (new one-way forks
     exit via [Blocked Frontier](specification/non-standard-paths.md#blocked-frontier)).

5. **Dispatch and synthesize independent review.** Submit `draft` `spec.md`
   under [Review](../shared/review.md) to fresh `reviewer-agent` lane(s) running
   [Specification Review](specification-review.md) with a fresh context window
   (never forking parent history):
   - **Brief hygiene:** on initial or fresh reviews, pass strictly the candidate
     path (`specs/<task>/spec.md`), input paths matching the assigned axis
     (`intent.md`, plus `research/*.md` for Axis 1 or single-reviewer), assigned
     axis/checks, and method path (`specification-review.md`)—omit author
     summaries, rationale, or "already verified" claims that break cold-start
     blindness or anchor the reviewer. Bounded delta rechecks add only the
     anchored findings and repair delta or refuting primary locators.
   - **Two-axis parallel (2 reviewers):** use for any multi-surface spec,
     breaking/external compatibility change, or authorization, money, data
     integrity, or concurrent state transitions—dispatch **Axis 1 (Grounding,
     Fidelity & Codebase Reality)** and **Axis 2 (Cold-Start, Contract
     Tightness & Purity)** in parallel; keep reports distinct and require both
     axes to clear on the same candidate.
   - **Default (1 reviewer):** all other specs (none of the two-axis triggers
     above; all six checks).
   - **On `FAIL`:** verify citations/witnesses and handle refuted findings under
     [Review Lifecycle](../shared/review.md#lifecycle).
     - *Confirmed `PHASE_DEFECT`:* repair in place, rerun affected step 4
       checks, and re-run every failed or invalidated axis (an Axis 2 repair
       altering behavior, baseline claims, or affected surfaces invalidates Axis
       1; an Axis 1 repair altering rules, guarantees, or scope invalidates Axis
       2).
     - *Confirmed `UPSTREAM_GAP`:* repair independent local defects first, then
       exit via [Wrong Premise In
       Intent](specification/non-standard-paths.md#wrong-premise-in-intent) or
       [Blocked Frontier](specification/non-standard-paths.md#blocked-frontier)
       without another review.
   - *Done when:* every dispatched axis returns `PASS` or `CONCERNS` on the
     current candidate (or closes via `adjudicator-agent` under [Review
     Lifecycle](../shared/review.md#lifecycle)), or a `blocked` handoff returns.

6. **Finalize and hand off.** Copy bounded `CONCERNS` into `Carried concerns`,
   flip `status: ready` in `spec.md`, and return [Transition Result
   V1](../shared/transition.md#transition-result-v1) (`status: ready`,
   `owner: Definition`, `result: specs/<task>/spec.md`) with `next_owner:
   Technical Design` whenever [Phase
   Selection](../../spec-first-workflow.md#phase-selection) triggers Technical
   Design, any `Open to Technical Design:` clause exists, or a `Downstream
   handoff` or `Carried concerns` item names `Technical Design` (otherwise
   `Planning`). Stop here.
