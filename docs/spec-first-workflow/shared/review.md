# Review

Use fresh independent review at two strategic boundaries—once at the end of
preparation ([Plan Review](../phases/plan-review.md)) and once at assembled
delivery ([Implementation Review](../phases/implementation-review.md))—plus
standalone `research only` syntheses.

## Trigger

- **Preparation boundary:** run one fresh [Plan Review](../phases/plan-review.md)
  before finalizing a ready `tasks.md` (or before closing a zero-delta
  preparation claim).
- **Standalone research:** run one fresh review under [Research](../phases/research.md#review).
- **Implementation delivery (ledgers and Direct Work):** keep unit coding and
  tests moving along the ready frontier; select one review disposition at the
  assembled delivery boundary:
  - **Full review:** required for any persisted ledger (`tasks.md`) with
    behavioral code changes, or for Direct Work when requested by the user,
    when changes touch authorization, money, data integrity, concurrency, or
    hard-to-reverse migration, when same-thread edits alter non-trivial
    production behavior without a pre-existing independent test/contract
    oracle, or when a material correctness question remains uncertain. Use the
    2-lens [Complementary Panel](../phases/implementation-review.md#complementary-panel)
    for multi-task ledgers (`≥2` planned code tasks) or high-risk structural
    changes; use one fresh 4-axis reviewer otherwise.
  - **Structure-only review:** when Full review is untriggered and the diff
    adds a workspace crate, dependency/feature, trait/layer, custom general
    mechanism, config key, or fallback/shim/parallel path; route to
    [Structure-Only Review](../phases/implementation-review.md#structure-only-review).
  - **Direct completion:** when neither trigger holds; finish local validation
    directly and record the checked trigger signals.

## Lifecycle

- Fix one candidate (diff or artifact bundle), authoritative inputs, review
  adapter, and check receipts. Pass only primary artifacts and receipts—omit
  author rationale and self-review claims so framing never anchors the
  reviewer.
- The reviewer stays read-only, attempts to falsify the candidate, reuses valid
  evidence, and returns [Review Result V1](../interfaces/review-result-v1.md).
  Missing optional proof and preferences never block `PASS`; confirmed in-scope
  defects must close.
- **Verify before repairing:** check every finding's anchor and witness against
  primary code before editing or escalating. Never self-approve past `FAIL`:
  present refuting primary anchors in a bounded recheck (or escalate a
  surviving evidence-backed conflict to `adjudicator-agent`).
- **Evaluate proposed remedies:** a repair closes the finding; any suggested
  fix is evidence. Every recheck also checks repair structure against
  [Engineering](../../../AGENTS.md#engineering)—a fix that introduces an
  unjustified crate, dependency, layer, config key, or parallel path still
  fails.
- **Bounded repair rounds:**
  - *Plan Review:* at most **one** bounded delta recheck (when repair addresses
    only anchored findings without reslicing or changing interfaces); if that
    still fails, run one fresh full review, then apply [Parent-Owned
    Recovery](transition.md#parent-owned-recovery) instead of a third cycle.
  - *Implementation Review:* batch confirmed findings per pass and allow at
    most **two** repair rounds for the same defect class, using a fresh
    `reviewer-agent` in [Delta Recheck](../phases/implementation-review.md#delta-recheck)
    mode (escalating to a fresh full review whenever repair expands writable
    scope, interfaces, or risk). If Round 2 still leaves the defect open, stop
    cycling and apply [Parent-Owned
    Recovery](transition.md#parent-owned-recovery).

## Route

| Fixed boundary | Adapter |
| --- | --- |
| Standalone research synthesis | [Research](../phases/research.md#review) |
| Preparation bundle (`spec.md`, optional `design.md`, `tasks.md`, packets) | [Plan Review](../phases/plan-review.md) |
| Assembled implementation delivery (ledger, Direct Work, or standalone review) | [Implementation Review](../phases/implementation-review.md) |

For a surviving material evidence-backed conflict across panel lenses or
refutations, dispatch `adjudicator-agent` for [Lane Result
V1](read-only-delegation.md#lane-result-v1) while keeping synthesis authority in
the parent.
