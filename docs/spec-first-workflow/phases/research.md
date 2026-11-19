# Research

Use when current or external evidence can change a named task decision. Own
evidence closure and its downstream implications, leaving final behavior and
mechanism choices to the requesting phase owner.

For a bounded inline lookup inside an active phase, fix the exact question,
affected decision, and stop condition before searching. For delegated or
standalone research, also pass live alternatives and falsifiers,
authoritative-source boundary, applicable versions and freshness, and required
downstream shape. Label quantities as measured, external limit, forecast,
accepted target, or assumption.

Verify decision-changing claims against **primary sources**—repository code,
`Cargo.toml`/`Cargo.lock`, committed contracts (`api/`), migrations, official
provider contracts, standards, and maintainer source outrank Markdown prose,
comments, blog summaries, and `intent.md`'s description of current behavior.
Test the leading implication against material counter-evidence and the
strongest viable alternative, and record unavailable surfaces as unknown.

Load one matching branch by default:

- repository/runtime/consumer divergence -> [Current-state
  baseline](research-branches.md#current-state-or-semantic-baseline);
- unfamiliar or current external behavior -> [Current external
  contract](research-branches.md#current-external-contract);
- a live solution family -> [Solution discovery](research-branches.md#solution-discovery-evidence);
- an empirical claim -> [Empirical claim](research-branches.md#empirical-claim-or-probe);
- conflict or freshness -> [Conflict or freshness](research-branches.md#conflict-or-freshness);
- a later input/proof gap -> [Downstream input](research-branches.md#downstream-input-closure).

The requesting phase owner chooses inline investigation or a fresh
`evidence-agent` via [Read-Only Delegation](../shared/read-only-delegation.md)
for bounded fact-gathering and read-heavy inventories, verifying
decision-critical primary sources directly. A lane already dispatched as
`evidence-agent` runs the investigation directly and never spawns nested
subagents.

Match output weight to downstream reuse:

- **Single-phase lookup:** return [Lane Result
  V1](../shared/read-only-delegation.md#lane-result-v1) inline.
- **Evidence reused downstream or needing refresh:** persist
  `specs/<task>/research/<topic>.md` through [Artifacts](../shared/artifacts.md)
  with: **Question & affected decision**, **Verified facts** (primary locator,
  scope/version, and `valid as of`), **Counter-evidence or conflicts**,
  **Unknowns & refresh triggers**, and **Decision implication**—keeping fact,
  inference, assumption, and unknown explicitly separated.

## Review

For an accepted `research only` boundary, apply shared
[Review](../shared/review.md). Missing evidence closure is `FAIL`; only a
bounded later proof risk may be `CONCERNS`.

Ready when the named next owner can act without repeating the search or
re-synthesizing sources. Reopen the smallest evidence or decision owner for a
required gap.
