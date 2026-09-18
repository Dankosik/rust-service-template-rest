# Rust Code / Ownership Design

Use when crate, module, or file ownership is not mechanically forced by the
crate graph, [Component Boundaries](../../architecture/boundaries.md), or
[Project Structure](../../project-structure-and-module-organization.md). Own
the exact responsibility map and inverse file map without writing
implementation.

Apply `rust-coder`'s earliest-owner rule and `rust-structural-quality`'s
deletion test to the ready behavior/design and current code, callers, the
composition root, the generated contract, tests, and replacement paths. Return [Ownership Map V1](../interfaces/ownership-map-v1.md) without
function-body or statement-level design.

Change cost: For a new abstraction or responsibility split, apply
`rust-structural-quality` to the proposed ownership map.

Refinement: When implementation evidence improves placement, update the
affected ownership-map entries and recheck only invalidated decisions
and proof; preserve unaffected boundaries.

## Review

Use root self-review for unambiguous placement. Load [Rust Ownership
Review](../rubrics/rust-ownership-review.md) only when its conditional trigger is
present. A broader shared [Review](../shared/review.md) routes through Technical
Design Review and consumes any current panel receipts.

Done when every responsibility has one owner, every changed file one present
reason, the crate graph keeps feature crates free of transport and provider
dependencies, and Planning can preserve placement without
choosing ownership, dependency/composition, generated authority, lifecycle,
cleanup, proof location, or exported surface. Reopen System Design when
placement cannot preserve mechanism/truth; reopen Specification when it cannot
preserve observable behavior.
