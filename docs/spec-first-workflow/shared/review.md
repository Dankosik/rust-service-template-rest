# Review

Use fresh independent review at strategic preparation boundaries—[Design
Review](../phases/design-review.md) before Planning on substantive
architecture, and [Plan Review](../phases/plan-review.md) before ready
`tasks.md`—and once at assembled delivery ([Implementation
Review](../phases/implementation-review.md)), plus standalone `research only`
syntheses.

## Trigger

- **Preparation boundaries:**
  - Run one fresh [Design Review](../phases/design-review.md) before Planning
    whenever [Design Review
    Gate](../phases/technical-design.md#design-review-gate) fires.
  - Run one fresh [Plan Review](../phases/plan-review.md) before finalizing a
    ready `tasks.md` (or before closing a zero-delta preparation claim).
- **Standalone research:** run one fresh review under [Research](../phases/research.md#review).
- **Implementation delivery (ledgers and Direct Work):** keep unit coding and
  tests moving along the ready frontier; select one review disposition at the
  assembled delivery boundary:
  - **Full review:** required for any persisted ledger (`tasks.md`) with code
    changes, or whenever a [Direct Work](../../../AGENTS.md#direct-work) Full
    review gate fires. Use the 2-lens [Complementary
    Panel](../phases/implementation-review.md#complementary-panel) for
    multi-task ledgers (`≥2` planned code tasks) or high-risk structural
    changes; use one fresh 4-axis reviewer otherwise.
  - **Structure-only review:** when Full review does not apply and a [Direct
    Work](../../../AGENTS.md#direct-work) Structure review gate fires; route to
    [Structure-Only Review](../phases/implementation-review.md#structure-only-review).
  - **Direct completion:** when neither gate applies; finish local validation
    directly and record the checked trigger signals.

## Lifecycle

- Fix one candidate (diff or artifact bundle), authoritative inputs, review
  adapter, and check receipts. Pass only primary artifacts and receipts—omit
  author rationale and self-review claims so framing never anchors the
  reviewer.
- The reviewer stays read-only, attempts to falsify the candidate, reuses valid
  evidence, and returns [Review Result](#review-result).
  Missing optional proof and preferences never block `PASS`; confirmed in-scope
  defects must close.
- **Verify before repairing:** check every finding's anchor and witness against
  primary code before editing or escalating. Never self-approve past `FAIL`:
  present partial refutations alongside fixes in a fresh `Delta Recheck`, and
  dispatch `adjudicator-agent` with both sides' primary anchors for a complete
  refutation (zero candidate changes) or a surviving evidence-backed conflict
  across panel lenses or recheck.
- **Evaluate proposed remedies:** a repair closes the finding; any suggested
  fix is evidence. Every recheck also checks repair structure against
  [Engineering](../../../AGENTS.md#engineering)—a fix that introduces an
  unjustified crate, dependency, layer, config key, or parallel path still
  fails.
- **Bounded repair rounds:**
  - *Design / Plan / Research Review:* at most **one** bounded delta recheck
    (when repair addresses only anchored findings without reslicing, reopening
    a one-way door, or changing interfaces); if that still fails, run one fresh
    full review, then apply [Parent-Owned
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
| Substantive design bundle (`spec.md`, `design.md`, optional `intent.md` / `research/*.md`) | [Design Review](../phases/design-review.md) |
| Preparation bundle (`spec.md`, optional `design.md`, `tasks.md`, packets) | [Plan Review](../phases/plan-review.md) |
| Assembled implementation delivery (ledger, Direct Work, or standalone review) | [Implementation Review](../phases/implementation-review.md) |

For a surviving material evidence-backed conflict across panel lenses or
refutations, dispatch `adjudicator-agent` for [Lane
Result](read-only-delegation.md#lane-result) while keeping synthesis authority
in the parent.

## Review Result

Every independent review returns:

```text
candidate: <fixed artifact revision, diff, tree, or commit>
verdict: PASS | CONCERNS | FAIL | NEEDS_PARENT | REVIEW_HANDOFF_INVALID
findings: <anchored surviving findings grouped by axis, or none>
evidence_boundary: <primary files, symbols, checks, or mutated branches actually inspected>
reopen_owner: <smallest owner or none>
```

Each surviving finding names its anchor, outcome impact, classification,
smallest owner, and evidence boundary. Missing mandatory proof is never
downgraded to a concern.

| Scope | Classifications | Allowed verdicts |
| --- | --- | --- |
| Design / Plan / Research review | `PHASE_DEFECT` (local repair), `UPSTREAM_GAP` (reopen user or external owner), `CONCERN` (bounded downstream risk with risk, owner, and closing action) | `PASS \| CONCERNS \| FAIL \| REVIEW_HANDOFF_INVALID` (`PASS \| FAIL` only on zero-delta checks) |
| Implementation review | `TASK_DEFECT` (repair owning task), `INTEGRATION_DEFECT` (repair seam across tasks), `UPSTREAM_GAP` (invalid accepted requirement/design in candidate scope), `FOLLOW_UP` (non-blocking) | `PASS \| FAIL \| NEEDS_PARENT \| REVIEW_HANDOFF_INVALID` |

- `FAIL`: witnessed in-scope defect (`PHASE_DEFECT`, `TASK_DEFECT`,
  `INTEGRATION_DEFECT`) or confirmed `UPSTREAM_GAP` invalidating the candidate
  or plan.
- `NEEDS_PARENT`: review cannot close because required proof, authority, or an
  authoritative input outside the candidate diff is missing or unavailable.
- `REVIEW_HANDOFF_INVALID`: broken review handoff (missing artifact/boundary,
  unresolved ref, or empty diff); fix the handoff and redispatch without
  consuming a repair round.
