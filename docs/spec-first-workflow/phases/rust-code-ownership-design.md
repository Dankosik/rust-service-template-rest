# Rust Code / Ownership Design

Use when crate, module, or file placement is open. Even with fixed files,
use this phase to settle who enforces an invariant, what callers must
guarantee, or which module may depend on another. Leave equivalent private
coding choices and function bodies to Implementation.

Ground placement in the accepted design, crate graph,
[`quality/architecture.json`](../../../quality/architecture.json), and
[Repository Architecture](../../repo-architecture.md). Check current callers,
composition, generated contracts, tests, and replacement paths. For an open
ownership choice, walk an affected caller through each viable option. What
must the caller know or do, and where is the guarantee enforced? For a new
abstraction or responsibility split, apply `rust-structural-quality`'s deletion
test and trace which callers would have to change together if the rule changed.
Record the selected owners and files in [Ownership Map V1](#ownership-map-v1).

Refinement: When implementation evidence improves placement, update the
affected ownership-map entries and recheck only invalidated decisions
and proof; preserve unaffected boundaries.

## Review

Check the ownership map locally for unambiguous placement. Use the
[Rust Ownership Review](#rust-ownership-review) panel when its trigger applies.
Include the map and any current panel receipts in the phase candidate for
[Technical Design Review](technical-design-review.md).

Done when every responsibility has one owner, every changed file one present
reason, the crate graph keeps feature crates free of transport and provider
dependencies, and Planning can preserve placement without
choosing ownership, dependency/composition, generated authority, lifecycle,
cleanup, proof location, or exported surface. Reopen System Design when
placement cannot preserve mechanism/truth; reopen Specification when it cannot
preserve observable behavior.

## Ownership Map V1

Return two reconciled sections. Describe each responsibility, its constraints,
and evidence once in Responsibilities; Files references those entries by
unambiguous name or anchor and supplies file-specific facts (retaining every
required field and verifying bidirectional coverage).

Responsibilities (one row per changed responsibility):

```text
responsibility | affected path | current evidence | semantic owner | exact package/file action | dependency/composition/generated boundary | cleanup | proof owner | reopen condition
```

For a non-mechanical implementation source, also record its selected reuse rung,
authority or version, strongest viable rejected source, parity proof, and
upgrade condition.

Files (one row per added or materially changed Rust file):

```text
path | responsibilities | one present reason to exist | declarations/visibility | call-path role | lifecycle/error ownership | allowed dependencies | forbidden responsibilities
```

Every responsibility appears exactly once and every file maps back to a present
responsibility. Use an evidence-bounded deterministic placement rule only when
implementation-local facts prevent an exact path now.

## Rust Ownership Review

Load this complementary panel only when a real responsibility/placement fork
survives, several crates or ownership boundaries change, generated/manual
containment is non-obvious, the restructure is broad or hard to reverse, or
current evidence/reviewers materially disagree.

Partition one fixed [Ownership Map V1](#ownership-map-v1) into:

1. responsibility and execution-path ownership;
2. crate and module placement, dependency direction, composition, visibility
   (`pub`, `pub(crate)`), and generated/manual containment;
3. file cohesion, naming, declaration grouping, and test placement
   (`#[cfg(test)]` beside the owner, `tests/` for black-box proof).

Each read-only lane returns only its lens result through shared
[Read-Only Delegation](../shared/read-only-delegation.md) and
[Review](../shared/review.md). The root synthesizes compatibility. The panel
passes only when every triggered lens is `PASS` on the same candidate; repair or
reopen findings, then re-run only each materially affected lens in fresh
context.
