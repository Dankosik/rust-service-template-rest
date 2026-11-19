# Intent

`specs/<task>/intent.md` preserves requester meaning and closed interview
decisions across the cold-start boundary into Definition, Technical Design,
Planning, and Review. It is the synthesized decision record after Intake—not a
chat transcript, behavior specification, technical design, proof plan, or task
list.

```markdown
# Intent: <short desired outcome>

status: draft | ready

## Problem

<Who is blocked or affected, what fails or is missing today, and why now; cite primary repository locators that grounded the interview as context anchors.>

## Desired outcome

<Concrete observable change for users, operators, or downstream consumers once complete.>

## Affected actors and surfaces

- <actor or consumer> — <external surface (route, RPC, CLI, config, event, or gate) and primary locator if existing>

## Settled decisions

### I1 — <short decision topic>

- **Binding:** `hard` | `default`
- **Decision:** <exact user choice; preserve user-pinned numbers, status codes, ordering rules, concrete examples, and wording verbatim>
- **Rejected:** <alternative(s) explicitly discussed and ruled out during grilling, with brief reason>
- **Why:** <requester's underlying rationale so downstream phases preserve the spirit on unseen edge cases>

## Concrete scenarios

- **<Scenario name>:** Given <starting state>, when <trigger>, then <observable user/operator outcome> *(omit for trivial tasks; include 1–3 user-confirmed happy and edge/failure examples)*

## Constraints & boundaries

- **Hard constraints & `Must not`:** <non-negotiable limits, exact user-pinned thresholds/codes, and forbidden outcomes>
- **Suggested mechanisms (non-binding context):** <technical mechanisms or hints the user mentioned casually; downstream phases may replace them when another design satisfies the outcome better>
- **Scope & non-goals:** <plausible adjacent work explicitly excluded from this change>

## Preferences & delegated choices

- **Priority / tie-breakers:** <ordered trade-off preferences guiding autonomous downstream decisions>
- **Delegated decisions:** <what downstream agents own deciding, guided by what outcome, within what limits>

## Assumptions & reopen triggers

- **A1 — <short assumption name>:** <reversible, outcome-preserving assumption surfaced at confirmation> — **Reopen if:** <objective condition or owner>

## Terminology

- **<Term>:** <canonical requester meaning>. **Not:** <excluded interpretation>

## Success signal

<How the requester or operator observes after delivery that the real problem is solved; record numeric thresholds only when established by the request or an applicable constraint.>

## Open questions

- <non-blocking external/research question> — **Owner:** `Research` | `<external>` | **Fallback:** <`A<n>` or delegated default> | **Reopen if:** <objective condition>
```

## Field Rules

- **Lifecycle (`status`):** start at `draft` when persisting
  progress across multi-round grilling, handling a reopen, or preparing
  confirmation; flip to `ready` only after the requester confirms shared
  understanding under [Intake](../phases/intake.md) (or explicitly waived
  confirmation with zero surviving high-risk branches or material assumptions).
  Downstream phases consume `intent.md` only at `status: ready`.
- **Draft resumability:** while `status: draft`, you may include a temporary
  `## Remaining frontier` section listing unasked design-tree branches and
  verified grounding locators so compaction or interruption preserves open
  questions; clear and remove it before Gate 1 and `status: ready`.
- **Task-adaptive omission:** omit `Settled decisions` (when no grilling
  decisions were required), `Concrete scenarios`, `Suggested mechanisms`,
  `Preferences & delegated choices`, `Assumptions & reopen triggers`,
  `Terminology`, or `Open questions` when empty rather than writing `N/A`
  boilerplate. Always keep `Problem`, `Desired outcome`, `Affected actors and
  surfaces`, `Constraints & boundaries` (`Hard constraints & Must not` and
  `Scope & non-goals`), and `Success signal`.
- **Addressable decisions (`I1`, `I2`, …), binding strength & verbatim
  fidelity:** give every closed interview branch its own `I<n>` heading so
  `spec.md`, `design.md`, and reviewers can cite `intent.md#i1-...` directly
  without chat archaeology. Mark `Binding: hard` for explicit user mandates or
  pinned external contracts (never weakened without reopening Intake) and
  `Binding: default` for confirmed defaults/preferences downstream phases may
  adjust only when primary code or a hard constraint conflicts (still honoring
  `Rejected`, `Why`, and an objective `Reopen if`). Preserve requester-supplied
  examples, `Concrete scenarios`, exact numbers, ordering rules, error codes,
  and `Must not` prohibitions verbatim; record `Rejected` alternatives and `Why`
  so fresh downstream sessions never resurrect ruled-out options or misread
  flattened prose.
- **De-mechanization and delegated boundaries:** keep casual mechanism mentions
  in `Suggested mechanisms (non-binding context)` unless the requester
  explicitly mandated the mechanism as a hard constraint. For each delegated
  choice, state what the next agent owns deciding, the guiding outcome, and its
  limits. Routine technical choices follow
  [Decision Ownership](../../../AGENTS.md#decision-ownership).
- **Assumptions (`A1`, `A2`, …), `Open questions` & authority:** record every
  material assumption with an objective `Reopen if` trigger and surface it
  during Gate 1 confirmation. At `status: ready`, `Open questions` must hold
  zero unanswered requester-owned questions—every entry must name a
  non-requester `Owner` (`Research` or external source), its active `Fallback`
  (`A<n>` or delegated default), and an objective `Reopen if`. Silence alone
  never delegates a requester-owned decision or grants authority to act.
- **Material change & ID stability:** on any mid-task clarification or reopen,
  flip `status` to `draft` while reconciling in place under
  [Artifacts](../shared/artifacts.md). Keep surviving `I<n>` and `A<n>` IDs
  stable (never renumber)—add new `I<n>` entries, mark overturned entries
  `Superseded by I<m>` (or update in place with explicit note), remove resolved
  open questions, flip back to `ready` once confirmed, and reopen only the
  downstream decisions invalidated by the delta.
