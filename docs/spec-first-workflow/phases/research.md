# Research

Use when current or external evidence can change a named decision. Own evidence
closure and its downstream implications, leaving final behavior and mechanism
choices to the requesting phase owner.

- **Scope before searching:** fix the exact question, affected decision, and
  stop condition for an inline lookup. For delegated or standalone research,
  also pass live alternatives and falsifiers, authoritative-source boundaries,
  applicable versions/freshness, and expected output shape. Label quantities as
  measured, external limit, forecast, accepted target, or assumption.
- **Primary sources outrank summaries:** verify decision-changing claims against
  repository code, `Cargo.toml`/`Cargo.lock`, `api/` contracts, migrations,
  official provider specs, standards, and maintainer source rather than Markdown
  prose, comments, blog summaries, or a task brief's/`intent.md`'s description
  of current behavior. Test the leading implication against counter-evidence and
  the strongest viable alternative, recording unavailable surfaces as unknown.
- **Load only the matching branch:**
  - repository/runtime/consumer divergence → [Current-state
    baseline](research-branches.md#current-state-or-semantic-baseline)
  - unfamiliar or current external behavior → [Current external
    contract](research-branches.md#current-external-contract)
  - live solution family → [Solution discovery](research-branches.md#solution-discovery-evidence)
  - empirical claim → [Empirical claim](research-branches.md#empirical-claim-or-probe)
  - conflict or freshness → [Conflict or freshness](research-branches.md#conflict-or-freshness)
  - later input or proof gap → [Downstream input](research-branches.md#downstream-input-closure)
- **Delegation:** offload read-heavy inventories to a fresh `evidence-agent` via
  [Read-Only Delegation](../shared/read-only-delegation.md), verifying
  decision-critical primary sources directly.
- **Output weight:** return [Lane Result
  V1](../shared/read-only-delegation.md#lane-result-v1) inline for a
  single-phase lookup. When reused downstream or needing refresh, persist
  `specs/<task>/research/<topic>.md` with: **Question & affected decision**,
  **Verified facts** (primary locator, scope/version, `valid as of`),
  **Counter-evidence or conflicts**, **Unknowns & refresh triggers**, and
  **Decision implication** (separating fact, inference, assumption, and
  unknown).
- *Done when:* the requesting owner can decide without repeating the search or
  re-synthesizing sources, with verified primary locators and explicit unknowns.

## Review

For an explicit `research only` boundary, apply [Review](../shared/review.md).
Missing evidence closure is `FAIL`; only a bounded later proof risk may be
`CONCERNS`.
